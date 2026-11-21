//! Session operations beyond append/read: search, fork, export, archive,
//! model, and a small key/value settings table.

use std::io::Write;

use rusqlite::{params, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};

use crate::migrations::now_ms;
use crate::store::{new_id, row_to_session, Result, Store, StoreError};
use crate::types::{Session, SessionSource};

const SESSION_COLS: &str = "s.id, s.title, s.source, s.source_id, s.source_profile, s.model, s.provider, s.created_at, s.updated_at, s.archived";

/// One full-text hit inside a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageHit {
    pub session_id: String,
    pub message_id: i64,
    /// Excerpt with matches wrapped in `[` `]`.
    pub snippet: String,
}

/// `list_sessions_filtered` input. Every `None` means "do not filter".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFilter {
    pub source: Option<SessionSource>,
    /// Leaves this source out (the sidebar hides `Subagent` children).
    pub exclude_source: Option<SessionSource>,
    pub archived: Option<bool>,
    /// Matches the title (substring) or any message content (FTS).
    pub query: Option<String>,
    /// 0 means unlimited.
    pub limit: usize,
}

/// FTS5 has its own grammar; a stray quote or `:` is a syntax error. Every
/// word is quoted and the words are AND-ed (FTS5's implicit operator).
pub fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl Store {
    /// Messages whose content matches `query`, best match first.
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageHit>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut st = self.conn().prepare(
            "SELECT m.session_id, m.id, snippet(messages_fts, 0, '[', ']', '…', 12)
             FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?1 AND m.superseded = 0 ORDER BY bm25(messages_fts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![q, limit as i64], |r| {
            Ok(MessageHit {
                session_id: r.get(0)?,
                message_id: r.get(1)?,
                snippet: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Sessions whose title or any message matches, most recent first.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<Session>> {
        self.list_sessions_filtered(&SessionFilter {
            query: Some(query.to_string()),
            limit,
            ..SessionFilter::default()
        })
    }

    pub fn list_sessions_filtered(&self, f: &SessionFilter) -> Result<Vec<Session>> {
        let mut sql = format!("SELECT {SESSION_COLS} FROM sessions s WHERE 1=1");
        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(src) = f.source {
            args.push(Box::new(src.as_str()));
            sql.push_str(&format!(" AND s.source = ?{}", args.len()));
        }
        if let Some(src) = f.exclude_source {
            args.push(Box::new(src.as_str()));
            sql.push_str(&format!(" AND s.source != ?{}", args.len()));
        }
        if let Some(a) = f.archived {
            args.push(Box::new(a as i64));
            sql.push_str(&format!(" AND s.archived = ?{}", args.len()));
        }
        if let Some(q) = f.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            // LIKE wildcards in the user's text are matched literally.
            let like = format!(
                "%{}%",
                q.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            args.push(Box::new(like));
            let like_at = args.len();
            match fts_query(q) {
                Some(fts) => {
                    args.push(Box::new(fts));
                    sql.push_str(&format!(
                        " AND (s.title LIKE ?{like_at} ESCAPE '\\' OR s.id IN (
                            SELECT m.session_id FROM messages_fts
                            JOIN messages m ON m.id = messages_fts.rowid
                            WHERE messages_fts MATCH ?{}))",
                        args.len()
                    ));
                }
                None => sql.push_str(&format!(" AND s.title LIKE ?{like_at} ESCAPE '\\'")),
            }
        }
        sql.push_str(" ORDER BY s.updated_at DESC");
        if f.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", f.limit));
        }
        let mut st = self.conn().prepare(&sql)?;
        let refs: Vec<&dyn ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = st.query_map(refs.as_slice(), row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// New native session with a copy of `id`'s messages, up to and including
    /// `upto_message_id` (all when `None`). The original is untouched.
    pub fn fork_session(
        &self,
        id: &str,
        upto_message_id: Option<i64>,
        title: &str,
    ) -> Result<Session> {
        let src = self
            .session(id)?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        let ts = now_ms();
        let fork = Session {
            id: new_id(),
            title: title.to_string(),
            source: SessionSource::Native,
            source_id: None,
            source_profile: None,
            model: src.model,
            provider: src.provider,
            created_at: ts,
            updated_at: ts,
            archived: false,
        };
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO sessions(id, title, source, model, provider, created_at, updated_at, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, (SELECT workspace FROM sessions WHERE id = ?7))",
            params![fork.id, fork.title, fork.source.as_str(), fork.model, fork.provider, ts, id],
        )?;
        tx.execute(
            "INSERT INTO messages(session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw)
             SELECT ?1, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw
             FROM messages WHERE session_id = ?2 AND superseded = 0 AND (?3 IS NULL OR id <= ?3) ORDER BY id",
            params![fork.id, id, upto_message_id],
        )?;
        tx.commit()?;
        Ok(fork)
    }

    /// One JSON object per line: the session first (`"type":"session"`), then
    /// each message (`"type":"message"`).
    pub fn export_session_jsonl(&self, id: &str, mut w: impl Write) -> Result<()> {
        let s = self
            .session(id)?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        let line = |w: &mut dyn Write, kind: &str, v: serde_json::Value| -> Result<()> {
            let mut v = v;
            v["type"] = kind.into();
            serde_json::to_writer(&mut *w, &v).map_err(std::io::Error::from)?;
            w.write_all(b"\n")?;
            Ok(())
        };
        line(
            &mut w,
            "session",
            serde_json::to_value(&s).unwrap_or_default(),
        )?;
        for m in self.messages(id, usize::MAX >> 1, 0)? {
            line(
                &mut w,
                "message",
                serde_json::to_value(&m).unwrap_or_default(),
            )?;
        }
        Ok(())
    }

    /// Human-readable transcript.
    pub fn export_session_markdown(&self, id: &str, mut w: impl Write) -> Result<()> {
        let s = self
            .session(id)?
            .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        writeln!(w, "# {}\n", s.title)?;
        if let Some(m) = &s.model {
            writeln!(w, "_modelo: {m}_\n")?;
        }
        for m in self.messages(id, usize::MAX >> 1, 0)? {
            writeln!(w, "## {}\n\n{}\n", m.role.as_str(), m.content)?;
        }
        Ok(())
    }

    pub fn set_session_model(
        &self,
        id: &str,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<()> {
        self.update_one(
            "UPDATE sessions SET model = ?2, provider = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, model, provider, now_ms()],
            id,
        )
    }

    /// Archiving does not bump `updated_at`: it is filing, not activity.
    pub fn set_archived(&self, id: &str, archived: bool) -> Result<()> {
        self.update_one(
            "UPDATE sessions SET archived = ?2 WHERE id = ?1",
            params![id, archived as i64],
            id,
        )
    }

    /// Permanently deletes a session and its messages/todos/checkpoints/events.
    pub fn delete_session(&self, id: &str) -> Result<()> {
        if self
            .conn()
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?
            == 0
        {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        let _ = self.set_session_pinned(id, false);
        Ok(())
    }

    /// Ordered list of pinned session ids (newest pin first).
    pub fn pinned_sessions(&self) -> Result<Vec<String>> {
        let Some(raw) = self.setting("sessions.pinned")? else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default())
    }

    /// Pins or unpins a session and returns the updated list of pinned session ids.
    pub fn set_session_pinned(&self, id: &str, pinned: bool) -> Result<Vec<String>> {
        let mut list = self.pinned_sessions()?;
        list.retain(|x| x != id);
        if pinned {
            list.insert(0, id.to_string());
        }
        let json = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into());
        self.set_setting("sessions.pinned", &json)?;
        Ok(list)
    }

    fn update_one(&self, sql: &str, p: &[&dyn ToSql], id: &str) -> Result<()> {
        if self.conn().execute(sql, p)? == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }
}
