//! The agent's own checklist: `todos`, one list per session.
//!
//! Not to be confused with `plan.rs` (`agent_tasks`), which schedules whole
//! subagent turns. A todo is a line the model keeps for itself and the UI
//! renders as a live checklist.

use crate::migrations::now_ms;
use crate::store::{Result, Store, StoreError};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    /// Hidden from `todo_list`; the row stays so ids are never reused.
    Deleted,
}

impl TodoStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => TodoStatus::Pending,
            "in_progress" => TodoStatus::InProgress,
            "completed" => TodoStatus::Completed,
            "deleted" => TodoStatus::Deleted,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub id: i64,
    pub session_id: String,
    pub subject: String,
    pub description: String,
    pub status: TodoStatus,
    /// Todos that cannot start until this one is completed.
    pub blocks: Vec<i64>,
    /// Todos that must be completed before this one.
    pub blocked_by: Vec<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Fields `todo_update` may change; `None` / empty leaves the field alone.
#[derive(Debug, Clone, Default)]
pub struct TodoPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub status: Option<TodoStatus>,
    /// Ids this todo now blocks (the reverse link is written too).
    pub add_blocks: Vec<i64>,
    /// Ids that now block this todo (the reverse link is written too).
    pub add_blocked_by: Vec<i64>,
}

const COLUMNS: &str =
    "id, session_id, subject, description, status, blocks, blocked_by, created_at, updated_at";

impl Store {
    /// A fresh session for a subagent. `source_id` links it to its parent (a
    /// session id, or a plan id for plan tasks) and `source_profile` carries
    /// the child's own id, so the (source, source_id, profile) key stays
    /// unique per child. `model` is recorded when known.
    pub fn create_subagent_session(
        &self,
        parent: &str,
        title: &str,
        model: Option<&str>,
    ) -> Result<crate::Session> {
        let id = crate::store::new_id();
        let ts = now_ms();
        self.conn().execute(
            "INSERT INTO sessions(id, title, source, source_id, source_profile, model, created_at, updated_at) VALUES (?1, ?2, 'subagent', ?3, ?1, ?4, ?5, ?5)",
            params![id, title, parent, model, ts],
        )?;
        self.session(&id)?
            .ok_or_else(|| StoreError::NotFound("session".into()))
    }

    /// The subagent sessions spawned by `parent`, oldest first.
    pub fn subagent_sessions(&self, parent: &str) -> Result<Vec<crate::Session>> {
        let mut st = self.conn().prepare(
            "SELECT id, title, source, source_id, source_profile, model, provider, created_at, updated_at, archived FROM sessions WHERE source = 'subagent' AND source_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = st.query_map(params![parent], crate::store::row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn todo_create(&self, session_id: &str, subject: &str, description: &str) -> Result<Todo> {
        let ts = now_ms();
        self.conn().execute(
            "INSERT INTO todos(session_id, subject, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![session_id, subject, description, ts],
        )?;
        self.todo(session_id, self.conn().last_insert_rowid())?
            .ok_or_else(|| StoreError::NotFound("todo".into()))
    }

    /// One todo of this session (a deleted one included).
    pub fn todo(&self, session_id: &str, id: i64) -> Result<Option<Todo>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM todos WHERE session_id = ?1 AND id = ?2"),
                params![session_id, id],
                row_to_todo,
            )
            .optional()?)
    }

    /// The live list, oldest first; deleted todos are left out.
    pub fn todo_list(&self, session_id: &str) -> Result<Vec<Todo>> {
        let mut st = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM todos WHERE session_id = ?1 AND status != 'deleted' ORDER BY id"
        ))?;
        let rows = st.query_map(params![session_id], row_to_todo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Applies `patch` in one transaction. Dependency links are symmetric and
    /// must point at todos of the same session.
    pub fn todo_update(&self, session_id: &str, id: i64, patch: &TodoPatch) -> Result<Todo> {
        let tx = self.write_tx()?;
        let mut t = self
            .todo(session_id, id)?
            .ok_or_else(|| StoreError::NotFound(format!("tarefa {id}")))?;
        if let Some(s) = &patch.subject {
            t.subject = s.clone();
        }
        if let Some(d) = &patch.description {
            t.description = d.clone();
        }
        if let Some(s) = patch.status {
            t.status = s;
        }
        let ts = now_ms();
        for (list, others, forward) in [
            (&patch.add_blocks, &mut t.blocks, true),
            (&patch.add_blocked_by, &mut t.blocked_by, false),
        ] {
            for &other in list {
                if other == id {
                    return Err(StoreError::NotFound(format!(
                        "tarefa {id} não pode depender de si mesma"
                    )));
                }
                let o = self
                    .todo(session_id, other)?
                    .ok_or_else(|| StoreError::NotFound(format!("tarefa {other}")))?;
                if !others.contains(&other) {
                    others.push(other);
                }
                // The mirror side: A blocks B  <=>  B is blocked by A.
                let (col, mut mirror) = if forward {
                    ("blocked_by", o.blocked_by)
                } else {
                    ("blocks", o.blocks)
                };
                if !mirror.contains(&id) {
                    mirror.push(id);
                    tx.execute(
                        &format!("UPDATE todos SET {col} = ?1, updated_at = ?2 WHERE id = ?3"),
                        params![json(&mirror), ts, other],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE todos SET subject = ?1, description = ?2, status = ?3, blocks = ?4, blocked_by = ?5, updated_at = ?6 WHERE id = ?7 AND session_id = ?8",
            params![
                t.subject,
                t.description,
                t.status.as_str(),
                json(&t.blocks),
                json(&t.blocked_by),
                ts,
                id,
                session_id
            ],
        )?;
        tx.commit()?;
        self.todo(session_id, id)?
            .ok_or_else(|| StoreError::NotFound(format!("tarefa {id}")))
    }
}

fn json(v: &[i64]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn row_to_todo(r: &rusqlite::Row) -> rusqlite::Result<Todo> {
    let status: String = r.get(4)?;
    let ids = |i| -> rusqlite::Result<Vec<i64>> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    Ok(Todo {
        id: r.get(0)?,
        session_id: r.get(1)?,
        subject: r.get(2)?,
        description: r.get(3)?,
        status: TodoStatus::parse(&status).unwrap_or(TodoStatus::Pending),
        blocks: ids(5)?,
        blocked_by: ids(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionSource;

    fn store() -> (Store, String) {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_session("t", SessionSource::Native).unwrap().id;
        (s, id)
    }

    #[test]
    fn create_update_list_and_delete() {
        let (s, sid) = store();
        let a = s.todo_create(&sid, "A", "faz a").unwrap();
        let b = s.todo_create(&sid, "B", "").unwrap();
        assert_eq!((a.id < b.id, a.status), (true, TodoStatus::Pending));
        let a = s
            .todo_update(
                &sid,
                a.id,
                &TodoPatch {
                    status: Some(TodoStatus::InProgress),
                    subject: Some("A2".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (a.subject.as_str(), a.status),
            ("A2", TodoStatus::InProgress)
        );
        s.todo_update(
            &sid,
            b.id,
            &TodoPatch {
                status: Some(TodoStatus::Deleted),
                ..Default::default()
            },
        )
        .unwrap();
        let live = s.todo_list(&sid).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].subject, "A2");
    }

    #[test]
    fn links_are_symmetric_and_idempotent() {
        let (s, sid) = store();
        let a = s.todo_create(&sid, "A", "").unwrap();
        let b = s.todo_create(&sid, "B", "").unwrap();
        let patch = TodoPatch {
            add_blocks: vec![b.id],
            ..Default::default()
        };
        s.todo_update(&sid, a.id, &patch).unwrap();
        s.todo_update(&sid, a.id, &patch).unwrap();
        assert_eq!(s.todo(&sid, a.id).unwrap().unwrap().blocks, vec![b.id]);
        assert_eq!(s.todo(&sid, b.id).unwrap().unwrap().blocked_by, vec![a.id]);
    }

    #[test]
    fn lists_are_per_session_and_unknown_ids_error() {
        let (s, sid) = store();
        let other = s.create_session("o", SessionSource::Native).unwrap().id;
        let a = s.todo_create(&sid, "A", "").unwrap();
        assert!(s.todo_list(&other).unwrap().is_empty());
        assert!(s.todo_update(&other, a.id, &TodoPatch::default()).is_err());
        let bad = TodoPatch {
            add_blocked_by: vec![999],
            ..Default::default()
        };
        assert!(s.todo_update(&sid, a.id, &bad).is_err());
        let own = TodoPatch {
            add_blocks: vec![a.id],
            ..Default::default()
        };
        assert!(s.todo_update(&sid, a.id, &own).is_err());
    }
}
