//! SQLite FTS5 backend over the database file owned by `zerith-sessions`.
//!
//! Dedupe is enforced by the unique index `memory_facts_dedupe` on live rows:
//! storing the same sentence twice updates the timestamp instead of creating a
//! second memory.

use crate::backend::{
    Confidence, Fact, FactKind, MemoryBackend, MemoryError, RecallHit, Result, Scope,
};
use rusqlite::{params, Connection, OptionalExtension};
use zerith_sessions::migrations::now_ms;

pub struct SqliteMemory {
    conn: Connection,
}

const COLUMNS: &str = "id, scope, kind, text, source, session_id, confidence,                        created_at, updated_at, superseded_by";

impl SqliteMemory {
    /// Takes a connection whose schema `zerith-sessions` already migrated.
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Opens or creates the database, running the shared migrations.
    pub fn open(path: impl AsRef<std::path::Path>) -> std::result::Result<Self, MemoryError> {
        let store = zerith_sessions::Store::open(path)?;
        Ok(Self::new(store.into_conn()))
    }

    pub fn open_in_memory() -> std::result::Result<Self, MemoryError> {
        let store = zerith_sessions::Store::open_in_memory()?;
        Ok(Self::new(store.into_conn()))
    }

    fn row_to_fact(r: &rusqlite::Row) -> rusqlite::Result<Fact> {
        let kind: String = r.get(2)?;
        let confidence: String = r.get(6)?;
        Ok(Fact {
            id: r.get(0)?,
            scope: r.get(1)?,
            kind: FactKind::parse(&kind),
            text: r.get(3)?,
            source: r.get(4)?,
            session_id: r.get(5)?,
            confidence: if confidence == "inferred" {
                Confidence::Inferred
            } else {
                Confidence::Confirmed
            },
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
            superseded_by: r.get(9)?,
        })
    }

    fn select_where(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Fact>> {
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(args, Self::row_to_fact)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

impl MemoryBackend for SqliteMemory {
    fn remember(
        &self,
        scope: &Scope,
        kind: FactKind,
        text: &str,
        confidence: Confidence,
        source: Option<&str>,
    ) -> Result<i64> {
        let text = text.trim();
        if text.is_empty() {
            return Err(MemoryError::Empty);
        }
        let ts = now_ms();
        self.conn.execute(
            "INSERT INTO memory_facts(scope, kind, text, source, confidence, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(scope, text) WHERE deleted = 0
             DO UPDATE SET updated_at = excluded.updated_at",
            params![scope.as_str(), kind.as_str(), text, source, confidence.as_str(), ts],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM memory_facts WHERE scope = ?1 AND text = ?2 AND deleted = 0",
            params![scope.as_str(), text],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    fn recall(&self, scope: &Scope, query: &str, limit: usize) -> Result<Vec<RecallHit>> {
        let Some(query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let sql = "SELECT f.id, f.scope, f.kind, f.text, f.source, f.session_id, f.confidence,
                          f.created_at, f.updated_at, f.superseded_by, bm25(memory_fts) AS score
                   FROM memory_fts
                   JOIN memory_facts f ON f.id = memory_fts.rowid
                   WHERE memory_fts MATCH ?1 AND f.scope = ?2 AND f.deleted = 0
                   ORDER BY score LIMIT ?3";
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![query, scope.as_str(), limit as i64], |r| {
            Ok(RecallHit {
                fact: Self::row_to_fact(r)?,
                score: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn list(&self, scope: &Scope, limit: usize) -> Result<Vec<Fact>> {
        self.select_where(
            &format!(
                "SELECT {COLUMNS} FROM memory_facts
                 WHERE scope = ?1 AND deleted = 0
                 ORDER BY updated_at DESC LIMIT ?2"
            ),
            &[&scope.as_str(), &(limit as i64)],
        )
    }

    fn get(&self, id: i64) -> Result<Fact> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM memory_facts WHERE id = ?1"),
                params![id],
                Self::row_to_fact,
            )
            .optional()?
            .ok_or(MemoryError::NotFound(id))
    }

    fn update(&self, id: i64, text: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE memory_facts SET text = ?1, updated_at = ?2 WHERE id = ?3 AND deleted = 0",
            params![text.trim(), now_ms(), id],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id));
        }
        Ok(())
    }

    fn forget(&self, id: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE memory_facts SET deleted = 1, text = '', updated_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id));
        }
        Ok(())
    }

    fn supersede(&self, old_id: i64, by: i64) -> Result<()> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM memory_facts WHERE id = ?1",
                params![by],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(MemoryError::NotFound(by));
        }
        self.conn.execute(
            "UPDATE memory_facts SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
            params![by, now_ms(), old_id],
        )?;
        Ok(())
    }

    fn export(&self, scope: &Scope) -> Result<Vec<Fact>> {
        self.select_where(
            &format!(
                "SELECT {COLUMNS} FROM memory_facts WHERE scope = ?1 AND deleted = 0 ORDER BY id"
            ),
            &[&scope.as_str()],
        )
    }
}

/// FTS5 has its own grammar: a bare `:` or a quote is a syntax error, and
/// returning `None` for an unusable query is better than surfacing a parse
/// failure to the agent. Every word is quoted and the words are OR-ed so one
/// unknown term does not hide the rest.
fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" OR "))
    }
}
