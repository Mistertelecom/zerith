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

const PAUSED_KEY: &str = "memory.paused";
const EXCLUDE_PREFIX: &str = "memory.exclude:";

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

    fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, v, now_ms()],
            )?,
            None => self
                .conn
                .execute("DELETE FROM settings WHERE key = ?1", params![key])?,
        };
        Ok(())
    }

    /// Pauses automatic extraction everywhere. Persisted; explicit
    /// `remember` calls still work.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.set_setting(PAUSED_KEY, paused.then_some("1"))
    }

    pub fn is_paused(&self) -> Result<bool> {
        Ok(self.setting(PAUSED_KEY)?.is_some())
    }

    /// Stops extraction from one session (e.g. a private chat).
    pub fn exclude_session(&self, session_id: &str) -> Result<()> {
        self.set_setting(&format!("{EXCLUDE_PREFIX}{session_id}"), Some("1"))
    }

    pub fn include_session(&self, session_id: &str) -> Result<()> {
        self.set_setting(&format!("{EXCLUDE_PREFIX}{session_id}"), None)
    }

    /// What the runtime checks before calling `extract` + `remember`.
    pub fn is_extraction_allowed(&self, session_id: &str) -> Result<bool> {
        Ok(!self.is_paused()?
            && self
                .setting(&format!("{EXCLUDE_PREFIX}{session_id}"))?
                .is_none())
    }

    /// Writes live facts (all scopes when `None`) as one JSON object per line.
    pub fn export_jsonl(&self, scope: Option<&Scope>, mut w: impl std::io::Write) -> Result<usize> {
        let facts = match scope {
            Some(s) => self.export(s)?,
            None => self.select_where(
                &format!("SELECT {COLUMNS} FROM memory_facts WHERE deleted = 0 ORDER BY id"),
                &[],
            )?,
        };
        for f in &facts {
            serde_json::to_writer(&mut w, f).map_err(std::io::Error::from)?;
            w.write_all(b"\n")?;
        }
        Ok(facts.len())
    }

    /// Reads what `export_jsonl` wrote. Ids are reassigned; scope, kind,
    /// confidence, source and session survive. A fact already present is
    /// deduped, not duplicated. Returns how many lines were applied.
    pub fn import_jsonl(&self, r: impl std::io::BufRead) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for (i, line) in r.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let f: Fact = serde_json::from_str(&line).map_err(|e| MemoryError::Import {
                line: i + 1,
                message: e.to_string(),
            })?;
            let id = self.remember(
                &Scope(f.scope.clone()),
                f.kind,
                &f.text,
                f.confidence,
                f.source.as_deref(),
            )?;
            self.conn.execute(
                "UPDATE memory_facts SET session_id = COALESCE(session_id, ?1) WHERE id = ?2",
                params![f.session_id, id],
            )?;
            n += 1;
        }
        tx.commit()?;
        Ok(n)
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
        if scope.as_str() == "*" {
            return Err(MemoryError::Empty);
        }
        if zerith_security::mentions_secret(text) {
            return Err(MemoryError::Secret);
        }
        let ts = now_ms();
        self.conn.execute(
            "INSERT INTO memory_facts(scope, kind, text, source, confidence, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(scope, text) WHERE deleted = 0
             DO UPDATE SET updated_at = excluded.updated_at",
            params![scope.write_for(kind), kind.as_str(), text, source, confidence.as_str(), ts],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM memory_facts WHERE scope = ?1 AND text = ?2 AND deleted = 0",
            params![scope.write_for(kind), text],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    fn recall(&self, scope: &Scope, query: &str, limit: usize) -> Result<Vec<RecallHit>> {
        let Some(query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let fetch = (limit.saturating_mul(3)).clamp(20, 200);
        let sql = "SELECT f.id, f.scope, f.kind, f.text, f.source, f.session_id, f.confidence,
                          f.created_at, f.updated_at, f.superseded_by, bm25(memory_fts) AS score
                   FROM memory_fts
                   JOIN memory_facts f ON f.id = memory_fts.rowid
                   WHERE memory_fts MATCH ?1 AND (?2 = '*' OR instr('|' || ?2 || '|', '|' || f.scope || '|') > 0) AND f.deleted = 0
                   ORDER BY score LIMIT ?3";
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![query, scope.as_str(), fetch as i64], |r| {
            Ok(RecallHit {
                fact: Self::row_to_fact(r)?,
                score: r.get(10)?,
            })
        })?;
        let now = now_ms();
        let mut hits: Vec<RecallHit> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        // FTS5 `bm25()` returns smaller (more negative) values for better matches.
        // Subtract small workspace and recency bonuses to nudge recent project
        // facts ahead without overriding strongly higher keyword matches.
        for h in &mut hits {
            let ws_bonus =
                if h.fact.scope.starts_with("project:") || h.fact.scope.starts_with("workspace:") {
                    0.35
                } else {
                    0.0
                };
            let age_days = (now - h.fact.updated_at).max(0) as f64 / 86_400_000.0;
            let recency_bonus = 0.25 / (1.0 + age_days / 30.0);
            h.score -= ws_bonus + recency_bonus;
        }
        hits.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn list(&self, scope: &Scope, limit: usize) -> Result<Vec<Fact>> {
        self.select_where(
            &format!(
                "SELECT {COLUMNS} FROM memory_facts
                 WHERE (?1 = '*' OR instr('|' || ?1 || '|', '|' || scope || '|') > 0) AND deleted = 0
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

    fn is_extraction_allowed(&self, session_id: &str) -> Result<bool> {
        SqliteMemory::is_extraction_allowed(self, session_id)
    }

    fn export(&self, scope: &Scope) -> Result<Vec<Fact>> {
        self.select_where(
            &format!(
                "SELECT {COLUMNS} FROM memory_facts WHERE (?1 = '*' OR instr('|' || ?1 || '|', '|' || scope || '|') > 0) AND deleted = 0 ORDER BY id"
            ),
            &[&scope.as_str()],
        )
    }
}

const STOPWORDS: &[&str] = &[
    // PT-BR
    "a", "ao", "aos", "as", "com", "como", "da", "das", "de", "do", "dos", "e", "em", "na", "nas",
    "no", "nos", "o", "os", "ou", "para", "por", "qual", "quando", "que", "se", "sem", "ser",
    "seu", "sua", "um", "uma", "uns", "umas", // EN
    "an", "and", "are", "as", "at", "be", "by", "for", "from", "how", "in", "is", "it", "of", "on",
    "or", "that", "the", "this", "to", "was", "were", "what", "when", "where", "which", "who",
    "will", "with",
];

/// FTS5 has its own grammar: a bare `:` or a quote is a syntax error, and
/// returning `None` for an unusable query is better than surfacing a parse
/// failure to the agent. Common PT/EN stopwords are dropped unless nothing
/// else remains, and surviving terms are OR-ed.
fn fts_query(q: &str) -> Option<String> {
    let raw_words: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if raw_words.is_empty() {
        return None;
    }
    let filtered: Vec<String> = raw_words
        .iter()
        .filter(|w| {
            let low = w.to_ascii_lowercase();
            !STOPWORDS.contains(&low.as_str())
        })
        .map(|w| format!("\"{w}\""))
        .collect();
    if !filtered.is_empty() {
        Some(filtered.join(" OR "))
    } else {
        Some(
            raw_words
                .into_iter()
                .map(|w| format!("\"{w}\""))
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    }
}

#[cfg(test)]
mod fts_tests {
    use super::*;

    #[test]
    fn fts_query_drops_pt_en_stopwords_unless_all_are_stopwords() {
        assert_eq!(
            fts_query("como configurar o BGP no roteador").as_deref(),
            Some("\"configurar\" OR \"BGP\" OR \"roteador\"")
        );
        assert_eq!(
            fts_query("the with for").as_deref(),
            Some("\"the\" OR \"with\" OR \"for\"")
        );
        assert_eq!(fts_query("... :::"), None);
    }

    #[test]
    fn recall_boosts_recent_project_scope_over_older_user_scope() {
        let mem = SqliteMemory::open_in_memory().unwrap();
        let old_id = mem
            .remember(
                &Scope::user("local"),
                FactKind::Fact,
                "vlan 100 trunk gateway",
                Confidence::Confirmed,
                None,
            )
            .unwrap();
        mem.conn
            .execute(
                "UPDATE memory_facts SET updated_at = ?1 WHERE id = ?2",
                params![now_ms() - 90 * 86_400_000, old_id],
            )
            .unwrap();
        mem.remember(
            &Scope::project("rede"),
            FactKind::Fact,
            "vlan 100 trunk gateway",
            Confidence::Confirmed,
            None,
        )
        .unwrap();

        let hits = mem.recall(&Scope::all(), "vlan trunk", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].fact.scope, "project:rede");
        assert_eq!(hits[1].fact.scope, "user:local");
    }
}
