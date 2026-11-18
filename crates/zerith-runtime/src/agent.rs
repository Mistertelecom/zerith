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
    is_failover_error, suggested_cooldown_ms, Cancel, ChatMessage, ChatRequest, Client, Gateway,
    GatewayOutcome, ProviderError, RouteCandidate, StreamEvent, ToolCall, ToolDef, Usage,
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
#[derive(Clone)]
pub struct ApprovalGate {
    rx: Arc<Mutex<mpsc::Receiver<(u64, Approval)>>>,
    granted: Arc<Mutex<HashSet<String>>>,
    /// Answers that arrived for another waiter (parallel children).
    early: Arc<Mutex<HashMap<u64, Approval>>>,
    /// `ApprovalNeeded` events of subagents, for the parent loop to emit.
    relayed: Arc<Mutex<Vec<RunEvent>>>,
}

pub fn approval_channel() -> (ApprovalSender, ApprovalGate) {
    let (tx, rx) = mpsc::channel();
    (
        ApprovalSender(tx),
        ApprovalGate {
            rx: Arc::new(Mutex::new(rx)),
            granted: Arc::default(),
            early: Arc::default(),
            relayed: Arc::default(),
        },
    )
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

const PREVIEW_CHARS: usize = 200;
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
            Ok(cp) => r
                .host()
                .on_before_write(move |p| cp.record_file(p).map_err(|e| e.to_string())),
            Err(e) => eprintln!("zerith: checkpoint indisponível: {e}"),
        }
    }
    store.append(session_id, &NewMessage::new(Role::User, prompt))?;

    // The model has the skill tool exactly when the registry offers it.
    let skill_tool = agent.tools.is_some_and(|r| r.get("skill").is_some());
    let mut system_text = crate::system_prompt(title, ctx, skill_tool, session_id, prompt);
    if let Some(r) = agent.tools {
        system_text.push_str(&crate::tool_guidance(r));
    }
    let system = vec![ChatMessage::system(system_text)];
    let kids_saved = if let (Some(r), Some(p)) = (agent.tools, agent.policy) {
        Some(subagent::bind(
            r,
            p,
            store,
            client,
            &ctx,
            model,
            session_id,
            &agent.subagents,
            agent.approvals,
        ))
    } else {
        None
    };
    // The newest rows, which include the prompt just appended.
    let rows = store.recent_messages(session_id, agent.history.max(1))?;
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

    let defs: Vec<ToolDef> = agent
        .tools
        .map(|r| {
            r.specs()
                .into_iter()
                // Deferred tools run through `mcp_call`: no schema per request.
                .filter(|s| !s.source.starts_with(zerith_tools::DEFERRED_SOURCE))
                .map(|s| ToolDef {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    parameters: s.parameters.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let budget = ctx
        .settings
        .and_then(|st| {
            crate::resolve_gateway(st, model, client.config())
                .0
                .candidates
                .into_iter()
                .next()
                .and_then(|c| c.config.context_limit)
        })
        .or(client.config().context_limit)
        .map(|l| l as usize * 8 / 10);
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

    let (mut prompt_tokens, mut completion_tokens) = (0, 0);
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
        steer(store, session_id, agent, &mut hist, log)?;

        if let Some(b) = budget {
            if hist.tokens() > b {
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
        let mut req = ChatRequest::new(model, hist.chat());
        if !defs.is_empty() {
            req = req.tools(defs.clone()).tool_choice("auto");
        }
        let mut failed = Vec::new();
        let out = request(
            store,
            client,
            ctx,
            agent,
            session_id,
            step,
            model,
            &req,
            cancel,
            on_event,
            log,
            &mut failed,
            &mut pending_rtk_tokens,
        );
        // A failed or abandoned attempt is evidence, not history: it goes to
        // the log and never to `messages`.
        for f in failed {
            log.emit("assistant/attempt", json!({"step": step, "attempt": f}));
        }
        let (out, gw_outcome, step_savings) = out?;
        total_saved_ponytail_tokens += step_savings.ponytail;
        total_saved_caveman_tokens += step_savings.caveman;
        last_account_id = Some(gw_outcome.account_id);
        last_model_used = Some(gw_outcome.model);
        if let Some(u) = &out.usage {
            prompt_tokens += u.prompt_tokens;
            completion_tokens += u.completion_tokens;
            on_event(RunEvent::Usage {
                input: u.prompt_tokens,
                output: u.completion_tokens,
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
            if step < max_steps && steer(store, session_id, agent, &mut hist, log)? > 0 {
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
                        outcome.output = compressed.text;
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
}

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
        StreamEvent::Done { usage } => {
            if usage.is_some() {
                out.usage = usage;
            }
        }
    }
}

/// Runs a single candidate preserving detailed `assistant/attempt` diagnostics
/// when streaming falls back to blocking `complete` on HTTP 400.
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
    let available = store
        .is_account_available(
            &cand.account_id,
            cand.window_hours,
            cand.quota_tokens,
            cand.quota_requests,
        )
        .unwrap_or(true);
    if !available {
        return Err(ProviderError::RateLimited {
            retry_after_ms: None,
            body: "todas as contas configuradas atingiram o limite ou estão em cooldown".into(),
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
                record_failover(
                    store,
                    session_id,
                    role_kind,
                    cand,
                    &err,
                    suggested_cooldown_ms(&err),
                    failed,
                );
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
            record_failover(
                store,
                session_id,
                role_kind,
                cand,
                &e,
                suggested_cooldown_ms(&e),
                failed,
            );
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
                record_failover(
                    store,
                    session_id,
                    role_kind,
                    cand,
                    &e2,
                    suggested_cooldown_ms(&e2),
                    failed,
                );
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
    client: &Client,
    ctx: TurnContext<'_>,
    agent: &AgentOptions<'_>,
    session_id: &str,
    step: usize,
    model: &str,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
    log: &TurnLog<'_>,
    failed: &mut Vec<Value>,
    pending_rtk_tokens: &mut u64,
) -> Result<(StepOut, GatewayOutcome<()>, StepSavings)> {
    let mut gateway = match ctx.settings {
        Some(st) => crate::resolve_gateway(st, model, client.config()).0,
        None => default_gateway(client, model),
    };
    gateway.final_retry = Some(client.retry_policy());

    let role_kind = if agent.subagents.depth > 0 {
        "subagent"
    } else {
        "main"
    };

    let (out, gw_outcome) = if gateway.candidates.len() == 1 {
        let cand = &gateway.candidates[0];
        request_single(
            store, &gateway, cand, session_id, role_kind, req, cancel, on_event, failed,
        )?
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
            |event| apply_stream_event(event, &mut out, &mut painted, on_event),
        )?;
        (out, gw_outcome)
    };

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
            }),
        );
    }

    let (prompt_tokens, completion_tokens) = out
        .usage
        .as_ref()
        .map(|u| (u.prompt_tokens, u.completion_tokens))
        .unwrap_or((0, 0));
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
        saved_rtk_tokens: step_rtk,
        saved_ponytail_tokens: step_ponytail,
        saved_caveman_tokens: step_caveman,
        saved_tokens: step_saved_tokens,
        status: "ok".to_string(),
        latency_ms: gw_outcome.latency_ms,
    });

    Ok((
        out,
        gw_outcome,
        StepSavings {
            ponytail: step_ponytail,
            caveman: step_caveman,
        },
    ))
}
