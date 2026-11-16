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

fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = std::env::var_os("HERMES_HOME") {
        v.push(PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        v.push(home.join(".hermes"));
        // Profiles: ~/.hermes/<profile>/state.db
        let root = home.join(".hermes");
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    v.push(e.path());
                }
            }
        }
    }
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

/// `.../.hermes/state.db` is the default profile; `.../.hermes/work/state.db`
/// is the profile named `work`.
fn profile_of(path: &Path) -> String {
    let dir = path.parent().and_then(|p| p.file_name());
    match dir.and_then(|d| d.to_str()) {
        Some(".hermes") | Some("hermes") | None => "default".to_string(),
        Some(other) => other.to_string(),
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

/// Imports every session in `path` into `store`.
///
/// `profile` names the source profile; it is part of the idempotency key so two
/// profiles with the same session id do not collide.
pub fn import(path: &Path, profile: &str, store: &Store) -> Result<ImportReport> {
    let src = open_read_only(path)?;
    let layout = inspect(path)?;
    if !layout.sessions_table || !layout.messages_table {
        return Err(ImportError::Schema(format!(
            "{} lacks sessions/messages",
            path.display()
        )));
    }

    let scols = columns(&src, "sessions")?;
    let mcols = columns(&src, "messages")?;
    let mut report = ImportReport::default();

    let title_col = pick(&scols, &["title"]).unwrap_or("id");
    let model_col = pick(&scols, &["model"]);
    let created_col = pick(&scols, &["started_at", "created_at"]);
    let updated_col = pick(&scols, &["ended_at", "updated_at", "started_at"]);
    let archived_col = pick(&scols, &["archived"]);

    let sql = format!(
        "SELECT id, {title_col}, {model}, {created}, {updated}, {archived} FROM sessions ORDER BY id",
        model = model_col.unwrap_or("NULL"),
        created = created_col.unwrap_or("0"),
        updated = updated_col.unwrap_or("0"),
        archived = archived_col.unwrap_or("0"),
    );
    let mut st = src.prepare(&sql)?;
    let mut rows = st.query([])?;

    while let Some(row) = rows.next()? {
        report.sessions_seen += 1;
        let Ok(source_id) = row.get::<_, String>(0) else {
            report.failed.push("session with an undecodable id".into());
            continue;
        };
        let title: String = row
            .get::<_, Option<String>>(1)?
            .unwrap_or_else(|| source_id.clone());
        let model: Option<String> = row.get(2)?;
        // Hermes stores REAL epoch seconds; Zerith stores milliseconds.
        let created = to_ms(row.get::<_, Option<f64>>(3)?.unwrap_or(0.0));
        let updated = to_ms(row.get::<_, Option<f64>>(4)?.unwrap_or(0.0));
        let archived = row.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0;

        let session = Session {
            id: format!("hermes-{profile}-{source_id}"),
            title,
            source: SessionSource::Hermes,
            source_id: Some(source_id.clone()),
            source_profile: Some(profile.to_string()),
            model,
            provider: None,
            created_at: created,
            updated_at: if updated == 0 { created } else { updated },
            archived,
        };

        let (local_id, created_now) = store.upsert_imported(&session)?;
        let n = import_messages(&src, &mcols, &source_id, &local_id, store)?;
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
