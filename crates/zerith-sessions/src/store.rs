//! The SQLite store. One connection, WAL, foreign keys on.
//!
//! Keep one per process and pass `&Store`. Writes are explicit transactions
//! so a crash never leaves a session with half a turn.

use crate::migrations::{self, now_ms};
use crate::types::{Message, NewMessage, Role, Session, SessionSource};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database and runs migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self> {
        // Several connections open the same file at once (the window opens one
        // per screen). Without a timeout the second one fails at once with
        // "database is locked" instead of waiting its turn.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Switching to WAL needs the write lock and, on a brand new file, can
        // lose a race to another connection doing the same. The busy timeout
        // covers the wait; a retry covers the rare "locked" that a pragma
        // returns without waiting.
        let mut tries = 0;
        loop {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(()) => break,
                Err(e) if is_locked(&e) && tries < 20 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => return Err(e.into()),
            }
        }
        migrations::run(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// A write transaction that takes the write lock up front (`BEGIN
    /// IMMEDIATE`), so it waits on the busy timeout. A deferred one that reads
    /// first and then writes fails at once with SQLITE_BUSY when another
    /// connection committed in between, which parallel subagents (one
    /// connection each) make routine.
    pub(crate) fn write_tx(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
    }

    /// Hands the connection to another crate that shares this schema
    /// (`zerith-memory`). The store stops existing afterwards.
    pub fn into_conn(self) -> Connection {
        self.conn
    }

    pub fn schema_version(&self) -> Result<i32> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn create_session(&self, title: &str, source: SessionSource) -> Result<Session> {
        let id = new_id();
        let ts = now_ms();
        self.conn.execute(
            "INSERT INTO sessions(id, title, source, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, source.as_str(), ts],
        )?;
        Ok(Session {
            id,
            title: title.to_string(),
            source,
            source_id: None,
            source_profile: None,
            model: None,
            provider: None,
            created_at: ts,
            updated_at: ts,
            archived: false,
        })
    }

    /// Inserts a session that came from another system, or returns the id of
    /// the one already imported under the same (source, source_id, profile).
    /// This is what makes an interrupted import resumable and repeatable.
    pub fn upsert_imported(&self, s: &Session) -> Result<(String, bool)> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE source = ?1 AND source_id = ?2 AND source_profile IS ?3",
                params![s.source.as_str(), s.source_id, s.source_profile],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
        self.conn.execute(
            "INSERT INTO sessions(id, title, source, source_id, source_profile, model, provider, created_at, updated_at, archived) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                s.id,
                s.title,
                s.source.as_str(),
                s.source_id,
                s.source_profile,
                s.model,
                s.provider,
                s.created_at,
                s.updated_at,
                s.archived as i64
            ],
        )?;
        Ok((s.id.clone(), true))
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, title, source, source_id, source_profile, model, provider, created_at, updated_at, archived FROM sessions WHERE id = ?1",
                params![id],
                row_to_session,
            )
            .optional()?)
    }

    /// The folder a session works in. `None`: never set (CLI, pre-v8 rows).
    pub fn workspace(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT workspace FROM sessions WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Points a session at a folder. Leaves `updated_at` alone: picking a
    /// folder is not a message, so the session keeps its place in the list.
    pub fn set_workspace(&self, id: &str, dir: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sessions SET workspace = ?1 WHERE id = ?2",
            params![dir, id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// Distinct session folders, most recently used first.
    pub fn recent_workspaces(&self, limit: usize) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT workspace FROM sessions WHERE workspace IS NOT NULL AND workspace != '' GROUP BY workspace ORDER BY MAX(updated_at) DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every session that has a folder, as `(id, folder)`.
    pub fn workspaces(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut st = self
            .conn
            .prepare("SELECT id, workspace FROM sessions WHERE workspace IS NOT NULL")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<Session>> {
        let mut st = self.conn.prepare(
            "SELECT id, title, source, source_id, source_profile, model, provider, created_at, updated_at, archived FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Appends one message in its own transaction and bumps the session clock.
    pub fn append(&self, session_id: &str, m: &NewMessage) -> Result<i64> {
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO messages(session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session_id,
                m.role.as_str(),
                m.content,
                m.tool_name,
                m.tool_calls.as_ref().map(|v| v.to_string()),
                m.reasoning,
                m.token_count,
                now_ms(),
                m.raw.as_ref().map(|v| v.to_string())
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_ms(), session_id],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn messages(&self, session_id: &str, limit: usize, offset: usize) -> Result<Vec<Message>> {
        let mut st = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw FROM messages WHERE session_id = ?1 AND superseded = 0 ORDER BY id LIMIT ?2 OFFSET ?3",
        )?;
        let rows = st.query_map(
            params![session_id, limit as i64, offset as i64],
            row_to_message,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest `limit` messages, oldest first: what a turn replays.
    pub fn recent_messages(&self, session_id: &str, limit: usize) -> Result<Vec<Message>> {
        let mut st = self.conn.prepare(
            "SELECT * FROM (SELECT id, session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw FROM messages WHERE session_id = ?1 AND superseded = 0 ORDER BY id DESC LIMIT ?2) ORDER BY id",
        )?;
        let rows = st.query_map(params![session_id, limit as i64], row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What a turn replays, oldest first: every row from the `window`-th most
    /// recent user message on (tool calls and results ride free), plus the
    /// newest compaction summary when it is older than that, so a summary
    /// never falls out of the window. Synthetic user rows (summaries, hook
    /// and tool context, subagent reports) are not messages and do not count.
    pub fn history_rows(&self, session_id: &str, window: usize) -> Result<Vec<Message>> {
        // CASE, not OR: SQLite may evaluate both sides of an OR, and json_each
        // on a malformed `raw` would fail the whole query.
        let start: i64 = self
            .conn
            .query_row(
                "SELECT id FROM messages WHERE session_id = ?1 AND superseded = 0 AND role = 'user'
                 AND CASE WHEN raw IS NULL OR NOT json_valid(raw) THEN 1 ELSE NOT EXISTS (
                     SELECT 1 FROM json_each(raw) WHERE key IN
                     ('summary', 'hook_context', 'hook_stop', 'tool_context', 'subagents_joined')) END
                 ORDER BY id DESC LIMIT 1 OFFSET ?2",
                params![session_id, window.saturating_sub(1) as i64],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut st = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw FROM messages
             WHERE session_id = ?1 AND superseded = 0 AND (id >= ?2 OR id = (
                 SELECT id FROM messages WHERE session_id = ?1 AND superseded = 0
                 AND CASE WHEN raw IS NULL OR NOT json_valid(raw) THEN 0
                     ELSE json_type(raw, '$.summary.start_id') IS NOT NULL END
                 ORDER BY id DESC LIMIT 1))
             ORDER BY id",
        )?;
        let rows = st.query_map(params![session_id, start], row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message_count(&self, session_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND superseded = 0",
            params![session_id],
            |r| r.get(0),
        )?)
    }

    /// PRAGMA integrity_check, run at startup and after a restore.
    pub fn integrity_ok(&self) -> Result<bool> {
        let v: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(v == "ok")
    }

    /// Consistent copy through SQLite backup API, never a raw file copy of a
    /// live WAL database.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let mut dst = Connection::open(path)?;
        let backup = rusqlite::backup::Backup::new(&self.conn, &mut dst)?;
        backup.run_to_completion(64, std::time::Duration::from_millis(20), None)?;
        Ok(())
    }
}

pub(crate) fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    let src: String = r.get(2)?;
    Ok(Session {
        id: r.get(0)?,
        title: r.get(1)?,
        source: SessionSource::parse(&src).unwrap_or(SessionSource::Native),
        source_id: r.get(3)?,
        source_profile: r.get(4)?,
        model: r.get(5)?,
        provider: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        archived: r.get::<_, i64>(9)? != 0,
    })
}

pub(crate) fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    let role: String = r.get(2)?;
    let tool_calls: Option<String> = r.get(5)?;
    let raw: Option<String> = r.get(9)?;
    Ok(Message {
        id: r.get(0)?,
        session_id: r.get(1)?,
        role: Role::parse(&role).unwrap_or(Role::User),
        content: r.get(3)?,
        tool_name: r.get(4)?,
        tool_calls: tool_calls.and_then(|s| serde_json::from_str(&s).ok()),
        reasoning: r.get(6)?,
        token_count: r.get(7)?,
        created_at: r.get(8)?,
        raw: raw.and_then(|s| serde_json::from_str(&s).ok()),
    })
}

/// Random-enough id without a uuid dependency: two hashes of the clock. Not
/// time-ordered; ordering is the rowid.
pub(crate) fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let a = h.finish();
    format!("{:016x}{:016x}", a, a.rotate_left(17))
}

fn is_locked(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::DatabaseBusy
                || f.code == rusqlite::ErrorCode::DatabaseLocked
    )
}
