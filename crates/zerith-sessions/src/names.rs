//! Finding a session by what the user typed, and renaming it.
//!
//! `--resume <id|name>` and `session rename` need one rule for "which session
//! did you mean". Order: exact id, exact title (case-insensitive), unique id
//! prefix of at least [`MIN_PREFIX`] characters. An ambiguous title is an
//! error that lists the candidates, never a silent pick.

use rusqlite::params;

use crate::store::{Result, Store, StoreError};
use crate::types::Session;

pub const MIN_PREFIX: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("nenhuma sessão corresponde a {0:?}")]
    NotFound(String),
    #[error("{key:?} corresponde a mais de uma sessão: {}", .ids.join(", "))]
    Ambiguous { key: String, ids: Vec<String> },
    #[error("{0}")]
    Store(#[from] StoreError),
}

impl Store {
    /// The most recently active session that is not archived.
    pub fn latest_session(&self) -> Result<Option<Session>> {
        Ok(self
            .list_sessions_filtered(&crate::SessionFilter {
                archived: Some(false),
                limit: 1,
                ..Default::default()
            })?
            .into_iter()
            .next())
    }

    /// Resolves what the user typed (see the module docs).
    pub fn resolve_session(&self, key: &str) -> std::result::Result<Session, ResolveError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(ResolveError::NotFound(key.into()));
        }
        if let Some(s) = self.session(key)? {
            return Ok(s);
        }
        let ids = |sql: &str, arg: String| -> Result<Vec<String>> {
            let mut st = self.conn().prepare(sql)?;
            let rows = st.query_map(params![arg], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let mut found = ids(
            "SELECT id FROM sessions WHERE lower(title) = lower(?1) ORDER BY updated_at DESC",
            key.to_string(),
        )?;
        if found.is_empty() && key.len() >= MIN_PREFIX {
            // `%` and `_` in the key are escaped: an id is hex, a stray
            // wildcard must not match everything.
            let like = key
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            found = ids(
                "SELECT id FROM sessions WHERE id LIKE ?1 || '%' ESCAPE '\\' ORDER BY updated_at DESC",
                like,
            )?;
        }
        match found.len() {
            0 => Err(ResolveError::NotFound(key.into())),
            1 => Ok(self
                .session(&found[0])?
                .ok_or_else(|| ResolveError::NotFound(key.into()))?),
            _ => Err(ResolveError::Ambiguous {
                key: key.into(),
                ids: found,
            }),
        }
    }

    /// Sets the title. Filing, not activity: `updated_at` is left alone so a
    /// rename does not reorder the list.
    pub fn rename_session(&self, id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "o título não pode ser vazio",
            )));
        }
        let n = self.conn().execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }
}
