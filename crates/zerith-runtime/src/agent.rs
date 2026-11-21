//! The tool loop: request → stream → tool calls → pipeline → persist →
//! request again, until the model answers without calling a tool, the user
//! cancels, or `max_steps` runs out.
//!
//! Durable records are written once each: one assistant row per step (with
//! its tool calls as JSON), one tool row per result. Deltas are never written.
//! Alongside the rows, every turn writes an audit trail to the `events` table
//! (see [`crate::events`]); a turn with no `turn/end` is a crash.
//!
//! The pieces live in their own files: [`crate::pipeline`] (what happens to
//! one tool call), [`crate::compact`] (keeping the history inside the context
//! window) and [`crate::spill`] (outputs too big for the model).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_core::hooks::{run_hooks, HookEvent, HookOutcome, HookPayload};
use zerith_providers::{
    is_failover_error, Cancel, ChatMessage, ChatRequest, Client, Gateway, GatewayOutcome,
    ProviderError, RouteCandidate, StreamEvent, ToolCall, ToolDef, Usage,
};
use zerith_security::Policy;
use zerith_sessions::{checkpoints, migrations::now_ms, NewMessage, NewUsageRecord, Role, Store};
use zerith_tools::Registry;

use crate::compact::{replay_items, ClientSummarizer, History, Summarizer};
use crate::events::{CancelCause, CauseSlot, TurnLog};
use crate::pipeline::{Middleware, Pipeline};
use crate::question::{self, QuestionGate};
use crate::spill::{self, SPILL_MAX_AGE};
use crate::subagent::{self, SubagentOptions};
use crate::{Result, RunError, RunEvent, TurnContext, TurnReport, HISTORY_LIMIT};

pub use crate::compact::{estimate_tokens, fit_context, replay, DROPPED};

/// The user's answer to an `ApprovalNeeded` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    AllowOnce,
    /// Allow this tool for as long as the host keeps the same `ApprovalGate`.
    AllowSession,
    Deny,
}

/// The UI's half: answers an `ApprovalNeeded { id, .. }` from any thread.
#[derive(Clone)]
pub struct ApprovalSender(mpsc::Sender<(u64, Approval)>);

impl ApprovalSender {
    /// Returns false when the turn is no longer listening (it ended or was
    /// cancelled); the answer is then moot.
    pub fn answer(&self, id: u64, approval: Approval) -> bool {
        self.0.send((id, approval)).is_ok()
    }
}

/// The turn's half. Keep one per conversation: `AllowSession` grants live
/// here, so a new gate starts asking again. Clones share everything: a
/// subagent holds a clone of its parent's gate, so its prompts reach the same
/// person through the parent's events (see [`ApprovalGate::relay`]).
pub type SharedGrants = Arc<Mutex<HashSet<String>>>;

#[derive(Clone)]
pub struct ApprovalGate {
    rx: Arc<Mutex<mpsc::Receiver<(u64, Approval)>>>,
    granted: SharedGrants,
    /// Answers that arrived for another waiter (parallel children).
    early: Arc<Mutex<HashMap<u64, Approval>>>,
    /// `ApprovalNeeded` events of subagents, for the parent loop to emit.
    relayed: Arc<Mutex<Vec<RunEvent>>>,
}

pub fn approval_channel() -> (ApprovalSender, ApprovalGate) {
    approval_channel_with_grants(Arc::default())
}

/// Like [`approval_channel`], sharing `AllowSession` grants across turns in a conversation.
pub fn approval_channel_with_grants(grants: SharedGrants) -> (ApprovalSender, ApprovalGate) {
    let (tx, rx) = mpsc::channel();
    (
        ApprovalSender(tx),
        ApprovalGate {
            rx: Arc::new(Mutex::new(rx)),
            granted: grants,
            early: Arc::default(),
            relayed: Arc::default(),
        },
    )
}

/// Computes the session grant key for a tool call. Simple `shell` commands get
/// a prefix scope (`shell:prefix:<prog> <subcmd> *` or `shell:prefix:<prog> *`),
/// whereas chained/piped/redirected commands require an exact normalized match
/// so `AllowSession` never widens to arbitrary suffixes.
pub fn approval_grant_key(tool: &str, args: &Value) -> String {
    if tool != "shell" {
        return tool.to_string();
    }
    let Some(raw) = args.get("command").and_then(Value::as_str) else {
        return tool.to_string();
    };
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.is_empty() {
        return tool.to_string();
    }
    let normalized = tokens.join(" ");
    let unsafe_meta = raw
        .chars()
        .any(|c| matches!(c, ';' | '&' | '|' | '`' | '>' | '<' | '\n' | '\r'))
        || raw.contains("$(");
    if unsafe_meta {
        return format!("shell:exact:{normalized}");
    }
    let tok0 = tokens[0];
    if let Some(tok1) = tokens.get(1) {
        let plain_subcmd = !tok1.starts_with('-')
            && !tok1.contains(['/', '\\', '.', '='])
            && tok1
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if plain_subcmd {
            return format!("shell:prefix:{tok0} {tok1} *");
        }
    }
    format!("shell:prefix:{tok0} *")
}

impl ApprovalGate {
    /// Blocks until the answer for `id` arrives. Fails closed: a cancel, or a
    /// sender that went away, is a Deny. Several waiters may share the gate:
    /// an answer meant for another one is parked for it, not dropped.
    // ponytail: parked answers to prompts nobody waits for anymore are never
    // pruned; a few bytes per stale answer.
    pub(crate) fn wait(&self, id: u64, cancel: &Cancel) -> Approval {
        loop {
            if cancel.is_cancelled() {
                return Approval::Deny;
            }
            if let Some(a) = self.early.lock().ok().and_then(|mut e| e.remove(&id)) {
                return a;
            }
            let Ok(rx) = self.rx.lock() else {
                return Approval::Deny;
            };
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((got, a)) if got == id => return a,
                Ok((got, a)) => {
                    drop(rx);
                    if let Ok(mut e) = self.early.lock() {
                        e.insert(got, a);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Approval::Deny,
            }
        }
    }

    /// A subagent's prompt: queued here, emitted by the parent loop while it
    /// waits for the `agent` call (the child cannot reach the parent's
    /// `on_event`).
    pub(crate) fn relay(&self, e: RunEvent) {
        if let Ok(mut q) = self.relayed.lock() {
            q.push(e);
        }
    }

    pub(crate) fn take_relayed(&self) -> Vec<RunEvent> {
        self.relayed
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    // ponytail: grants are per tool name ("allow shell for the session"), not
    // per command prefix. Key on (tool, pattern) if users want finer grants.
    pub(crate) fn granted(&self, tool: &str) -> bool {
        self.granted
            .lock()
            .map(|g| g.contains(tool))
            .unwrap_or(false)
    }

    pub(crate) fn grant(&self, tool: &str) {
        if let Ok(mut g) = self.granted.lock() {
            g.insert(tool.to_string());
        }
    }
}

/// The UI's half of the steering channel: say something to the agent while
/// it works.
#[derive(Clone)]
pub struct InboxSender(mpsc::Sender<String>);

impl InboxSender {
    /// Returns false when the receiver is gone.
    pub fn send(&self, text: impl Into<String>) -> bool {
        self.0.send(text.into()).is_ok()
    }
}

/// The turn's half. Messages sent while a turn runs are drained at the next
/// step boundary (before the next model request, or before the turn would
/// end) and appended as user messages, stored and logged like any other.
pub struct InboxReceiver(Mutex<mpsc::Receiver<String>>);

impl InboxReceiver {
    fn drain(&self) -> Vec<String> {
        let Ok(rx) = self.0.lock() else {
            return Vec::new();
        };
        rx.try_iter().filter(|s| !s.trim().is_empty()).collect()
    }
}

pub fn inbox_channel() -> (InboxSender, InboxReceiver) {
    let (tx, rx) = mpsc::channel();
    (InboxSender(tx), InboxReceiver(Mutex::new(rx)))
}

/// What the loop may use besides the conversation. `Default` is "no tools":
/// one request, one answer, exactly the old turn.
pub struct AgentOptions<'a> {
    pub tools: Option<&'a Registry>,
    /// Required for tools to run; without it every call is refused.
    pub policy: Option<&'a Policy>,
    /// Without a gate, a call that needs approval is refused (fail closed).
    pub approvals: Option<&'a ApprovalGate>,
    pub max_steps: usize,
    /// Past messages replayed to the model. The execution profile sets it
    /// (`Profile::limits().history_window`); the default is `HISTORY_LIMIT`.
    pub history: usize,
    /// Run around every tool call, in order. The host puts hooks and
    /// permission rules here (see [`Middleware`]).
    pub middleware: Vec<Box<dyn Middleware + Send + Sync>>,
    /// Safe calls of one step that may run at once; 1 is serial.
    pub max_parallel_tools: usize,
    /// Where outputs over `OUTPUT_CAP_CHARS` are written
    /// (`<dir>/<session>/<call>.txt`). `None` truncates instead.
    pub spill_dir: Option<PathBuf>,
    /// Writes compaction summaries. `None`: the turn's own client and model.
    pub summarizer: Option<&'a dyn Summarizer>,
    /// Messages from the user, sent while the turn runs.
    pub inbox: Option<&'a InboxReceiver>,
    /// Where `cancel_with` recorded why; read into `turn/end`.
    pub cancel_cause: Option<&'a CauseSlot>,
    /// Where `ask_user` questions are answered. Without one the call fails
    /// closed ("o usuário não respondeu").
    pub questions: Option<&'a QuestionGate>,
    /// Optional dynamic reminder appended transiently to the latest user message
    /// (in addition to recalled memory and dynamic skill matches).
    pub dynamic_context: Option<String>,
    /// Optional reasoning effort hint (`"low"`, `"medium"`, `"high"`).
    pub reasoning_effort: Option<String>,
    /// Strict Orchestrator Mode: the root model thinks, plans, and coordinates
    /// via `agent` (which runs on `executor_model` or `routing.subagent_model`),
    /// while heavy execution tools are restricted to the subagents.
    pub orchestrator: bool,
    /// Explicit executor model selected for subagents in this turn.
    pub executor_model: Option<String>,
    /// Depth and concurrency of the subagents this turn may start.
    pub subagents: SubagentOptions,
}

pub const DEFAULT_MAX_STEPS: usize = 25;
pub const DEFAULT_MAX_PARALLEL_TOOLS: usize = 4;

impl Default for AgentOptions<'_> {
    fn default() -> Self {
        Self {
            tools: None,
            policy: None,
            approvals: None,
            max_steps: DEFAULT_MAX_STEPS,
            history: HISTORY_LIMIT,
            middleware: Vec::new(),
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            spill_dir: None,
            summarizer: None,
            inbox: None,
            cancel_cause: None,
            questions: None,
            dynamic_context: None,
            reasoning_effort: None,
            orchestrator: false,
            executor_model: None,
            subagents: SubagentOptions::default(),
        }
    }
}

impl AgentOptions<'_> {
    /// No tools, with the history window of an execution profile.
    pub fn for_profile(p: zerith_core::settings::Profile) -> Self {
        Self {
            history: p.limits().history_window,
            ..Self::default()
        }
    }
}

const PREVIEW_CHARS: usize = 4000;
/// Of a failed attempt's partial text, how much the audit log keeps.
const ATTEMPT_CHARS: usize = 2000;

/// Runs one agent turn with tools. See the module docs for the loop.
#[allow(clippy::too_many_arguments)]
pub fn run_agent_turn(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    agent: AgentOptions<'_>,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &Cancel,
    mut on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    let on_event: &mut dyn FnMut(RunEvent) = &mut on_event;
    let mut agent = agent;
    // Subagents run their tools through the very same instances: a hook's
    // deny binds them as it binds this turn.
    let inherited = subagent::share(&mut agent.middleware);
    // billion-context keys its compression state on this id.
    let _conversation = zerith_providers::bili::conversation(session_id);
    let session = store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;
    let log = TurnLog::new(store, session_id);
    log.emit(
        "turn/start",
        json!({"model": model, "prompt_chars": prompt.chars().count()}),
    );

    let result = turn_body(
        store,
        client,
        ctx,
        &agent,
        &inherited,
        &session.title,
        session_id,
        model,
        prompt,
        cancel,
        on_event,
        &log,
    );
    // No child may outlive its turn unseen: whatever is still running in the
    // background (error, stop, step limit) is stopped and reported.
    for (id, name) in subagent::abandon(session_id) {
        on_event(RunEvent::Subagent {
            id,
            name,
            state: "cancelled".into(),
            summary: "parado: a conversa terminou antes dele".into(),
            model: String::new(),
        });
    }
    match &result {
        Ok((_, reason)) => log.end(reason, None, None),
        Err(RunError::Provider(ProviderError::Cancelled)) => {
            let cause = agent
                .cancel_cause
                .and_then(|s| s.get())
                .unwrap_or(CancelCause::User);
            log.end("cancelled", Some(cause), None);
        }
        Err(e) => log.end("error", None, Some(e.to_string())),
    }
    result.map(|(report, _)| report)
}

/// The loop proper. Returns the report and why it stopped (`done` or
/// `max_steps`); errors are classified by the caller.
#[allow(clippy::too_many_arguments)]
fn turn_body(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    agent: &AgentOptions<'_>,
    inherited: &[subagent::SharedMiddleware],
    title: &str,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
    log: &TurnLog<'_>,
) -> Result<(TurnReport, &'static str)> {
    if let Some(dir) = &agent.spill_dir {
        spill::prune(dir, SPILL_MAX_AGE);
    }
    // hooks run where the turn's tools act: the session's folder, not the
    // process's (the desktop starts from anywhere)
    let cwd = agent
        .policy
        .map(|p| p.workspace.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut prompt_context = Vec::new();
    if let Some(st) = ctx.settings {
        if store.message_count(session_id).unwrap_or(1) == 0 {
            let start_out = run_hooks(
                HookEvent::SessionStart,
                &HookPayload {
                    session_id: session_id.to_string(),
                    cwd: cwd.clone(),
                    ..Default::default()
                },
                st,
            );
            log_hook_errors(log, HookEvent::SessionStart, &start_out.errors);
        }
        let prompt_out = run_hooks(
            HookEvent::UserPromptSubmit,
            &HookPayload {
                session_id: session_id.to_string(),
                prompt: Some(prompt.to_string()),
                cwd: cwd.clone(),
                ..Default::default()
            },
            st,
        );
        log_hook_errors(log, HookEvent::UserPromptSubmit, &prompt_out.errors);
        if prompt_out.blocked() {
            return Err(RunError::Blocked(hook_reason(
                &prompt_out,
                "UserPromptSubmit",
            )));
        }
        prompt_context = prompt_out.additional_context;
    }
    // Checkpoint first: its boundary is the newest message *before* this
    // prompt, so a conversation rewind removes the prompt too. A failure only
    // disables rewind for this turn; it never stops the turn.
    if let (Some(r), Some(root)) = (
        agent.tools,
        store.conn().path().filter(|p| !p.is_empty()).and_then(|p| {
            std::path::Path::new(p)
                .parent()
                .map(std::path::Path::to_path_buf)
        }),
    ) {
        let opened = checkpoints::next_turn(store, session_id)
            .and_then(|turn| checkpoints::Checkpoint::begin(store, &root, session_id, turn));
        match opened {
            Ok(cp) => {
                // The whole workspace is snapshotted at the turn's first
                // mutation, so rewind also undoes what the shell changed.
                let cp = Arc::new(cp);
                let (w, ws) = (cp.clone(), cwd.clone());
                r.host().on_before_write(move |p| {
                    w.snapshot(&ws);
                    w.record_file(p).map_err(|e| e.to_string())
                });
                r.host().on_before_shell(move |ws| cp.snapshot(ws));
            }
            Err(e) => eprintln!("zerith: checkpoint indisponível: {e}"),
        }
    }
    let prompt_row_id = store.append(session_id, &NewMessage::new(Role::User, prompt))?;

    let kids_saved = if let (Some(r), Some(p)) = (agent.tools, agent.policy) {
        Some(subagent::bind(
            r,
            p,
            store,
            client,
            &ctx,
            model,
            agent.executor_model.as_deref(),
            session_id,
            &agent.subagents,
            agent.approvals,
            inherited,
        ))
    } else {
        None
    };

    // The model has the skill tool exactly when the registry offers it.
    let skill_tool = agent.tools.is_some_and(|r| r.get("skill").is_some());
    let mut prompt_ctx = crate::build_prompt_context(title, ctx, skill_tool, session_id, prompt);
    if let Some(r) = agent.tools {
        prompt_ctx.static_system.push_str(&crate::tool_guidance(r));
    }
    let is_root_orchestrator = agent.orchestrator
        && agent.subagents.depth == 0
        && agent.tools.is_some_and(|r| r.get("agent").is_some());
    let exec_label = agent
        .executor_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ctx.settings
                .and_then(|st| st.routing.subagent_model.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(model);
    let orch_block = is_root_orchestrator.then(|| {
        format!(
            "# Modo Orquestrador Ativo (Pensamento: `{model}` → Execução: `{exec_label}`)\n\
             Você é o **Modelo de Pensamento (Arquiteto/Orquestrador)**. As ferramentas operacionais pesadas (`shell`, `edit_file`, `write_file`, `multi_edit`, `net_connect`, `terminal_batch`, `net_broadcast`, `net_vpn`, `net_probe`, `net_path`, `net_backup`, `net_transfer`, `read_file`, `grep`, `glob`) estão atribuídas exclusivamente aos subagentes executores, que rodam no modelo **`{exec_label}`**.\n\
             Fluxo obrigatório para pedidos técnicos/operacionais:\n\
             1. **Pense e Planeje:** Analise o pedido, consulte as skills pré-carregadas (e `net_inventory` se envolver clientes) e estruture os passos com `task_create`.\n\
             2. **Delegue a Execução:** Chame a ferramenta `agent` (ex.: `agent=\"executor\"`, `\"netops-architect\"`, `\"proxmox-architect\"`, `\"noc-incident-commander\"`, `\"ftth-gpon-architect\"`, `\"linux-devops-architect\"`, `\"explore\"` ou `\"general\"`) passando um `prompt` completo e autossuficiente (com IPs/cliente, instruções da skill e o que verificar). Para tarefas independentes, chame vários `agent` em paralelo.\n\
             3. **Audite e Conclua:** Revise o relatório técnico do Executor, atualize `task_update` e entregue a resposta final ao usuário.\n\
             (Se o usuário fizer apenas uma pergunta conceitual simples que não exija executar ferramentas, responda diretamente.)"
        )
    });
    let merged_dyn = match (
        agent
            .dynamic_context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        orch_block.as_deref(),
    ) {
        (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
        (Some(a), None) => Some(a.to_string()),
        (None, Some(b)) => Some(b.to_string()),
        (None, None) => None,
    };
    let dynamic_reminder = match (prompt_ctx.dynamic_reminder, merged_dyn.as_deref()) {
        (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
        (a, None) => a,
        (None, Some(b)) => Some(b.to_string()),
    };
    let system = vec![ChatMessage::system(prompt_ctx.static_system)];
    // Resolve Gateway once per turn (instead of hitting settings/keyring on
    // every tool step). Its first candidate sets the context window and folding.
    let mut gateway = match ctx.settings {
        Some(st) => crate::resolve_gateway(st, model, client.config()).0,
        None => default_gateway(client, model),
    };
    gateway.final_retry = Some(client.retry_policy());
    let first = gateway.candidates.first();
    let routed_cfg = first.map_or(client.config(), |c| &c.config);
    let window = first
        .and_then(|c| c.config.context_limit)
        .or(client.config().context_limit)
        .or_else(|| zerith_providers::models::context_window(first.map_or(model, |c| &c.model)))
        .filter(|&l| l > 0)
        .map_or(crate::compact::DEFAULT_CONTEXT, |l| l as usize);
    // billion-context folds the history on the wire: it gets the whole
    // conversation, and nothing here prunes or summarizes it (two compressors
    // over one history fight, and a rewritten prefix loses its cache).
    let folded = || zerith_providers::bili::routes(routed_cfg);
    // A 1M window holds far more than the profile's 40 prompts.
    // ponytail: 2000 prompts through the proxy; the proxy's own store is the
    // real history past that.
    let replay = if folded() {
        2000
    } else {
        agent.history.max(1) * (window / crate::compact::DEFAULT_CONTEXT).clamp(1, 8)
    };
    // The newest rows, which include the prompt just appended.
    let rows = store.history_rows(session_id, replay)?;
    let mut hist = History::new(system, replay_items(&rows));
    for text in prompt_context {
        if text.trim().is_empty() {
            continue;
        }
        let cid = store.append(
            session_id,
            &NewMessage {
                raw: Some(json!({"hook_context": true})),
                ..NewMessage::new(Role::User, text.clone())
            },
        )?;
        hist.push(ChatMessage::user(text), cid);
    }

    let context_probe = {
        let mut s = prompt.to_lowercase();
        if let Some(rem) = &dynamic_reminder {
            s.push(' ');
            s.push_str(&rem.to_lowercase());
        }
        for row in rows.iter().rev().take(6) {
            s.push(' ');
            s.push_str(&row.content.to_lowercase());
            if let Some(tn) = &row.tool_name {
                s.push(' ');
                s.push_str(&tn.to_lowercase());
            }
        }
        s
    };

    let defs: Vec<ToolDef> = agent
        .tools
        .map(|r| {
            let specs = r.specs();
            let large_registry = specs.len() > 20;
            specs
                .into_iter()
                // Deferred tools run through `mcp_call`: no schema per request.
                .filter(|s| !s.source.starts_with(zerith_tools::DEFERRED_SOURCE))
                .filter(|s| {
                    if is_root_orchestrator {
                        is_orchestrator_root_tool(&s.name)
                    } else if large_registry {
                        should_include_tool_schema(&s.name, &context_probe)
                    } else {
                        true
                    }
                })
                .map(|s| ToolDef {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    parameters: s.parameters.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    // The tool schemas ride on every request: their share of the window is
    // not history's to use.
    // ponytail: floor at a quarter of the window so a huge non-deferred
    // toolset does not drop all history; a per-model window fixes it properly.
    let schema_tokens = serde_json::to_string(&defs).map_or(0, |s| s.len() / 4);
    let budget = Some(
        (window * 8 / 10)
            .saturating_sub(schema_tokens)
            .max(window / 4),
    );
    let default_summarizer = ClientSummarizer { client, model };
    let summarizer: &dyn Summarizer = agent.summarizer.unwrap_or(&default_summarizer);
    let mut summary_failed = false;

    let pipe = Pipeline {
        core: agent.tools.zip(agent.policy),
        memory: ctx.memory,
        scope: ctx.scope,
        middleware: &agent.middleware,
        approvals: agent.approvals,
        relay: agent.subagents.depth == 0,
        spill_dir: agent.spill_dir.as_deref(),
        session_id,
        cancel,
    };

    let (mut prompt_tokens, mut completion_tokens) = (0u64, 0u64);
    let (mut cache_read_tokens, mut cache_write_tokens) = (0u64, 0u64);
    let mut pending_rtk_tokens: u64 = 0;
    let mut total_saved_rtk_tokens: u64 = 0;
    let mut total_saved_ponytail_tokens: u64 = 0;
    let mut total_saved_caveman_tokens: u64 = 0;
    let mut last_account_id: Option<String> = None;
    let mut last_model_used: Option<String> = None;
    let mut last = (0i64, String::new(), None::<String>);
    let max_steps = agent.max_steps.max(1);
    let mut reason = "done";

    for step in 1..=max_steps {
        on_event(RunEvent::Step { n: step });
        steer(store, session_id, agent, &mut hist, log, on_event)?;

        if let Some(b) = budget {
            if hist.tokens() > shrink_at(window, schema_tokens, b, folded()) {
                shrink(
                    store,
                    session_id,
                    &mut hist,
                    b,
                    summarizer,
                    &mut summary_failed,
                    cancel,
                    log,
                    on_event,
                    ctx.settings,
                    &cwd,
                );
            }
        }

        log.emit(
            "step/start",
            json!({"n": step, "tokens_estimate": hist.tokens()}),
        );
        let mut chat_msgs = hist.chat();
        let prompt_msg_idx = hist
            .items
            .iter()
            .position(|it| it.lo == prompt_row_id && it.hi == prompt_row_id)
            .map(|i| hist.system.len() + i);
        crate::inject_dynamic_reminder(&mut chat_msgs, prompt_msg_idx, dynamic_reminder.as_deref());
        let mut req = ChatRequest::new(model, chat_msgs).prompt_cache_key(session_id);
        if let Some(effort) = agent
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("off"))
        {
            req = req.reasoning_effort(effort);
        }
        if !defs.is_empty() {
            req = req.tools(defs.clone()).tool_choice("auto");
        }
        let mut failed = Vec::new();
        // What this step painted, kept if the step dies (error or stop).
        let mut painted = String::new();
        let out = request(
            store,
            &gateway,
            ctx,
            agent,
            session_id,
            step,
            &req,
            cancel,
            &mut |e: RunEvent| {
                if let RunEvent::Delta(t) = &e {
                    painted.push_str(t);
                }
                on_event(e)
            },
            log,
            &mut failed,
            &mut pending_rtk_tokens,
        );
        // A failed or abandoned attempt is evidence, not history: it goes to
        // the log and never to `messages`.
        for f in failed {
            log.emit("assistant/attempt", json!({"step": step, "attempt": f}));
        }
        let (mut out, gw_outcome, ttft_ms, step_savings) = match out {
            Ok(o) => o,
            Err(e) => {
                keep_partial(store, session_id, &painted, &e, log)?;
                return Err(e);
            }
        };
        // The output cap cut the answer: a call's JSON is incomplete and must
        // not run (half a file); a text answer is kept and says so.
        let cut = out
            .stop_reason
            .as_deref()
            .is_some_and(zerith_providers::is_cut_off);
        let cut_calls = cut && !out.calls.is_empty();
        if cut_calls {
            for c in &mut out.calls {
                c.arguments = "{}".into();
            }
        } else if cut {
            out.text.push_str(CUT_TEXT);
            on_event(RunEvent::Delta(CUT_TEXT.into()));
        }
        total_saved_ponytail_tokens += step_savings.ponytail;
        total_saved_caveman_tokens += step_savings.caveman;
        last_account_id = Some(gw_outcome.account_id);
        last_model_used = Some(gw_outcome.model);
        if let Some(u) = &out.usage {
            prompt_tokens += u.prompt_tokens;
            completion_tokens += u.completion_tokens;
            cache_read_tokens += u.cache_read_tokens;
            cache_write_tokens += u.cache_write_tokens;
            on_event(RunEvent::Usage {
                input: u.prompt_tokens,
                output: u.completion_tokens,
                cache_read: u.cache_read_tokens,
                cache_write: u.cache_write_tokens,
                ttft_ms,
            });
        }

        let id = store.append(
            session_id,
            &NewMessage {
                reasoning: out.reasoning.clone(),
                token_count: out.usage.as_ref().map(|u| u.completion_tokens as i64),
                tool_calls: (!out.calls.is_empty()).then(|| json!(out.calls)),
                ..NewMessage::new(Role::Assistant, out.text.clone())
            },
        )?;
        log.emit(
            "step/end",
            json!({"n": step, "message_id": id, "tool_calls": out.calls.len(),
                   "prompt_tokens": out.usage.as_ref().map(|u| u.prompt_tokens),
                   "completion_tokens": out.usage.as_ref().map(|u| u.completion_tokens)}),
        );
        last = (id, out.text.clone(), out.reasoning.clone());
        hist.push(ChatMessage::assistant(out.text, out.calls.clone()), id);
        if out.calls.is_empty() {
            // Told something while the answer streamed: the turn is not over.
            if step < max_steps && steer(store, session_id, agent, &mut hist, log, on_event)? > 0 {
                continue;
            }
            // Background children the model never collected: the turn waits
            // for them (showing their progress) and hands the results back,
            // instead of ending with work still running unseen.
            if step < max_steps {
                let joined = subagent::join_background(session_id, cancel, || {
                    subagent::flush(store, session_id, agent.tools, on_event);
                    if let (0, Some(g)) = (agent.subagents.depth, agent.approvals) {
                        for e in g.take_relayed() {
                            on_event(e);
                        }
                    }
                });
                // Stopped while waiting: the turn was stopped, not answered.
                if cancel.is_cancelled() {
                    return Err(ProviderError::Cancelled.into());
                }
                if !joined.is_empty() {
                    let text = background_report(&joined);
                    let id = store.append(
                        session_id,
                        &NewMessage {
                            raw: Some(json!({"subagents_joined": true})),
                            ..NewMessage::new(Role::User, text.clone())
                        },
                    )?;
                    log.emit(
                        "subagent/joined",
                        json!({"n": joined.len(), "message_id": id}),
                    );
                    hist.push(ChatMessage::user(text), id);
                    continue;
                }
            }
            if step < max_steps {
                if let Some(st) = ctx.settings {
                    let stop_out = run_hooks(
                        HookEvent::Stop,
                        &HookPayload {
                            session_id: session_id.to_string(),
                            cwd: cwd.clone(),
                            ..Default::default()
                        },
                        st,
                    );
                    log_hook_errors(log, HookEvent::Stop, &stop_out.errors);
                    if stop_out.blocked() {
                        let msg = stop_message(&stop_out);
                        let cid = store.append(
                            session_id,
                            &NewMessage {
                                raw: Some(json!({"hook_stop": true})),
                                ..NewMessage::new(Role::User, msg.clone())
                            },
                        )?;
                        hist.push(ChatMessage::user(msg), cid);
                        continue;
                    }
                }
            }
            break;
        }

        let mut context: Vec<String> = Vec::new();
        let mut i = 0;
        if cut_calls {
            for c in &out.calls {
                on_event(RunEvent::ToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    args: c.arguments.clone(),
                });
                record(store, session_id, c, CUT_CALL, &mut hist)?;
                log.emit(
                    "tool/result",
                    json!({"id": c.id, "name": c.name, "ok": false, "cut": true}),
                );
                on_event(RunEvent::ToolResult {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    ok: false,
                    output_preview: CUT_CALL.chars().take(PREVIEW_CHARS).collect(),
                });
            }
            i = out.calls.len();
        }
        while i < out.calls.len() {
            if cancel.is_cancelled() {
                // Close every open call so the stored history stays paired.
                close_cancelled(store, session_id, &out.calls[i..], &mut hist, log)?;
                return Err(ProviderError::Cancelled.into());
            }
            // Consecutive safe calls form a group; anything else stands alone
            // and is a barrier.
            let mut j = i + 1;
            if pipe.is_safe(&out.calls[i]) {
                while j < out.calls.len() && pipe.is_safe(&out.calls[j]) {
                    j += 1;
                }
            }
            let group = &out.calls[i..j];
            let mut prepared = Vec::with_capacity(group.len());
            for call in group {
                on_event(RunEvent::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    args: call.arguments.clone(),
                });
                log.emit(
                    "tool/call",
                    json!({"id": call.id, "name": call.name, "args": call.arguments}),
                );
                prepared.push(pipe.prepare(call, on_event));
            }
            // `ask_user` blocks on the person, so the loop answers it itself,
            // on this thread, before the rest of the group runs.
            for p in prepared.iter_mut() {
                if p.settled.is_none() && p.inv.name == "ask_user" {
                    let (ok, text) =
                        question::settle(agent.questions, &p.inv.args, cancel, on_event);
                    p.settled = Some(crate::pipeline::ToolOutcome::new(ok, text));
                }
            }
            let ran = if cancel.is_cancelled() {
                vec![None; prepared.len()]
            } else {
                pipe.run_group(&prepared, agent.max_parallel_tools.max(1), on_event)
            };
            subagent::flush(store, session_id, agent.tools, on_event);
            // ponytail: a background child's prompt is shown at the next tool
            // group; pump between stream chunks if that wait ever matters.
            if let (0, Some(g)) = (agent.subagents.depth, agent.approvals) {
                for e in g.take_relayed() {
                    on_event(e);
                }
            }
            // Commit in the model's order, whatever order they finished in.
            for ((call, p), ran) in group.iter().zip(prepared).zip(ran) {
                let ran = ran.or_else(|| {
                    cancel
                        .is_cancelled()
                        .then(|| (false, "[cancelado]".to_string()))
                });
                let (mut outcome, spilled) = pipe.finish(p, ran);
                if ctx.settings.is_some_and(|s| s.token_saver.rtk_enabled) {
                    let before_bytes = outcome.output.len();
                    let compressed =
                        crate::tokensaver::compress_tool_output(&call.name, &outcome.output);
                    if compressed.saved_bytes > 0 {
                        let after_bytes = compressed.text.len();
                        // What RTK dropped stays readable: a cut `show run` or
                        // build log the model cannot get back makes it guess.
                        let kept = spilled
                            .is_none()
                            .then_some(agent.spill_dir.as_deref())
                            .flatten()
                            .and_then(|root| {
                                let p = crate::spill::spill_path(root, session_id, &call.id);
                                std::fs::create_dir_all(p.parent()?).ok()?;
                                std::fs::write(&p, &outcome.output).ok()?;
                                Some(p)
                            });
                        outcome.output = match kept {
                            Some(p) => format!(
                                "{}\n[resumido; saída completa: {}; use read_file com offset/limit ou grep]",
                                compressed.text,
                                p.display()
                            ),
                            None => compressed.text,
                        };
                        pending_rtk_tokens += compressed.saved_tokens;
                        total_saved_rtk_tokens += compressed.saved_tokens;
                        log.emit(
                            "tool/rtk",
                            json!({
                                "tool": call.name,
                                "before": before_bytes,
                                "after": after_bytes,
                                "saved_tokens": compressed.saved_tokens,
                                "filter": compressed.filter,
                            }),
                        );
                        on_event(RunEvent::TokenSaved {
                            tool: call.name.clone(),
                            before_bytes,
                            after_bytes,
                            filter: compressed.filter.map(str::to_string),
                        });
                    }
                }
                record(store, session_id, call, &outcome.output, &mut hist)?;
                let preview: String = outcome.output.chars().take(PREVIEW_CHARS).collect();
                log.emit(
                    "tool/result",
                    json!({"id": call.id, "name": call.name, "ok": outcome.ok,
                           "chars": outcome.output.chars().count(),
                           "spill": spilled.as_ref().map(|p| p.display().to_string()),
                           "preview": preview}),
                );
                on_event(RunEvent::ToolResult {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    ok: outcome.ok,
                    output_preview: preview,
                });
                context.extend(outcome.context);
            }
            i = j;
        }
        // Extra context comes after every result of the step: a user message
        // between two tool results would break their pairing.
        for text in context {
            let id = store.append(
                session_id,
                &NewMessage {
                    raw: Some(json!({"tool_context": true})),
                    ..NewMessage::new(Role::User, text.clone())
                },
            )?;
            log.emit(
                "tool/context",
                json!({"message_id": id, "chars": text.chars().count()}),
            );
            hist.push(ChatMessage::user(text), id);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        if step == max_steps {
            // Said out loud and stored: a turn that silently stops mid-task
            // reads as a finished one.
            let note = format!("[parado: limite de {max_steps} passos atingido]");
            on_event(RunEvent::Delta(note.clone()));
            let id = store.append(session_id, &NewMessage::new(Role::Assistant, note.clone()))?;
            last = (id, note, None);
            reason = "max_steps";
        }
    }

    // What the subagents saved counts for the turn that delegated; their own
    // usage rows already hold it, so only the report adds it up.
    if let Some(k) = kids_saved.and_then(|k| k.lock().ok().map(|k| *k)) {
        total_saved_rtk_tokens += k.rtk;
        total_saved_ponytail_tokens += k.ponytail;
        total_saved_caveman_tokens += k.caveman;
    }
    on_event(RunEvent::Done {
        stored: Some(last.0),
    });
    let total_saved_tokens = total_saved_rtk_tokens
        .saturating_add(total_saved_ponytail_tokens)
        .saturating_add(total_saved_caveman_tokens);
    Ok((
        TurnReport {
            session_id: session_id.to_string(),
            message_id: last.0,
            text: last.1,
            reasoning: last.2,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
            saved_rtk_tokens: total_saved_rtk_tokens,
            saved_ponytail_tokens: total_saved_ponytail_tokens,
            saved_caveman_tokens: total_saved_caveman_tokens,
            saved_tokens: total_saved_tokens,
            account_id: last_account_id,
            model_used: last_model_used,
        },
        reason,
    ))
}

/// Brings the history under `budget`: prune tool bodies, then summarize the
/// oldest span, then drop whole old turns. Never fails the turn.
#[allow(clippy::too_many_arguments)]
/// Tokens of history past which the turn shrinks it. With billion-context
/// folding on the wire Zerith stays out of the way and steps in only near
/// the window's edge, where the provider would refuse the request.
fn shrink_at(window: usize, schema_tokens: usize, budget: usize, folded: bool) -> usize {
    if folded {
        (window * 95 / 100)
            .saturating_sub(schema_tokens)
            .max(budget)
    } else {
        budget
    }
}

#[allow(clippy::too_many_arguments)]
fn shrink(
    store: &Store,
    session_id: &str,
    hist: &mut History,
    budget: usize,
    summarizer: &dyn Summarizer,
    summary_failed: &mut bool,
    cancel: &Cancel,
    log: &TurnLog<'_>,
    on_event: &mut dyn FnMut(RunEvent),
    settings: Option<&zerith_core::Settings>,
    cwd: &Path,
) {
    let before = hist.tokens();
    hist.prune_tools(budget);
    if hist.tokens() > budget && !*summary_failed {
        let blocked = if let Some(st) = settings {
            let out = run_hooks(
                HookEvent::PreCompact,
                &HookPayload {
                    session_id: session_id.to_string(),
                    cwd: cwd.to_path_buf(),
                    ..Default::default()
                },
                st,
            );
            log_hook_errors(log, HookEvent::PreCompact, &out.errors);
            if out.blocked() {
                log.emit("hook/pre_compact_blocked", json!({"reasons": out.reasons}));
                true
            } else {
                false
            }
        } else {
            false
        };
        if !blocked {
            match crate::compact::summarize_into(
                store,
                session_id,
                hist,
                budget / 2,
                summarizer,
                cancel,
                log,
                "pressure",
            ) {
                Ok(Some(r)) => on_event(RunEvent::Compacted {
                    before: r.tokens_before,
                    after: r.tokens_after,
                }),
                Ok(None) => {}
                // A failed summary must not fail the turn: drop instead, and do
                // not retry it on every step.
                Err(_) => *summary_failed = true,
            }
        }
    }
    let dropped = hist.drop_oldest(budget);
    if dropped > 0 {
        log.emit(
            "compaction/fallback",
            json!({"dropped_items": dropped, "tokens_before": before,
                   "tokens_after": hist.tokens()}),
        );
    }
}

fn log_hook_errors(log: &TurnLog<'_>, event: HookEvent, errors: &[String]) {
    for err in errors {
        log.emit("hook/error", json!({"event": event.as_str(), "error": err}));
    }
}

fn hook_reason(out: &HookOutcome, fallback: &str) -> String {
    let parts: Vec<&str> = out
        .reasons
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        fallback.to_string()
    } else {
        parts.join("\n")
    }
}

fn stop_message(out: &HookOutcome) -> String {
    let parts: Vec<&str> = out
        .reasons
        .iter()
        .chain(&out.additional_context)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        "continue".to_string()
    } else {
        parts.join("\n")
    }
}

/// Appends what the user said while the turn ran. Returns how many messages.
fn steer(
    store: &Store,
    session_id: &str,
    agent: &AgentOptions<'_>,
    hist: &mut History,
    log: &TurnLog<'_>,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<usize> {
    let Some(inbox) = agent.inbox else {
        return Ok(0);
    };
    let msgs = inbox.drain();
    for text in &msgs {
        let id = store.append(
            session_id,
            &NewMessage {
                raw: Some(json!({"steer": true})),
                ..NewMessage::new(Role::User, text.clone())
            },
        )?;
        log.emit(
            "user/steer",
            json!({"message_id": id, "chars": text.chars().count()}),
        );
        on_event(RunEvent::Steered { text: text.clone() });
        hist.push(ChatMessage::user(text.clone()), id);
    }
    Ok(msgs.len())
}

/// What the model reads when background children finish after its answer.
fn background_report(joined: &[subagent::Joined]) -> String {
    let mut s = String::from(
        "[Os subagentes em segundo plano terminaram. Use os resultados abaixo e conclua a resposta.]",
    );
    for (id, name, r) in joined {
        match r {
            Ok(t) => s.push_str(&format!("\n\n## {id} ({name})\n{t}")),
            Err(e) => s.push_str(&format!("\n\n## {id} ({name}) falhou\n{e}")),
        }
    }
    s
}

fn record(
    store: &Store,
    session_id: &str,
    call: &ToolCall,
    output: &str,
    hist: &mut History,
) -> Result<()> {
    let id = store.append(
        session_id,
        &NewMessage {
            tool_name: Some(call.name.clone()),
            tool_calls: Some(json!({"tool_call_id": call.id})),
            ..NewMessage::new(Role::Tool, output)
        },
    )?;
    hist.push(ChatMessage::tool(call.id.clone(), output), id);
    Ok(())
}

/// A step that died after painting text keeps it: the person saw it, and a
/// conversation must not end on an unanswered prompt. Text only, no calls,
/// so replay stays paired.
fn keep_partial(
    store: &Store,
    session_id: &str,
    painted: &str,
    err: &RunError,
    log: &TurnLog<'_>,
) -> Result<()> {
    if painted.trim().is_empty() {
        return Ok(());
    }
    let why = match err {
        RunError::Provider(ProviderError::Cancelled) => "parada pelo usuário".to_string(),
        RunError::Provider(e) => zerith_providers::explain::headline(e),
        e => e.to_string(),
    };
    let text = format!("{painted}\n\n_[resposta interrompida: {why}]_");
    let id = store.append(
        session_id,
        &NewMessage {
            raw: Some(json!({"partial": true})),
            ..NewMessage::new(Role::Assistant, text)
        },
    )?;
    log.emit(
        "assistant/partial",
        json!({"message_id": id, "chars": painted.chars().count()}),
    );
    Ok(())
}

fn close_cancelled(
    store: &Store,
    session_id: &str,
    calls: &[ToolCall],
    hist: &mut History,
    log: &TurnLog<'_>,
) -> Result<()> {
    for c in calls {
        record(store, session_id, c, "[cancelado]", hist)?;
        log.emit(
            "tool/result",
            json!({"id": c.id, "name": c.name, "ok": false, "cancelled": true}),
        );
    }
    Ok(())
}

#[derive(Default)]
struct StepOut {
    text: String,
    reasoning: Option<String>,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    stop_reason: Option<String>,
}

/// What the model reads instead of running a call cut by the output cap: its
/// JSON is incomplete, and running it would write half a file.
const CUT_CALL: &str = "erro: resposta cortada pelo limite de saída antes de terminar esta \
chamada; nada foi executado. Divida o conteúdo: escreva o arquivo em partes menores \
(um arquivo curto e depois edições) e chame de novo.";
/// Appended to a text answer the output cap cut short.
const CUT_TEXT: &str = "\n\n_[resposta cortada pelo limite de saída do modelo]_";

fn default_gateway(client: &Client, model: &str) -> Gateway {
    let cfg = client.config();
    let provider = if cfg.name.trim().is_empty() {
        cfg.kind.as_str().to_string()
    } else {
        cfg.name.clone()
    };
    let account_id = cfg
        .account_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| provider.clone());
    Gateway::new(vec![RouteCandidate {
        account_id,
        provider,
        model: model.to_string(),
        config: cfg.clone(),
        priority: 0,
        quota_tokens: None,
        quota_requests: None,
        window_hours: 5,
    }])
}

fn record_failover(
    store: &Store,
    session_id: &str,
    role_kind: &str,
    cand: &RouteCandidate,
    err: &ProviderError,
    cooldown_ms: u64,
    failed: &mut Vec<Value>,
) {
    // 0 = the error is about the model, not the account (404): no cooldown
    if cooldown_ms > 0 {
        let until_ms = now_ms().saturating_add(i64::try_from(cooldown_ms).unwrap_or(i64::MAX));
        let _ = store.set_account_cooldown(&cand.account_id, until_ms, Some(&err.to_string()));
    }
    let status = if matches!(
        err,
        ProviderError::RateLimited { .. } | ProviderError::Http { status: 429, .. }
    ) {
        "rate_limited"
    } else {
        "error"
    };
    let _ = store.record_usage(&NewUsageRecord {
        session_id: Some(session_id.to_string()),
        account_id: cand.account_id.clone(),
        provider: cand.provider.clone(),
        model: cand.model.clone(),
        role_kind: role_kind.to_string(),
        prompt_tokens: 0,
        completion_tokens: 0,
        saved_rtk_tokens: 0,
        saved_ponytail_tokens: 0,
        saved_caveman_tokens: 0,
        saved_tokens: 0,
        status: status.to_string(),
        latency_ms: 0,
        ..NewUsageRecord::default()
    });
    failed.push(json!({
        "mode": "gateway_failover",
        "account": cand.account_id,
        "model": cand.model,
        "error": err.to_string(),
    }));
}

fn apply_stream_event(
    event: StreamEvent,
    out: &mut StepOut,
    painted: &mut bool,
    on_event: &mut dyn FnMut(RunEvent),
) {
    match event {
        StreamEvent::Delta(t) => {
            *painted = true;
            out.text.push_str(&t);
            on_event(RunEvent::Delta(t));
        }
        StreamEvent::Reasoning(r) => {
            *painted = true;
            out.reasoning.get_or_insert_with(String::new).push_str(&r);
            on_event(RunEvent::Reasoning(r));
        }
        StreamEvent::ToolCalls(c) => out.calls = c,
        // A trailing chunk without usage must not erase earlier counts.
        StreamEvent::Done { usage, stop_reason } => {
            if usage.is_some() {
                out.usage = usage;
            }
            if stop_reason.is_some() {
                out.stop_reason = stop_reason;
            }
        }
    }
}

/// Runs a single candidate preserving detailed `assistant/attempt` diagnostics
/// when streaming falls back to blocking `complete` on HTTP 400.
/// The only account is never cooled down (cooldown 0 below): with nothing to
/// fail over to, a cooldown just refuses the next turns unsent.
#[allow(clippy::too_many_arguments)]
fn request_single(
    store: &Store,
    gateway: &Gateway,
    cand: &RouteCandidate,
    session_id: &str,
    role_kind: &str,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
    failed: &mut Vec<Value>,
) -> Result<(StepOut, GatewayOutcome<()>)> {
    if cancel.is_cancelled() {
        return Err(ProviderError::Cancelled.into());
    }
    // The only account: a cooldown would refuse the request without sending
    // it, and blame a rate limit that may never have happened. Only the quota
    // the user set still holds; the real error of each attempt is shown.
    let over_quota = store
        .account_window_stats(&cand.account_id, cand.window_hours)
        .is_ok_and(|s| {
            cand.quota_tokens.is_some_and(|q| s.total_tokens >= q)
                || cand.quota_requests.is_some_and(|q| s.requests >= q)
        });
    if over_quota {
        return Err(ProviderError::RateLimited {
            retry_after_ms: None,
            body: format!(
                "a conta {} atingiu a cota configurada para a janela de {} h",
                cand.account_id, cand.window_hours
            ),
        }
        .into());
    }

    let started = Instant::now();
    let mut cfg = cand.config.clone();
    cfg.account_id = Some(cand.account_id.clone());
    let mut cand_client = match Client::new(cfg) {
        Ok(c) => c,
        Err(err) => {
            if is_failover_error(&err) {
                record_failover(store, session_id, role_kind, cand, &err, 0, failed);
            }
            return Err(err.into());
        }
    };
    if let Some(retry) = gateway.final_retry {
        cand_client = cand_client.with_retry(retry);
    }

    let mut cand_req = req.clone();
    cand_req.model = cand.model.clone();

    let mut out = StepOut::default();
    let mut painted = false;
    let streamed = cand_client.stream(&cand_req, cancel, |event| {
        apply_stream_event(event, &mut out, &mut painted, on_event);
    });
    let Err(e) = streamed else {
        return Ok((
            out,
            GatewayOutcome {
                value: (),
                account_id: cand.account_id.clone(),
                provider: cand.provider.clone(),
                model: cand.model.clone(),
                latency_ms: started.elapsed().as_millis() as u64,
                attempts: 1,
            },
        ));
    };

    if !matches!(e, ProviderError::Cancelled) {
        failed.push(json!({
            "mode": "stream",
            "error": e.to_string(),
            "partial": out.text.chars().take(ATTEMPT_CHARS).collect::<String>(),
            "partial_chars": out.text.chars().count(),
        }));
    }
    if painted || !matches!(e, ProviderError::Http { status: 400, .. }) {
        if !painted && is_failover_error(&e) {
            record_failover(store, session_id, role_kind, cand, &e, 0, failed);
        }
        return Err(e.into());
    }

    let c = match cand_client.complete(&cand_req, cancel) {
        Ok(c) => c,
        Err(e2) => {
            if !matches!(e2, ProviderError::Cancelled) {
                failed.push(json!({"mode": "complete", "error": e2.to_string()}));
            }
            if is_failover_error(&e2) {
                record_failover(store, session_id, role_kind, cand, &e2, 0, failed);
            }
            return Err(e2.into());
        }
    };
    if let Some(r) = &c.reasoning {
        on_event(RunEvent::Reasoning(r.clone()));
    }
    if !c.text.is_empty() {
        on_event(RunEvent::Delta(c.text.clone()));
    }
    Ok((
        StepOut {
            text: c.text,
            reasoning: c.reasoning,
            calls: c.tool_calls,
            usage: c.usage,
            stop_reason: c.finish_reason,
        },
        GatewayOutcome {
            value: (),
            account_id: cand.account_id.clone(),
            provider: cand.provider.clone(),
            model: cand.model.clone(),
            latency_ms: started.elapsed().as_millis() as u64,
            attempts: 1,
        },
    ))
}

#[derive(Default, Clone, Copy)]
struct StepSavings {
    ponytail: u64,
    caveman: u64,
}

/// One model request routed through the local [`Gateway`].
///
/// Streaming runs first; HTTP 400 falls back once to blocking completion.
/// Multi-account failover, cooldowns, and usage telemetry are recorded per
/// attempt against `store`.
#[allow(clippy::too_many_arguments)]
fn request(
    store: &Store,
    gateway: &Gateway,
    ctx: TurnContext<'_>,
    agent: &AgentOptions<'_>,
    session_id: &str,
    step: usize,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
    log: &TurnLog<'_>,
    failed: &mut Vec<Value>,
    pending_rtk_tokens: &mut u64,
) -> Result<(StepOut, GatewayOutcome<()>, u64, StepSavings)> {
    let role_kind = if agent.subagents.depth > 0 {
        "subagent"
    } else {
        "main"
    };

    let req_started = Instant::now();
    let mut first_token_ms: Option<u64> = None;
    let mut record_ev = |event: StreamEvent,
                         out: &mut StepOut,
                         painted: &mut bool,
                         on_event: &mut dyn FnMut(RunEvent)| {
        if first_token_ms.is_none()
            && matches!(
                event,
                StreamEvent::Delta(_) | StreamEvent::Reasoning(_) | StreamEvent::ToolCalls(_)
            )
        {
            first_token_ms = Some(req_started.elapsed().as_millis() as u64);
        }
        apply_stream_event(event, out, painted, on_event);
    };

    let (out, gw_outcome) = if gateway.candidates.len() == 1 {
        let cand = &gateway.candidates[0];
        let (out, outcome) = request_single(
            store,
            gateway,
            cand,
            session_id,
            role_kind,
            req,
            cancel,
            &mut |e| {
                if first_token_ms.is_none()
                    && matches!(e, RunEvent::Delta(_) | RunEvent::Reasoning(_))
                {
                    first_token_ms = Some(req_started.elapsed().as_millis() as u64);
                }
                on_event(e);
            },
            failed,
        )?;
        (out, outcome)
    } else {
        let mut out = StepOut::default();
        let mut painted = false;
        let gw_outcome = gateway.stream_with(
            req,
            cancel,
            |cand| {
                store
                    .is_account_available(
                        &cand.account_id,
                        cand.window_hours,
                        cand.quota_tokens,
                        cand.quota_requests,
                    )
                    .unwrap_or(true)
            },
            |cand, err, cooldown_ms| {
                record_failover(store, session_id, role_kind, cand, err, cooldown_ms, failed);
            },
            |event| record_ev(event, &mut out, &mut painted, on_event),
        )?;
        (out, gw_outcome)
    };
    let ttft_ms = first_token_ms.unwrap_or(gw_outcome.latency_ms);

    on_event(RunEvent::Route {
        account_id: gw_outcome.account_id.clone(),
        provider: gw_outcome.provider.clone(),
        model: gw_outcome.model.clone(),
        attempts: gw_outcome.attempts,
    });
    if ctx.settings.is_some() || gw_outcome.attempts > 1 {
        log.emit(
            "step/route",
            json!({
                "step": step,
                "account": gw_outcome.account_id,
                "provider": gw_outcome.provider,
                "model": gw_outcome.model,
                "attempts": gw_outcome.attempts,
                "latency_ms": gw_outcome.latency_ms,
                "ttft_ms": ttft_ms,
            }),
        );
    }

    let (prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens) = out
        .usage
        .as_ref()
        .map(|u| {
            (
                u.prompt_tokens,
                u.completion_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    let tool_args_bytes: usize = out.calls.iter().map(|c| c.arguments.len()).sum();
    let step_rtk = std::mem::take(pending_rtk_tokens);
    let (step_ponytail, step_caveman) = match ctx.settings {
        Some(st) => (
            crate::tokensaver::estimate_ponytail_saved_tokens(
                st.token_saver.ponytail,
                &out.text,
                tool_args_bytes,
                completion_tokens,
            ),
            crate::tokensaver::estimate_caveman_saved_tokens(
                st.token_saver.caveman,
                &out.text,
                completion_tokens,
            ),
        ),
        None => (0, 0),
    };
    let step_saved_tokens = step_rtk
        .saturating_add(step_ponytail)
        .saturating_add(step_caveman);

    let _ = store.record_usage(&NewUsageRecord {
        session_id: Some(session_id.to_string()),
        account_id: gw_outcome.account_id.clone(),
        provider: gw_outcome.provider.clone(),
        model: gw_outcome.model.clone(),
        role_kind: role_kind.to_string(),
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_write_tokens,
        saved_rtk_tokens: step_rtk,
        saved_ponytail_tokens: step_ponytail,
        saved_caveman_tokens: step_caveman,
        saved_tokens: step_saved_tokens,
        status: "ok".to_string(),
        latency_ms: gw_outcome.latency_ms,
        ttft_ms,
    });

    Ok((
        out,
        gw_outcome,
        ttft_ms,
        StepSavings {
            ponytail: step_ponytail,
            caveman: step_caveman,
        },
    ))
}

/// Tools the root Thinking/Brain model retains in Strict Orchestrator Mode.
/// All operational execution tools (`shell`, `edit_file`, `write_file`,
/// `net_connect`, `terminal_batch`, `net_broadcast`, `read_file`, `grep`, etc.)
/// remain bound in the subagent runtime (`rt.base`) so the Executor model gets
/// 100% of them, while the Brain model is physically restricted to planning,
/// inventory lookup, skill reading, and dispatching `agent`.
pub(crate) fn is_orchestrator_root_tool(name: &str) -> bool {
    matches!(
        name,
        "agent"
            | "agent_result"
            | "task_create"
            | "task_update"
            | "task_list"
            | "task_get"
            | "ask_user"
            | "skill"
            | "skill_search"
            | "memory_save"
            | "memory_recall"
            | "memory_update"
            | "memory_forget"
            | "memory_search"
            | "memory_delete"
            | "session_search"
            | "net_inventory"
            | "workflow_list"
            | "channel_list"
            | "enter_plan_mode"
            | "exit_plan_mode"
    )
}

/// Contextual tool-schema filter for large built-in registries (> 20 tools).
/// Keeps all core file, shell, task, subagent, skill, memory, diagnostics,
/// terminal, and network tools on every turn, and only includes bulky niche
/// schemas (`browser_*`, `lsp_*`, media generation, skill/agent authoring)
/// when the prompt or recent conversation mentions them.
pub(crate) fn should_include_tool_schema(name: &str, context_lower: &str) -> bool {
    if name.starts_with("browser_") {
        return [
            "browser",
            "navegador",
            "site",
            "página",
            "pagina",
            "web",
            "http://",
            "https://",
            "click",
            "clicar",
            "clique",
            "screenshot",
            "chrome",
            "kitesurf",
            "playwright",
            "painel",
            "dashboard",
            "grafana",
            "browser_",
        ]
        .iter()
        .any(|kw| context_lower.contains(kw));
    }
    if name.starts_with("lsp_") {
        return [
            "lsp",
            "definição",
            "definicao",
            "definition",
            "referência",
            "referencia",
            "references",
            "símbolo",
            "simbolo",
            "symbol",
            "hover",
            ".rs",
            ".ts",
            ".tsx",
            ".py",
            ".go",
            "struct ",
            "trait ",
            "impl ",
            "fn ",
            "class ",
            "function",
            "refator",
            "compil",
        ]
        .iter()
        .any(|kw| context_lower.contains(kw));
    }
    if matches!(name, "image_generate" | "tts_speak" | "audio_transcribe") {
        return [
            "gerar imagem",
            "crie uma imagem",
            "desenhe",
            "ilustra",
            "generate image",
            "image_generate",
            "tts",
            "voz",
            "falar",
            "narrar",
            "áudio",
            "audio",
            "transcrev",
            "whisper",
            ".mp3",
            ".wav",
            ".ogg",
            ".m4a",
            ".flac",
        ]
        .iter()
        .any(|kw| context_lower.contains(kw));
    }
    if matches!(name, "create_skill" | "create_agent" | "delete_ai_item") {
        return [
            "criar skill",
            "crie uma skill",
            "nova skill",
            "salvar skill",
            "create_skill",
            "criar agente",
            "crie um agente",
            "novo agente",
            "create_agent",
            "delete_ai_item",
        ]
        .iter()
        .any(|kw| context_lower.contains(kw));
    }
    if matches!(
        name,
        "workflow_list"
            | "channel_list"
            | "workflow_create_or_update"
            | "workflow_toggle"
            | "workflow_delete"
            | "workflow_run"
    ) {
        return [
            "workflow",
            "automação",
            "automacao",
            "automatiz",
            "automate",
            "fluxo",
            "rotina",
            "routine",
            "schedule",
            "agend",
            "cron",
            "whatsapp",
            "evolution",
            "waha",
            "telegram",
            "discord",
            "slack",
            "webhook",
            "alerta",
            "notifica",
            "relatório",
            "relatorio",
            "monitor",
            "channel_list",
            "workflow_",
        ]
        .iter()
        .any(|kw| context_lower.contains(kw));
    }
    true
}

#[cfg(test)]
mod shrink_tests {
    use super::shrink_at;

    #[test]
    fn with_billion_context_zerith_only_shrinks_near_the_edge() {
        // 1M window, 20k of tool schemas, the usual 80% budget.
        let budget = 1_000_000 * 8 / 10 - 20_000;
        assert_eq!(shrink_at(1_000_000, 20_000, budget, false), budget);
        assert_eq!(shrink_at(1_000_000, 20_000, budget, true), 930_000);
        // Never below the normal budget, even when the schemas eat the window.
        assert_eq!(shrink_at(100_000, 90_000, 25_000, true), 25_000);
    }
}

#[cfg(test)]
mod approval_grant_tests {
    use super::*;

    #[test]
    fn shell_prefix_scopes_simple_commands_and_isolates_chained_ones() {
        assert_eq!(
            approval_grant_key("shell", &json!({"command": "git status -s"})),
            "shell:prefix:git status *"
        );
        assert_eq!(
            approval_grant_key("shell", &json!({"command": "cargo test --lib"})),
            "shell:prefix:cargo test *"
        );
        assert_eq!(
            approval_grant_key("shell", &json!({"command": "ping -c 4 8.8.8.8"})),
            "shell:prefix:ping *"
        );
        assert_eq!(
            approval_grant_key("shell", &json!({"command": "git status && rm -rf /"})),
            "shell:exact:git status && rm -rf /"
        );
        assert_eq!(
            approval_grant_key("shell", &json!({"command": "ls | xargs rm"})),
            "shell:exact:ls | xargs rm"
        );
        assert_eq!(
            approval_grant_key("write_file", &json!({"path": "a.txt"})),
            "write_file"
        );
    }

    #[test]
    fn shared_grants_persist_across_approval_channels() {
        let shared: SharedGrants = Arc::default();
        let (_tx1, gate1) = approval_channel_with_grants(shared.clone());
        gate1.grant("shell:prefix:git commit *");
        let (_tx2, gate2) = approval_channel_with_grants(shared);
        assert!(gate2.granted("shell:prefix:git commit *"));
        assert!(!gate2.granted("shell:exact:git commit -m x && rm -rf ."));
    }

    #[test]
    fn orchestrator_root_keeps_brain_tools_and_delegates_execution_tools() {
        assert!(is_orchestrator_root_tool("agent"));
        assert!(is_orchestrator_root_tool("task_create"));
        assert!(is_orchestrator_root_tool("skill"));
        assert!(is_orchestrator_root_tool("net_inventory"));
        assert!(!is_orchestrator_root_tool("shell"));
        assert!(!is_orchestrator_root_tool("edit_file"));
        assert!(!is_orchestrator_root_tool("net_connect"));
        assert!(!is_orchestrator_root_tool("terminal_batch"));
    }

    #[test]
    fn contextual_schema_pruning_drops_unrelated_niche_tools_and_keeps_relevant_ones() {
        let netops_ctx = "conecte por ssh na mikrotik do cliente acme e veja o bgp";
        assert!(should_include_tool_schema("net_connect", netops_ctx));
        assert!(should_include_tool_schema("terminal_batch", netops_ctx));
        assert!(should_include_tool_schema("shell", netops_ctx));
        assert!(!should_include_tool_schema("browser_click", netops_ctx));
        assert!(!should_include_tool_schema("lsp_hover", netops_ctx));
        assert!(!should_include_tool_schema("image_generate", netops_ctx));
        assert!(!should_include_tool_schema("create_skill", netops_ctx));

        let browser_ctx = "abra o navegador em https://10.0.0.1 e tire um screenshot";
        assert!(should_include_tool_schema("browser_open", browser_ctx));
        assert!(should_include_tool_schema(
            "browser_screenshot",
            browser_ctx
        ));
    }
}
