//! The conversation turn: start, cancel, and answer the two things a turn can
//! block on (an approval and a question).
//!
//! The turn itself runs on its own thread; the window only ever sees
//! `turn:event` payloads, so a slow model never freezes a frame.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};
use zerith_providers::Cancel;
use zerith_runtime::{
    approval_channel, question_channel, Approval, ApprovalSender, QuestionSender,
};

use crate::state::AppState;
use crate::turn::{self, Setup, TurnEvent};
use crate::types::Decision;

/// The handles of one running turn. Dropped when it finishes, which closes the
/// gates and lets a blocked call fail closed.
pub struct Running {
    pub cancel: Cancel,
    pub approvals: ApprovalSender,
    pub questions: QuestionSender,
}

/// Every turn currently running, by session.
#[derive(Default)]
pub struct Turns(Mutex<HashMap<String, Running>>);

impl Turns {
    /// Replaces whatever ran for this session before: one turn per conversation.
    pub fn start(&self, id: &str, run: Running) -> Option<Running> {
        self.0.lock().ok()?.insert(id.to_string(), run)
    }

    pub fn take(&self, id: &str) -> Option<Running> {
        self.0.lock().ok()?.remove(id)
    }

    /// Reads without removing, so an approval can be answered mid-turn.
    pub fn with<R>(&self, id: &str, f: impl FnOnce(&Running) -> R) -> Option<R> {
        let map = self.0.lock().ok()?;
        map.get(id).map(f)
    }
}

#[derive(Deserialize)]
pub struct SendArgs {
    pub session_id: String,
    pub model: String,
    pub prompt: String,
    pub mode: String,
    pub agent: Option<String>,
    pub history: usize,
}

/// Starts a turn. Returns at once: the answer arrives as `turn:event`.
#[tauri::command]
pub fn send_turn(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    turns: tauri::State<'_, Turns>,
    args: SendArgs,
) -> Result<(), String> {
    let prompt = args.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Escreva uma mensagem".into());
    }
    if turns.with(&args.session_id, |_| ()).is_some() {
        return Err("Esta conversa já está respondendo".into());
    }

    let setup = Setup {
        root: state.root().to_path_buf(),
        db: state.db(),
        workspace: turn::session_workspace(&state.db(), &args.session_id),
        mode: turn::parse_mode(&args.mode),
        memory_paused: crate::wire::memory_paused(&state.db()),
        agent: args.agent.clone(),
        history: args.history.max(1),
    };

    let cancel = Cancel::default();
    let (approvals, approval_gate) = approval_channel();
    let (questions, question_gate) = question_channel();
    let session = args.session_id.clone();
    turns.start(
        &session,
        Running {
            cancel: cancel.clone(),
            approvals: approvals.clone(),
            questions: questions.clone(),
        },
    );

    // The event channel is per turn: a stale thread can never write into the
    // next turn's transcript.
    let emit_app = app.clone();
    let emit_session = session.clone();
    let mut emit = move |ev: TurnEvent| {
        // Waiting on the person: call them back if the window is behind.
        match &ev {
            TurnEvent::Approval { tool, .. } => {
                crate::notify::ping(&emit_app, "Zerith precisa da sua aprovação", tool)
            }
            TurnEvent::Question { .. } => crate::notify::ping(
                &emit_app,
                "Zerith tem uma pergunta",
                "Responda na conversa para ele continuar.",
            ),
            _ => {}
        }
        let _ = emit_app.emit("turn:event", (&emit_session, ev));
    };

    std::thread::spawn(move || {
        let result = turn::run(
            &setup,
            &session,
            &args.model,
            &prompt,
            &cancel,
            &approval_gate,
            &question_gate,
            &mut emit,
        );
        // Always tell the window the turn is over, even after a failure: the
        // composer stays disabled otherwise.
        app.state::<Turns>().take(&session);
        let _ = app.emit("turn:done", &session);
        match result {
            Ok(()) => crate::notify::ping(&app, "Zerith respondeu", "A resposta está pronta."),
            Err(e) => crate::notify::ping(&app, "A conversa parou com erro", &e),
        }
    });

    Ok(())
}

/// Stops the turn at the next network read.
#[tauri::command]
pub fn cancel_turn(turns: tauri::State<'_, Turns>, session_id: String) -> Result<(), String> {
    turns
        .with(&session_id, |r| r.cancel.cancel())
        .ok_or_else(|| "Nenhum turno rodando nesta conversa".to_string())
}

/// Answers an approval card. False means the turn is already gone.
#[tauri::command]
pub fn answer_approval(
    turns: tauri::State<'_, Turns>,
    session_id: String,
    id: u64,
    decision: Decision,
) -> Result<bool, String> {
    let approval = match decision {
        Decision::Once => Approval::AllowOnce,
        Decision::Session => Approval::AllowSession,
        Decision::Deny => Approval::Deny,
    };
    turns
        .with(&session_id, |r| r.approvals.answer(id, approval))
        .ok_or_else(|| "Turno encerrado".to_string())
}

/// Answers an `ask_user` card with the labels the person picked.
#[tauri::command]
pub fn answer_question(
    turns: tauri::State<'_, Turns>,
    session_id: String,
    id: u64,
    answers: Vec<String>,
) -> Result<bool, String> {
    turns
        .with(&session_id, |r| r.questions.answer(id, answers))
        .ok_or_else(|| "Turno encerrado".to_string())
}
