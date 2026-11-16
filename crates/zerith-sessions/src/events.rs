//! The durable event log (migration 4). One row per thing that happened in a
//! turn; never per streaming delta. `messages` is still what a turn replays;
//! events are the audit trail and the crash detector.

use crate::migrations::now_ms;
use crate::store::{Result, Store};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: i64,
    pub session_id: String,
    pub turn_id: Option<String>,
    /// Per-session counter, 1-based, gap-free.
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
    pub created_at: i64,
}

/// How a turn ended, from its `turn/end` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    /// `done`, `cancelled`, `error` or `max_steps`.
    pub reason: String,
    /// `user`, `parent` or `hook`; only for `cancelled`.
    pub cause: Option<String>,
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub turn_id: String,
    pub started_at: i64,
    pub start_seq: i64,
    /// `None`: `turn/start` without `turn/end`, i.e. the process died mid-turn.
    pub ended: Option<TurnEnd>,
}

impl TurnSummary {
    pub fn is_open(&self) -> bool {
        self.ended.is_none()
    }
}

impl Store {
    /// Appends one event and returns its per-session `seq`.
    pub fn append_event(
        &self,
        session_id: &str,
        turn_id: Option<&str>,
        kind: &str,
        payload: &Value,
    ) -> Result<i64> {
        let tx = self.write_tx()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO events(session_id, turn_id, seq, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, turn_id, seq, kind, payload.to_string(), now_ms()],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Events with `seq > after_seq`, oldest first.
    pub fn events(&self, session_id: &str, after_seq: i64, limit: usize) -> Result<Vec<Event>> {
        let mut st = self.conn().prepare(
            "SELECT id, session_id, turn_id, seq, kind, payload, created_at FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = st.query_map(params![session_id, after_seq, limit as i64], row_to_event)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every turn of the session, oldest first, with how it ended. A turn whose
    /// `ended` is `None` never closed: the process died (or was killed) in it.
    pub fn turn_summary(&self, session_id: &str) -> Result<Vec<TurnSummary>> {
        let mut st = self.conn().prepare(
            "SELECT id, session_id, turn_id, seq, kind, payload, created_at FROM events WHERE session_id = ?1 AND kind IN ('turn/start', 'turn/end') ORDER BY seq",
        )?;
        let events = st.query_map(params![session_id], row_to_event)?;
        let mut turns: Vec<TurnSummary> = Vec::new();
        for e in events {
            let e = e?;
            let Some(turn_id) = e.turn_id.clone() else {
                continue;
            };
            if e.kind == "turn/start" {
                turns.push(TurnSummary {
                    turn_id,
                    started_at: e.created_at,
                    start_seq: e.seq,
                    ended: None,
                });
            } else if let Some(t) = turns.iter_mut().rev().find(|t| t.turn_id == turn_id) {
                t.ended = Some(TurnEnd {
                    reason: e.payload["reason"].as_str().unwrap_or("error").to_string(),
                    cause: e.payload["cause"].as_str().map(str::to_string),
                    seq: e.seq,
                });
            }
        }
        Ok(turns)
    }

    /// True when the newest compaction bracket is a `compaction/start` with no
    /// `compaction/end` after it: a compaction that never finished.
    pub fn has_open_compaction(&self, session_id: &str) -> Result<bool> {
        let kind: Option<String> = self
            .conn()
            .query_row(
                "SELECT kind FROM events WHERE session_id = ?1 AND kind IN ('compaction/start', 'compaction/end') ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(kind.as_deref() == Some("compaction/start"))
    }
}

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload: String = r.get(5)?;
    Ok(Event {
        id: r.get(0)?,
        session_id: r.get(1)?,
        turn_id: r.get(2)?,
        seq: r.get(3)?,
        kind: r.get(4)?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        created_at: r.get(6)?,
    })
}
