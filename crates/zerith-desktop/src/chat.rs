//! The conversation turn: start, cancel, and answer the two things a turn can
//! block on (an approval and a question).
//!
//! The turn itself runs on its own thread; the window only ever sees
//! `turn:event` payloads, so a slow model never freezes a frame.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};
use zerith_providers::Cancel;
use zerith_runtime::{
    approval_channel_with_grants, inbox_channel, question_channel, Approval, ApprovalSender,
    InboxSender, QuestionSender, SharedGrants,
};

use crate::state::AppState;
use crate::turn::{self, Setup, TurnEvent, TurnOutcome};
use crate::types::Decision;

static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);

/// The handles of one running turn. Dropped when it finishes, which closes the
/// gates and lets a blocked call fail closed.
pub struct Running {
    pub turn_id: u64,
    pub cancel: Cancel,
    pub approvals: ApprovalSender,
    pub questions: QuestionSender,
    pub inbox: InboxSender,
}

/// Every turn currently running, by session, plus per-session `AllowSession` grants
/// and active background pre-compaction cancel tokens.
#[derive(Default)]
pub struct Turns {
    running: Mutex<HashMap<String, Running>>,
    grants: Mutex<HashMap<String, SharedGrants>>,
    /// The last turn's arguments per conversation: a turn the app starts on
    /// its own (a monitor's notice) runs with the same model, mode and agent.
    last: Mutex<HashMap<String, SendArgs>>,
    precompacts: Arc<Mutex<HashMap<String, Cancel>>>,
}

fn start_precompact_in(precompacts: &Mutex<HashMap<String, Cancel>>, id: &str) -> Cancel {
    let cancel = Cancel::default();
    if let Ok(mut map) = precompacts.lock() {
        if let Some(prev) = map.insert(id.to_string(), cancel.clone()) {
            prev.cancel();
        }
    }
    cancel
}

impl Turns {
    /// Atomically registers a turn for `id` only if no turn is currently active.
    pub fn try_start(&self, id: &str, run: Running) -> bool {
        let Ok(mut map) = self.running.lock() else {
            return false;
        };
        match map.entry(id.to_string()) {
            Entry::Vacant(slot) => {
                slot.insert(run);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    pub fn take(&self, id: &str) -> Option<Running> {
        self.running.lock().ok()?.remove(id)
    }

    /// Removes the running turn for `id` only if its `turn_id` matches.
    pub fn take_if(&self, id: &str, turn_id: u64) -> Option<Running> {
        let mut map = self.running.lock().ok()?;
        match map.entry(id.to_string()) {
            Entry::Occupied(entry) if entry.get().turn_id == turn_id => Some(entry.remove()),
            _ => None,
        }
    }

    /// Reads without removing, so an approval can be answered mid-turn.
    pub fn with<R>(&self, id: &str, f: impl FnOnce(&Running) -> R) -> Option<R> {
        let map = self.running.lock().ok()?;
        map.get(id).map(f)
    }

    /// Returns the persistent session grants bucket for `id`.
    pub fn grants_for(&self, id: &str) -> SharedGrants {
        let Ok(mut map) = self.grants.lock() else {
            return SharedGrants::default();
        };
        map.entry(id.to_string()).or_default().clone()
    }

    /// Removes stored session-level approval grants for `id` (e.g., on delete).
    pub fn remove_grants(&self, id: &str) {
        if let Ok(mut map) = self.grants.lock() {
            map.remove(id);
        }
    }

    /// Registers a `Cancel` token for `id`'s background pre-compaction,
    /// cancelling any prior token registered for the same session.
    pub fn start_precompact(&self, id: &str) -> Cancel {
        start_precompact_in(&self.precompacts, id)
    }

    /// Cancels and removes any active background pre-compaction token for `id`.
    pub fn cancel_precompact(&self, id: &str) {
        if let Ok(mut map) = self.precompacts.lock() {
            if let Some(prev) = map.remove(id) {
                prev.cancel();
            }
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct SendArgs {
    pub session_id: String,
    pub model: String,
    pub prompt: String,
    pub mode: String,
    pub agent: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub orchestrator: bool,
    #[serde(default)]
    pub executor_model: Option<String>,
    /// `/goal`: `prompt` é o objetivo, e o turno vira o laço de
    /// [`turn::run_goal`] (o mesmo slot, o mesmo botão de parar).
    #[serde(default)]
    pub goal: bool,
    // No `history`: the replay window is the execution profile's
    // (`history_window`), read by the turn. An old window still sending one
    // is fine, serde ignores the field.
}

/// Starts a turn. Returns at once: the answer arrives as `turn:event`.
#[tauri::command]
pub fn send_turn(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    turns: tauri::State<'_, Turns>,
    args: SendArgs,
) -> Result<(), String> {
    if args.prompt.trim().is_empty() {
        return Err("Escreva uma mensagem".into());
    }
    if let Ok(mut last) = turns.last.lock() {
        last.insert(args.session_id.clone(), args.clone());
    }
    start_turn(app, &state, &turns, args)
}

/// A conversation whose turn ended while a monitor kept running: when the
/// monitor has news, the app starts a turn on its own with the notice, as
/// opencode's `synthetic` + `wake` does for a finished background command
/// (MIT) and Claude Code's Monitor describes. Hermes queues the same kind of
/// event as a new turn when the session is idle (MIT). No code copied.
pub fn spawn_monitor_waker(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("monitor-waker".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let turns = app.state::<Turns>();
            for session in zerith_tools::sessions_with_monitor_news() {
                if turns.with(&session, |_| ()).is_some() {
                    continue; // a running turn collects its own notices
                }
                let Some(mut args) = turns.last.lock().ok().and_then(|l| l.get(&session).cloned())
                else {
                    continue; // started outside this window: nobody to resume
                };
                let notices = zerith_tools::take_monitor_notices(&session);
                if notices.is_empty() {
                    continue;
                }
                args.goal = false; // o aviso é um turno comum, não um novo objetivo
                args.prompt = format!(
                    "{}\n[aviso automático do Zerith, não escrito pelo usuário: o monitor seguiu rodando depois da sua última resposta]",
                    notices.join("\n\n")
                );
                let _ = app.emit("turn:wake", (&session, &args.prompt));
                let state = app.state::<AppState>();
                if let Err(e) = start_turn(app.clone(), &state, &turns, args) {
                    eprintln!("zerith: monitor não conseguiu retomar a conversa: {e}");
                }
            }
        });
}

fn start_turn(
    app: AppHandle,
    state: &AppState,
    turns: &Turns,
    args: SendArgs,
) -> Result<(), String> {
    let prompt = args.prompt.trim().to_string();

    let turn_id = NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed);
    let cancel = Cancel::default();
    let (approvals, approval_gate) =
        approval_channel_with_grants(turns.grants_for(&args.session_id));
    let (questions, question_gate) = question_channel();
    let (inbox, inbox_rx) = inbox_channel();
    let session = args.session_id.clone();
    if !turns.try_start(
        &session,
        Running {
            turn_id,
            cancel: cancel.clone(),
            approvals: approvals.clone(),
            questions: questions.clone(),
            inbox,
        },
    ) {
        return Err("Esta conversa já está respondendo".into());
    }
    turns.cancel_precompact(&session);
    let precompacts = Arc::clone(&turns.precompacts);

    let root = state.root().to_path_buf();
    let db = state.db();
    let mcp_cache = state.mcp_cache();
    let mode = turn::parse_mode(&args.mode);

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

    let spawned = std::thread::Builder::new().spawn(move || {
        // Whatever happens below, even a panic, the turn ends: the slot frees
        // (else every later message is refused) and the spinner stops.
        let end_app = app.clone();
        let end_session = session.clone();
        let end = OnDrop(Some(move || {
            if end_app
                .state::<Turns>()
                .take_if(&end_session, turn_id)
                .is_some()
            {
                let _ = end_app.emit("turn:done", &end_session);
            }
        }));
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let setup = Setup {
                root,
                db: db.clone(),
                workspace: turn::session_workspace(&db, &session),
                mode,
                memory_paused: crate::wire::memory_paused(&db),
                agent: args.agent.clone(),
                reasoning_effort: args.reasoning_effort.clone(),
                orchestrator: args.orchestrator,
                executor_model: args.executor_model.clone(),
                mcp_cache: Some(mcp_cache),
                register_precompact: Some(Arc::new({
                    let precompacts = Arc::clone(&precompacts);
                    let sid = session.clone();
                    move || start_precompact_in(&precompacts, &sid)
                })),
            };
            let run = if args.goal { turn::run_goal } else { turn::run };
            run(
                &setup,
                &session,
                &args.model,
                &prompt,
                &cancel,
                &approval_gate,
                &question_gate,
                Some(&inbox_rx),
                &mut emit,
            )
        }));
        // A setup error or a panic reaches the chat before `turn:done`.
        let final_outcome = outcome(result, &mut emit);
        drop(end);
        if let Some((title, body)) = notification_for(&final_outcome) {
            crate::notify::ping(&app, title, &body);
        }
    });
    if let Err(e) = spawned {
        turns.take_if(&args.session_id, turn_id);
        return Err(format!("não foi possível iniciar o turno: {e}"));
    }

    Ok(())
}

/// Runs its closure when dropped, unwinding included.
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// Classifies the final desktop notification (if any) for a [`TurnOutcome`].
fn notification_for(outcome: &TurnOutcome) -> Option<(&'static str, String)> {
    match outcome {
        TurnOutcome::Done => Some(("Zerith respondeu", "A resposta está pronta.".into())),
        TurnOutcome::Failed(e) => Some(("A conversa parou com erro", e.clone())),
        TurnOutcome::Cancelled => None,
    }
}

/// Tells the chat how a turn ended when `turn::run` itself could not: an
/// error before the turn started (no provider, store) or a panic becomes a
/// `Failed` event, so a turn never ends silently. Returns the classified outcome.
fn outcome(
    result: std::thread::Result<Result<TurnOutcome, String>>,
    emit: &mut dyn FnMut(TurnEvent),
) -> TurnOutcome {
    let message = match result {
        Ok(Ok(TurnOutcome::Done)) => return TurnOutcome::Done,
        Ok(Ok(TurnOutcome::Cancelled)) => return TurnOutcome::Cancelled,
        Ok(Ok(TurnOutcome::Failed(e))) => return TurnOutcome::Failed(e),
        Ok(Err(e)) => e,
        Err(p) => {
            let why = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "falha sem mensagem".into());
            format!("erro interno: {why}")
        }
    };
    emit(TurnEvent::Failed {
        message: message.clone(),
    });
    TurnOutcome::Failed(message)
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
        Decision::Project => Approval::AllowProject,
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

/// Sends a steering instruction into an active turn's inbox.
#[tauri::command]
pub fn steer_turn(
    turns: tauri::State<'_, Turns>,
    session_id: String,
    prompt: String,
) -> Result<bool, String> {
    let text = prompt.trim().to_string();
    if text.is_empty() {
        return Err("Escreva uma instrução".into());
    }
    turns
        .with(&session_id, |r| r.inbox.send(text))
        .ok_or_else(|| "Nenhum turno rodando nesta conversa".to_string())
}

/// A resposta de `/btw`, para o cartão "pergunta paralela".
#[derive(serde::Serialize)]
pub struct BtwAnswer {
    pub text: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
}

/// `/btw <pergunta>`: uma chamada sem ferramentas com o prefixo da conversa
/// ([`zerith_runtime::btw`]). Não usa o slot do turno (roda junto de um
/// turno em andamento), não grava nada na conversa e não entra na caixa de
/// entrada do turno.
#[tauri::command]
pub async fn btw_ask(
    state: tauri::State<'_, AppState>,
    session_id: String,
    model: String,
    question: String,
    agent: Option<String>,
) -> Result<BtwAnswer, String> {
    if question.trim().is_empty() {
        return Err("Uso: /btw <pergunta>".into());
    }
    if model.trim().is_empty() {
        return Err("Escolha um modelo antes de perguntar".into());
    }
    let root = state.root().to_path_buf();
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let store = zerith_sessions::Store::open(&db).map_err(|e| e.to_string())?;
        let workspace = turn::session_workspace(&db, &session_id);
        let paths = zerith_core::Paths::with_root(&root);
        let settings = zerith_core::Settings::load(&paths, Some(&workspace)).settings;
        let provider = crate::wire::configured_providers_from(&settings, &root)
            .into_iter()
            .next()
            .ok_or("Nenhum provedor configurado")?;
        let client = zerith_providers::Client::new(provider)
            .map_err(|e| zerith_providers::explain::headline(&e))?;
        let contrib = zerith_plugins::Library::open(&root.join("plugins"))
            .and_then(|l| l.contributions())
            .ok();
        let skills =
            zerith_skills::Library::open(root.join("skills"))
                .ok()
                .map(|l| match &contrib {
                    Some(c) => l.with_extra(c.skill_dirs()),
                    None => l,
                });
        let extra = contrib.as_ref().map(|c| c.agents.as_slice()).unwrap_or(&[]);
        let agent = agent
            .as_ref()
            .and_then(|n| zerith_core::agents::find_with_extra(&paths, Some(&workspace), extra, n));
        // Só para a orientação de ferramentas do prompt de sistema sair igual
        // à do turno (built-ins + criação de skills/agentes); nada roda.
        // ponytail: sem MCP aqui, a orientação não depende dele.
        let mut tools = zerith_tools::Registry::new();
        let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
        for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
            paths: paths.clone(),
            project_dir: Some(workspace.clone()),
            tools: have,
            mode: String::new(),
        }) {
            tools.register(t);
        }
        let a = zerith_runtime::btw::ask(
            &store,
            &client,
            zerith_runtime::TurnContext {
                skills: skills.as_ref(),
                agent: agent.as_ref(),
                settings: Some(&settings),
                project_dir: Some(&workspace),
                ..Default::default()
            },
            Some(&tools),
            settings.profile.limits().history_window,
            &session_id,
            &model,
            &question,
            &Cancel::default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(BtwAnswer {
            text: a.text,
            model: a.model,
            prompt_tokens: a.prompt_tokens,
            completion_tokens: a.completion_tokens,
            cache_read_tokens: a.cache_read_tokens,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(evs: &[TurnEvent]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                TurnEvent::Failed { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// A setup error and a panic both reach the chat as `Failed`, and the end
    /// guard runs even while unwinding; a clean turn adds nothing, and a
    /// cancelled turn stays silent.
    #[test]
    fn a_turn_never_ends_silently() {
        let mut evs = Vec::new();
        let ok = outcome(Ok(Ok(TurnOutcome::Done)), &mut |e| evs.push(e));
        assert_eq!(ok, TurnOutcome::Done);
        assert!(evs.is_empty());
        assert_eq!(
            notification_for(&ok),
            Some(("Zerith respondeu", "A resposta está pronta.".into()))
        );

        let cancelled = outcome(Ok(Ok(TurnOutcome::Cancelled)), &mut |e| evs.push(e));
        assert_eq!(cancelled, TurnOutcome::Cancelled);
        assert!(evs.is_empty());
        assert_eq!(notification_for(&cancelled), None);

        let turn_failed = outcome(Ok(Ok(TurnOutcome::Failed("429".into()))), &mut |e| {
            evs.push(e)
        });
        assert_eq!(turn_failed, TurnOutcome::Failed("429".into()));
        assert!(
            evs.is_empty(),
            "turn::run already emitted TurnEvent::Failed"
        );
        assert_eq!(
            notification_for(&turn_failed),
            Some(("A conversa parou com erro", "429".into()))
        );

        let err = outcome(Ok(Err("Nenhum provedor configurado".into())), &mut |e| {
            evs.push(e)
        });
        assert_eq!(
            err,
            TurnOutcome::Failed("Nenhum provedor configurado".into())
        );
        assert_eq!(failed(&evs), ["Nenhum provedor configurado"]);
        assert_eq!(
            notification_for(&err),
            Some((
                "A conversa parou com erro",
                "Nenhum provedor configurado".into()
            ))
        );

        let ended = std::cell::Cell::new(false);
        let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _end = OnDrop(Some(|| ended.set(true)));
            panic!("ferramenta quebrou");
        }));
        assert!(ended.get(), "the guard ran while unwinding");
        let mut evs = Vec::new();
        let panicked = outcome(caught.map(|()| Ok(TurnOutcome::Done)), &mut |e| evs.push(e));
        assert_eq!(failed(&evs), ["erro interno: ferramenta quebrou"]);
        assert_eq!(
            notification_for(&panicked),
            Some((
                "A conversa parou com erro",
                "erro interno: ferramenta quebrou".into()
            ))
        );

        let caught = std::panic::catch_unwind(|| -> Result<TurnOutcome, String> {
            panic!("{} de {}", 3, 4);
        });
        let mut evs = Vec::new();
        outcome(caught, &mut |e| evs.push(e));
        assert_eq!(failed(&evs), ["erro interno: 3 de 4"]);
    }

    #[test]
    fn remove_grants_and_precompact_tokens_manage_session_lifecycle() {
        let turns = Turns::default();
        turns
            .grants_for("s1")
            .lock()
            .unwrap()
            .insert("write_file".into());
        turns.remove_grants("s1");
        assert!(turns.grants_for("s1").lock().unwrap().is_empty());

        let c1 = turns.start_precompact("s1");
        assert!(!c1.is_cancelled());
        let c2 = turns.start_precompact("s1");
        assert!(c1.is_cancelled(), "new precompact cancels previous token");
        assert!(!c2.is_cancelled());
        turns.cancel_precompact("s1");
        assert!(c2.is_cancelled());
    }

    #[test]
    fn session_grants_persist_between_turns_and_stay_isolated() {
        let turns = Turns::default();
        let (_tx1, _gate1) = approval_channel_with_grants(turns.grants_for("s1"));
        let key = zerith_runtime::approval_grant_key(
            "shell",
            &serde_json::json!({"command": "git status"}),
        );
        {
            let shared = turns.grants_for("s1");
            shared.lock().unwrap().insert(key.clone());
        }
        let s1_turn2 = turns.grants_for("s1");
        let s2_turn1 = turns.grants_for("s2");
        assert!(s1_turn2.lock().unwrap().contains(&key));
        assert!(!s2_turn1.lock().unwrap().contains(&key));
        let chained = zerith_runtime::approval_grant_key(
            "shell",
            &serde_json::json!({"command": "git status && rm -rf x"}),
        );
        assert!(!s1_turn2.lock().unwrap().contains(&chained));
    }

    #[test]
    fn try_start_and_take_if_prevent_toctou_and_stale_clear() {
        let turns = Turns::default();
        let make_run = |turn_id: u64| {
            let (approvals, _) = approval_channel_with_grants(turns.grants_for("s1"));
            let (questions, _) = question_channel();
            let (inbox, _) = inbox_channel();
            Running {
                turn_id,
                cancel: Cancel::default(),
                approvals,
                questions,
                inbox,
            }
        };

        assert!(turns.try_start("s1", make_run(10)));
        assert!(!turns.try_start("s1", make_run(11)));
        assert_eq!(turns.with("s1", |r| r.turn_id), Some(10));

        // Stale turn_id must not clear the active turn.
        assert!(turns.take_if("s1", 999).is_none());
        assert_eq!(turns.with("s1", |r| r.turn_id), Some(10));

        // Matching turn_id frees the slot so a new turn can start.
        assert_eq!(turns.take_if("s1", 10).map(|r| r.turn_id), Some(10));
        assert!(turns.try_start("s1", make_run(12)));
        assert_eq!(turns.with("s1", |r| r.turn_id), Some(12));
    }
}
