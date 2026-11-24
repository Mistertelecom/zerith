//! The Hermes importer.
//!
//! Schema used (verified against hermes-agent rev 4162ecf1,
//! `hermes_state_common.py` lines 455-600):
//!
//! ```text
//! sessions(id TEXT PK, source, model, system_prompt, parent_session_id,
//!          started_at REAL, ended_at, message_count, tool_call_count,
//!          input_tokens, output_tokens, cache_read_tokens, reasoning_tokens,
//!          cwd, git_branch, title, profile_name, archived, pinned, hidden, ...)
//! messages(id INTEGER PK, session_id, role, content, tool_call_id, tool_calls,
//!          tool_name, timestamp REAL, token_count, finish_reason, reasoning,
//!          reasoning_content, active, compacted, api_content, display_kind,
//!          display_metadata, message_uid, ...)
//! ```
//!
//! Resume: a session already imported is not skipped blindly; messages with a
//! source id above the last one copied are appended. An interrupted run
//! therefore finishes the session it stopped in.
//!
//! Only `id`, `title` and `profile_name` are required from `sessions`; every
//! other column is read when present and skipped when a Hermes version does not
//! have it. That tolerance is the whole point of not pinning one schema.

use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use zerith_sessions::{NewMessage, Role, Session, SessionSource, Store};

use crate::{ImportError, Result};

/// Where a source database was found, and what it looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLayout {
    pub path: PathBuf,
    /// Hermes profile name, taken from the directory the database lives in.
    /// `~/.hermes/state.db` is the "default" profile.
    pub profile: String,
    pub sessions_table: bool,
    pub messages_table: bool,
}

/// Summary of one Hermes session for pre-import selection.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionPreview {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub archived: bool,
}

/// Read-only preview of a Hermes database before importing.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcePreview {
    pub layout: SourceLayout,
    pub sessions: Vec<SessionPreview>,
    pub total_messages: usize,
    pub min_date: Option<i64>,
    pub max_date: Option<i64>,
}

/// Reported per run so the user sees real numbers instead of "done".
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    pub sessions_seen: usize,
    pub sessions_imported: usize,
    pub sessions_skipped: usize,
    /// Already-known sessions that got new messages this run (an interrupted
    /// import finishing, or a conversation that continued in Hermes).
    pub sessions_resumed: usize,
    pub messages_imported: usize,
    pub messages_skipped: usize,
    /// Sessions whose source rows could not be decoded; the run continues.
    pub failed: Vec<String>,
}

/// Looks for `state.db` in the places Hermes actually puts it.
///
/// The spec is explicit that `~/.hermes/state.db` is a **common** path, not a
/// guarantee: profiles live in subdirectories, and some installs use an
/// explicit data directory. Every candidate is checked for the tables it needs
/// before being offered.
pub fn detect() -> Vec<SourceLayout> {
    let mut out = Vec::new();
    for base in candidates() {
        let db = if base.extension().is_some_and(|e| e == "db") {
            base
        } else {
            base.join("state.db")
        };
        if !db.is_file() {
            continue;
        }
        if let Ok(layout) = inspect(&db) {
            out.push(layout);
        }
    }
    out
}

/// Every home of [`crate::hermes_setup::home_candidates`] (so `HERMES_HOME`,
/// `~/.hermes` and `%LOCALAPPDATA%\hermes` are all seen), its
/// `profiles/<nome>/` folders, and the older `<home>/<nome>/` layout. Paths
/// already seen through another candidate are listed once.
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for home in crate::hermes_setup::home_candidates() {
        v.push(home.clone());
        for sub in [home.join("profiles"), home.clone()] {
            if let Ok(entries) = std::fs::read_dir(&sub) {
                let mut dirs: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && p.join("state.db").is_file())
                    .collect();
                dirs.sort();
                v.extend(dirs);
            }
        }
    }
    let mut seen = HashSet::new();
    v.retain(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())));
    v
}

fn inspect(path: &Path) -> Result<SourceLayout> {
    let conn = open_read_only(path)?;
    let sessions_table = has_table(&conn, "sessions")?;
    let messages_table = has_table(&conn, "messages")?;
    Ok(SourceLayout {
        path: path.to_path_buf(),
        profile: profile_of(path),
        sessions_table,
        messages_table,
    })
}

/// `<home>/state.db` is the default profile; `<home>/profiles/work/state.db`
/// (and the older `<home>/work/state.db`) is the profile named `work`.
pub(crate) fn profile_of(path: &Path) -> String {
    let dir = path.parent();
    let name = dir.and_then(|p| p.file_name()).and_then(|d| d.to_str());
    let grand = dir
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|d| d.to_str());
    let is_home = dir.is_some_and(|d| {
        crate::hermes_setup::home_candidates()
            .iter()
            .any(|h| h == d)
    });
    match (name, grand) {
        (Some(n), Some("profiles")) => n.to_string(),
        _ if is_home => "default".to_string(),
        (Some(".hermes") | Some("hermes") | None, _) => "default".to_string(),
        (Some(other), _) => other.to_string(),
    }
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The one place a source database is opened, and it cannot write.
fn open_read_only(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(ImportError::NotFound(path.display().to_string()));
    }
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?)
}

/// Columns present on a table, so a newer or older Hermes is tolerated instead
/// of crashing the import.
fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(1))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn pick<'a>(cols: &[String], wanted: &[&'a str]) -> Option<&'a str> {
    wanted.iter().copied().find(|w| cols.iter().any(|c| c == w))
}

struct SourceSessionRow {
    id: String,
    title: String,
    model: Option<String>,
    created_at: i64,
    updated_at: i64,
    archived: bool,
}

fn session_select_sql(scols: &[String]) -> String {
    let title_col = pick(scols, &["title"]).unwrap_or("id");
    let model_col = pick(scols, &["model"]);
    let created_col = pick(scols, &["started_at", "created_at"]);
    let updated_col = pick(scols, &["ended_at", "updated_at", "started_at"]);
    let archived_col = pick(scols, &["archived"]);

    format!(
        "SELECT id, {title_col}, {model}, {created}, {updated}, {archived} FROM sessions ORDER BY id",
        model = model_col.unwrap_or("NULL"),
        created = created_col.unwrap_or("0"),
        updated = updated_col.unwrap_or("0"),
        archived = archived_col.unwrap_or("0"),
    )
}

fn decode_session_row(row: &rusqlite::Row<'_>) -> Result<Option<SourceSessionRow>> {
    let Ok(id) = row.get::<_, String>(0) else {
        return Ok(None);
    };
    let title: String = row
        .get::<_, Option<String>>(1)?
        .unwrap_or_else(|| id.clone());
    let model: Option<String> = row.get(2)?;
    // Hermes stores REAL epoch seconds; Zerith stores milliseconds.
    let created_at = to_ms(row.get::<_, Option<f64>>(3)?.unwrap_or(0.0));
    let updated = to_ms(row.get::<_, Option<f64>>(4)?.unwrap_or(0.0));
    let updated_at = if updated == 0 { created_at } else { updated };
    let archived = row.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0;
    Ok(Some(SourceSessionRow {
        id,
        title,
        model,
        created_at,
        updated_at,
        archived,
    }))
}

fn open_validated(path: &Path) -> Result<(Connection, SourceLayout)> {
    let src = open_read_only(path)?;
    let layout = inspect(path)?;
    if !layout.sessions_table || !layout.messages_table {
        return Err(ImportError::Schema(format!(
            "{} lacks sessions/messages",
            path.display()
        )));
    }
    Ok((src, layout))
}

/// Inspects a Hermes database read-only, listing sessions, active message
/// counts, and date bounds without writing to the source.
pub fn preview(path: &Path) -> Result<SourcePreview> {
    let (src, layout) = open_validated(path)?;
    let scols = columns(&src, "sessions")?;
    let mcols = columns(&src, "messages")?;

    let mut counts: HashMap<String, usize> = HashMap::new();
    let where_active = match pick(&mcols, &["active"]) {
        Some(c) => format!(" WHERE COALESCE({c}, 1) = 1"),
        None => String::new(),
    };
    let msg_sql =
        format!("SELECT session_id, COUNT(*) FROM messages{where_active} GROUP BY session_id");
    let mut mst = src.prepare(&msg_sql)?;
    let mut mrows = mst.query([])?;
    while let Some(mrow) = mrows.next()? {
        if let Ok(Some(sid)) = mrow.get::<_, Option<String>>(0) {
            let n = mrow.get::<_, i64>(1)?.max(0) as usize;
            counts.insert(sid, n);
        }
    }

    let sql = session_select_sql(&scols);
    let mut st = src.prepare(&sql)?;
    let mut rows = st.query([])?;

    let mut sessions = Vec::new();
    let mut total_messages = 0;
    let mut min_date: Option<i64> = None;
    let mut max_date: Option<i64> = None;

    while let Some(row) = rows.next()? {
        let Some(s) = decode_session_row(row)? else {
            continue;
        };
        let message_count = counts.get(&s.id).copied().unwrap_or(0);
        total_messages += message_count;

        let min_candidate = if s.created_at > 0 {
            Some(s.created_at)
        } else if s.updated_at > 0 {
            Some(s.updated_at)
        } else {
            None
        };
        if let Some(d) = min_candidate {
            min_date = Some(min_date.map_or(d, |cur| cur.min(d)));
        }

        let max_candidate = if s.updated_at > 0 {
            Some(s.updated_at)
        } else if s.created_at > 0 {
            Some(s.created_at)
        } else {
            None
        };
        if let Some(d) = max_candidate {
            max_date = Some(max_date.map_or(d, |cur| cur.max(d)));
        }

        sessions.push(SessionPreview {
            id: s.id,
            title: s.title,
            model: s.model,
            created_at: s.created_at,
            updated_at: s.updated_at,
            message_count,
            archived: s.archived,
        });
    }

    Ok(SourcePreview {
        layout,
        sessions,
        total_messages,
        min_date,
        max_date,
    })
}

/// Imports every session in `path` into `store`.
///
/// `profile` names the source profile; it is part of the idempotency key so two
/// profiles with the same session id do not collide.
pub fn import(path: &Path, profile: &str, store: &Store) -> Result<ImportReport> {
    import_selected(path, profile, store, true, None)
}

/// Imports sessions from `path` into `store`, filtering out archived sessions
/// when `!include_archived` and restricting to `only_ids` when `Some`.
///
/// Sessions excluded by these user filters are counted in `sessions_seen` and
/// skipped before `upsert_imported`, so they do not increment
/// `sessions_imported`, `sessions_skipped`, or `failed`.
pub fn import_selected(
    path: &Path,
    profile: &str,
    store: &Store,
    include_archived: bool,
    only_ids: Option<&[String]>,
) -> Result<ImportReport> {
    let (src, _) = open_validated(path)?;
    let scols = columns(&src, "sessions")?;
    let mcols = columns(&src, "messages")?;
    let mut report = ImportReport::default();
    let wanted: Option<HashSet<&str>> =
        only_ids.map(|ids| ids.iter().map(String::as_str).collect());

    let sql = session_select_sql(&scols);
    let mut st = src.prepare(&sql)?;
    let mut rows = st.query([])?;

    while let Some(row) = rows.next()? {
        report.sessions_seen += 1;
        let Some(s) = decode_session_row(row)? else {
            report.failed.push("session with an undecodable id".into());
            continue;
        };

        if !include_archived && s.archived {
            continue;
        }
        if let Some(ref set) = wanted {
            if !set.contains(s.id.as_str()) {
                continue;
            }
        }

        let session = Session {
            id: format!("hermes-{profile}-{}", s.id),
            title: s.title,
            source: SessionSource::Hermes,
            source_id: Some(s.id.clone()),
            source_profile: Some(profile.to_string()),
            model: s.model,
            provider: None,
            created_at: s.created_at,
            updated_at: s.updated_at,
            archived: s.archived,
        };

        let (local_id, created_now) = store.upsert_imported(&session)?;
        let n = import_messages(&src, &mcols, &s.id, &local_id, store)?;
        report.messages_imported += n;
        tally(&mut report, created_now, n);
    }

    Ok(report)
}

/// New session, resumed session (a previous run stopped mid-way, or Hermes
/// added messages since), or nothing to do.
pub(crate) fn tally(report: &mut ImportReport, created_now: bool, appended: usize) {
    if created_now {
        report.sessions_imported += 1;
    } else if appended > 0 {
        report.sessions_resumed += 1;
    } else {
        report.sessions_skipped += 1;
    }
}

/// Per-message resume cursor: the highest source message id already copied.
/// It is read back from `raw`, which is written in the same transaction as
/// the message, so the cursor can never disagree with the data.
fn cursor(store: &Store, local_session: &str) -> Result<i64> {
    Ok(store.conn().query_row(
        "SELECT COALESCE(MAX(CAST(json_extract(raw, '$.id') AS INTEGER)), 0) FROM messages WHERE session_id = ?1",
        [local_session],
        |r| r.get(0),
    )?)
}

fn import_messages(
    src: &Connection,
    mcols: &[String],
    source_session: &str,
    local_session: &str,
    store: &Store,
) -> Result<usize> {
    let content_col = pick(mcols, &["content"]).or_else(|| pick(mcols, &["api_content"]));
    let active_col = pick(mcols, &["active"]);

    // Inactive rows are Hermes' own tombstones; skipping them keeps the imported
    // conversation equal to what Hermes shows.
    let where_active = match active_col {
        Some(c) => format!(" AND (COALESCE({c}, 1) = 1)"),
        None => String::new(),
    };

    // `SELECT *` on purpose: the row is preserved verbatim in `raw`, so a
    // column this importer does not know about yet is still carried across.
    let sql = format!(
        "SELECT * FROM messages WHERE session_id = ?1 AND id > ?2{where_active} ORDER BY id"
    );
    let after = cursor(store, local_session)?;
    let mut st = src.prepare(&sql)?;
    let mut rows = st.query(rusqlite::params![source_session, after])?;
    let mut n = 0;
    while let Some(row) = rows.next()? {
        let mut raw = serde_json::Map::new();
        for (i, name) in mcols.iter().enumerate() {
            let Ok(value) = row.get::<_, rusqlite::types::Value>(i) else {
                continue;
            };
            raw.insert(name.clone(), sql_value_to_json(&value));
        }

        let text = |name: &str| -> Option<String> {
            raw.get(name).and_then(|v| v.as_str()).map(str::to_string)
        };

        let raw_role = text("role").unwrap_or_else(|| "user".into());
        let content = content_col.and_then(text).unwrap_or_default();
        let msg = NewMessage {
            role: Role::parse(&raw_role).unwrap_or(Role::User),
            content,
            tool_name: text("tool_name"),
            tool_calls: text("tool_calls").and_then(|s| serde_json::from_str(&s).ok()),
            reasoning: text("reasoning").or_else(|| text("reasoning_content")),
            token_count: raw.get("token_count").and_then(|v| v.as_i64()),
            raw: Some(serde_json::Value::Object(raw)),
        };
        store.append(local_session, &msg)?;
        n += 1;
    }
    Ok(n)
}

/// SQLite values to JSON without losing the distinction between NULL and the
/// empty string, which matters for `active` and `compacted`.
fn sql_value_to_json(v: &rusqlite::types::Value) -> serde_json::Value {
    use rusqlite::types::Value;
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Real(f) => serde_json::Value::from(*f),
        Value::Text(s) => serde_json::Value::from(s.clone()),
        Value::Blob(b) => serde_json::Value::from(format!("<{} bytes>", b.len())),
    }
}

/// Hermes stores seconds as REAL; Zerith stores milliseconds as i64.
pub(crate) fn to_ms(secs: f64) -> i64 {
    if secs <= 0.0 {
        return 0;
    }
    (secs * 1000.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_come_from_the_folder_layout() {
        let p = |s: &str| profile_of(Path::new(s));
        assert_eq!(p("/x/.hermes/state.db"), "default");
        assert_eq!(p("/x/AppData/Local/hermes/state.db"), "default");
        assert_eq!(p("/x/hermes/profiles/netops-beta/state.db"), "netops-beta");
        assert_eq!(p("/x/.hermes/work/state.db"), "work");
    }
}
