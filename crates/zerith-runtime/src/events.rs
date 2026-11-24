//! Turn-level audit events and the cancel cause.
//!
//! Everything the model saw or was told is also written to the `events` table
//! (see `zerith_sessions::events`): `turn/start`, `step/start|end`,
//! `tool/call|result`, `assistant/attempt`, `compaction/*`, `user/steer`,
//! `tool/context`, `turn/end`. Streaming deltas are never written.
//!
//! Writing an event is best effort: a full disk must not turn a good answer
//! into a failed turn, and `messages` (the replay source) is written with
//! `?`. A turn whose `turn/end` is missing is the crash signal.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use zerith_providers::Cancel;
use zerith_sessions::Store;

/// Who pulled the cancel token. Recorded in `turn/end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCause {
    /// The person pressed stop. The default when nobody said otherwise.
    User,
    /// A parent agent (plan runner) cancelled its subagent.
    Parent,
    /// A hook or middleware decided the turn must stop.
    Hook,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::User => "user",
            CancelCause::Parent => "parent",
            CancelCause::Hook => "hook",
        }
    }
}

/// Side channel for the cause, because `Cancel` (zerith-providers) is a bare
/// flag. Share one slot (it is `Clone`) between whoever cancels and the turn
/// (`AgentOptions::cancel_cause`). The first cause wins.
#[derive(Clone, Default)]
pub struct CauseSlot(Arc<AtomicU8>);

impl CauseSlot {
    pub fn get(&self) -> Option<CancelCause> {
        match self.0.load(Ordering::SeqCst) {
            1 => Some(CancelCause::User),
            2 => Some(CancelCause::Parent),
            3 => Some(CancelCause::Hook),
            _ => None,
        }
    }
}

/// Cancels the turn and records why. Safe from any thread.
pub fn cancel_with(cancel: &Cancel, slot: &CauseSlot, cause: CancelCause) {
    let code = match cause {
        CancelCause::User => 1,
        CancelCause::Parent => 2,
        CancelCause::Hook => 3,
    };
    let _ = slot
        .0
        .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    cancel.cancel();
}

/// A turn longer than this tells the person it ended (`Notification`,
/// `idle_prompt`): they likely switched away while it ran.
pub(crate) const LONG_TURN: std::time::Duration = std::time::Duration::from_secs(60);

/// The `Notification` an event deserves, as `(notification_type, message)`:
/// the turn is blocked on the person.
pub(crate) fn notification_for(e: &crate::RunEvent) -> Option<(&'static str, String)> {
    let clip = |s: &str| s.chars().take(300).collect::<String>();
    match e {
        crate::RunEvent::ApprovalNeeded { tool, summary, .. } => Some((
            "permission_prompt",
            clip(&format!("Zerith pede aprovação ({tool}): {summary}")),
        )),
        crate::RunEvent::Question { questions, .. } => Some((
            "ask_user",
            clip(&format!(
                "Zerith tem uma pergunta: {}",
                questions.first().map_or("", |q| q.question.as_str())
            )),
        )),
        _ => None,
    }
}

static NEXT_TURN: AtomicU64 = AtomicU64::new(1);

/// Appends events for one turn (or, with `turn_id: None`, outside any turn).
pub(crate) struct TurnLog<'a> {
    store: &'a Store,
    session_id: &'a str,
    turn_id: Option<String>,
}

impl<'a> TurnLog<'a> {
    pub fn new(store: &'a Store, session_id: &'a str) -> Self {
        let id = format!(
            "{:x}-{:x}",
            zerith_sessions::migrations::now_ms(),
            NEXT_TURN.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            store,
            session_id,
            turn_id: Some(id),
        }
    }

    /// For work that is not a turn, such as a manual `/compact`.
    pub fn outside_turn(store: &'a Store, session_id: &'a str) -> Self {
        Self {
            store,
            session_id,
            turn_id: None,
        }
    }

    pub fn emit(&self, kind: &str, payload: Value) {
        let _ = self
            .store
            .append_event(self.session_id, self.turn_id.as_deref(), kind, &payload);
    }

    pub fn end(&self, reason: &str, cause: Option<CancelCause>, error: Option<String>) {
        let mut p = json!({"reason": reason});
        if let Some(c) = cause {
            p["cause"] = c.as_str().into();
        }
        if let Some(e) = error {
            p["error"] = e.into();
        }
        self.emit("turn/end", p);
    }
}
