//! Subagents: the `agent` tool's engine, and the one place a child session is
//! run (the plan runner uses it too).
//!
//! A child is a fresh session (source `subagent`, `source_id` = the parent's
//! session id) with its own tool allowlist, step budget and model. It shares
//! nothing with the parent's conversation: **only its final text goes back**.
//!
//! Limits, all enforced here:
//! * a process-wide [`Gate`] caps concurrent children at the execution
//!   profile's `max_subagents`;
//! * depth is capped at [`MAX_DEPTH`] (a child may spawn, a grandchild may
//!   not). Only the first level takes a permit: a deeper child runs under its
//!   parent's, serially, so a full gate can never deadlock on itself;
//! * the parent's cancel token, and the call's deadline, reach the child
//!   through a watchdog; a panic or error becomes an error string, never a
//!   crash.
//!
//! Children share the parent's [`ApprovalGate`]: a call the policy would *ask*
//! about is queued on it and the root turn shows the prompt in the person's
//! chat; no gate (plan runner, bare CLI) still refuses. They also run every
//! tool through the parent's middleware, the same instances (hooks, rules,
//! the plan gate): a `PreToolUse` deny binds a child exactly as it binds the
//! parent. Parallel children call it at once, so a middleware that keeps
//! state across `before`/`after` must key it by `(session_id, call id)`, as
//! [`crate::guards::Guards`] does. The policy itself (mode, rules, workspace)
//! is the parent's, made stricter by a definition, never looser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use zerith_core::agents::{self, AgentDef};
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client, ProviderConfig, ProviderError};
use zerith_security::{Mode, ModeHandle, Policy};
use zerith_sessions::{Role, Store};
use zerith_tools::{AgentRequest, EventSink, HostEvent, Registry, Services, SkillLoad};

use crate::pipeline::{Flow, Middleware, ToolInvocation, ToolOutcome};
use crate::{ApprovalGate, RunError, RunEvent, TurnContext};

/// A turn's middleware as its subagents hold it: outlives the turn's borrow
/// (a background child may still be stopping when the turn returns).
pub(crate) type SharedMiddleware = Arc<dyn Middleware + Send + Sync>;

/// One shared middleware in a turn's own list.
struct Inherited(SharedMiddleware);

impl Middleware for Inherited {
    fn before(&self, call: &mut ToolInvocation) -> Flow {
        self.0.before(call)
    }
    fn after(&self, call: &ToolInvocation, result: &mut ToolOutcome) {
        self.0.after(call, result)
    }
}

fn boxed(shared: &[SharedMiddleware]) -> Vec<Box<dyn Middleware + Send + Sync>> {
    shared
        .iter()
        .map(|m| Box::new(Inherited(m.clone())) as Box<dyn Middleware + Send + Sync>)
        .collect()
}

/// Moves `mw` behind `Arc`s and leaves forwarders in its place: the turn
/// keeps using it, and its subagents get the very same instances.
pub(crate) fn share(mw: &mut Vec<Box<dyn Middleware + Send + Sync>>) -> Vec<SharedMiddleware> {
    let shared: Vec<SharedMiddleware> = std::mem::take(mw).into_iter().map(Arc::from).collect();
    *mw = boxed(&shared);
    shared
}

/// Deepest chain of subagents: root -> child -> grandchild.
pub const MAX_DEPTH: usize = 2;
/// Steps a child may take when nothing says otherwise.
pub const DEFAULT_CHILD_STEPS: usize = 20;
/// How long a background child may run.
const BACKGROUND_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Read-only tools: what `explore` and `plan` get.
pub const READ_ONLY_TOOLS: [&str; 4] = ["read_file", "list_dir", "glob", "grep"];

const COMMON: &str = "Você roda isolado: quem o chamou só vê a sua mensagem final, então termine com um resumo objetivo e completo do resultado. Você não pode fazer perguntas ao usuário.";

// --- the concurrency gate ---------------------------------------------------

/// A counting semaphore with cancel and deadline awareness.
#[derive(Default)]
pub struct Gate {
    used: Mutex<usize>,
    cv: Condvar,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The process-wide gate every host shares unless it brings its own.
    pub fn global() -> Arc<Self> {
        static G: OnceLock<Arc<Gate>> = OnceLock::new();
        G.get_or_init(Gate::new).clone()
    }

    /// Children running now.
    pub fn in_use(&self) -> usize {
        self.used.lock().map(|u| *u).unwrap_or(0)
    }

    /// Waits for a free slot (at most `limit` at once). Honours the cancel
    /// token and the deadline; the slot is released when the permit drops.
    pub fn acquire(
        self: &Arc<Self>,
        limit: usize,
        cancel: &Cancel,
        deadline: Option<Instant>,
    ) -> Result<Permit, String> {
        let limit = limit.max(1);
        let mut used = self
            .used
            .lock()
            .map_err(|_| "gate corrompido".to_string())?;
        loop {
            if cancel.is_cancelled() {
                return Err("cancelado".into());
            }
            if *used < limit {
                *used += 1;
                return Ok(Permit(self.clone()));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("tempo esgotado esperando uma vaga de subagente".into());
            }
            used = self
                .cv
                .wait_timeout(used, Duration::from_millis(50))
                .map_err(|_| "gate corrompido".to_string())?
                .0;
        }
    }
}

/// A held slot of a [`Gate`].
pub struct Permit(Arc<Gate>);

impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut u) = self.0.used.lock() {
            *u = u.saturating_sub(1);
        }
        self.0.cv.notify_all();
    }
}

/// How the turn's subagents behave. `Default` is a root turn on the global
/// gate with the profile's limit.
#[derive(Clone, Default)]
pub struct SubagentOptions {
    /// 0 for a normal turn; a child's turn runs at its own depth.
    pub depth: usize,
    /// `None`: [`Gate::global`].
    pub gate: Option<Arc<Gate>>,
    /// `None`: the execution profile's `max_subagents`.
    pub max_concurrent: Option<usize>,
}

// --- the watchdog ------------------------------------------------------------

/// Forwards a parent's cancel and a deadline into a child's own token. Dropping
/// it stops the thread. `timed_out` tells the two causes apart.
pub(crate) struct Watch {
    _stop: mpsc::Sender<()>,
    pub timed_out: Arc<AtomicBool>,
}

pub(crate) fn watch(parent: Cancel, child: Cancel, deadline: Option<Instant>) -> Watch {
    let (tx, rx) = mpsc::channel::<()>();
    let timed_out = Arc::new(AtomicBool::new(false));
    let flag = timed_out.clone();
    std::thread::spawn(move || loop {
        // ponytail: 50 ms slices because `Cancel` has no wakeup; add a condvar
        // to `Cancel` if this ever matters.
        let slice = Duration::from_millis(50);
        let wait = deadline.map_or(slice, |d| {
            d.saturating_duration_since(Instant::now()).min(slice)
        });
        match rx.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if parent.is_cancelled() {
                    child.cancel();
                    return;
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    flag.store(true, Ordering::SeqCst);
                    child.cancel();
                    return;
                }
            }
            _ => return,
        }
    });
    Watch {
        _stop: tx,
        timed_out,
    }
}

// --- agents and their tools ---------------------------------------------------

/// Which tools an agent may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allow {
    /// `read_file`, `list_dir`, `glob`, `grep`.
    ReadOnly,
    /// Everything the parent has.
    All,
    /// The definition's `tools` minus `disallowedTools`.
    Def,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub def: AgentDef,
    pub allow: Allow,
}

fn builtin(name: &str) -> Option<(&'static str, Allow)> {
    Some(match name {
        "explore" => (
            "Você é um subagente de exploração, somente leitura. Procure no código, leia o que for preciso e responda com achados concretos (caminhos e linhas). Não altere nada.",
            Allow::ReadOnly,
        ),
        "plan" => (
            "Você é um subagente de planejamento, somente leitura. Estude o código e devolva um plano de implementação passo a passo: arquivos a mudar, ordem e riscos. Não altere nada.",
            Allow::ReadOnly,
        ),
        "general" | "default" => (
            "Você é um subagente de uso geral. Execute a tarefa por completo com as ferramentas disponíveis.",
            Allow::All,
        ),
        _ => return None,
    })
}

fn synthesize(name: &str, text: &str) -> AgentDef {
    agents::parse(
        name.to_string(),
        &format!("{text}\n\n{COMMON}"),
        PathBuf::new(),
        "user",
    )
}

/// A defined agent (`.zerith/agents/<name>.md`, project over user) wins over
/// a built-in of the same name; no name means `general`. An unknown name is
/// an error that lists what exists.
pub fn resolve_agent(workspace: &Path, name: Option<&str>) -> Result<Resolved, String> {
    let name = name.unwrap_or("general");
    let paths = zerith_core::Paths::resolve();
    let defined = agents::list(&paths, Some(workspace));
    if let Some(mut def) = defined.iter().find(|a| a.name == name).cloned() {
        def.instructions = format!("{}\n\n{COMMON}", def.instructions);
        return Ok(Resolved {
            name: name.into(),
            def,
            allow: Allow::Def,
        });
    }
    if let Some((text, allow)) = builtin(name) {
        return Ok(Resolved {
            name: name.into(),
            def: synthesize(name, text),
            allow,
        });
    }
    let mut known: Vec<String> = ["explore", "plan", "general"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    known.extend(defined.into_iter().map(|a| a.name));
    Err(format!(
        "agente desconhecido: {name}; disponíveis: {}",
        known.join(", ")
    ))
}

/// The registry a child at `depth` (1-based) works with. `ask_user` never
/// goes down (nobody to ask), and a child at [`MAX_DEPTH`] cannot spawn.
pub fn child_registry(base: &Registry, r: &Resolved, depth: usize) -> Registry {
    let all: Vec<String> = base.specs().iter().map(|s| s.name.clone()).collect();
    let names: Vec<&str> = all.iter().map(String::as_str).collect();
    let allowed: Vec<String> = match r.allow {
        Allow::ReadOnly => READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect(),
        Allow::All => all.clone(),
        Allow::Def => r.def.effective_tools(&names),
    };
    let can_spawn = depth < MAX_DEPTH;
    let mut reg = base.subset(|n| {
        allowed.iter().any(|a| a == n)
            && n != "ask_user"
            // A child must never be able to mint more skills or agents: what it
            // writes would outlive it and be offered to the person as theirs.
            && !matches!(n, "create_skill" | "create_agent" | "delete_ai_item")
            && (can_spawn || !matches!(n, "agent" | "agent_result"))
    });
    if can_spawn {
        // The child holds a permit already: its own children run one at a time.
        reg.serialize_agents();
    }
    reg
}

// --- running one child ----------------------------------------------------------

pub struct ChildSpec {
    pub db: PathBuf,
    pub cfg: ProviderConfig,
    pub policy: Policy,
    pub registry: Registry,
    /// The parent's session id (or a plan id): stored as `source_id`.
    pub parent: String,
    pub title: String,
    pub prompt: String,
    pub model: String,
    pub def: Option<AgentDef>,
    /// The parent's skills, plugin folders included.
    pub skills_root: Option<zerith_skills::Library>,
    pub scope: Option<Scope>,
    pub max_steps: usize,
    /// The child's own depth (1 for a direct child).
    pub depth: usize,
    pub gate: Option<Arc<Gate>>,
    /// Pulled by whoever owns the child (parent turn, plan).
    pub cancel: Cancel,
    pub deadline: Option<Instant>,
    /// Receives events of the child's own subagents.
    pub sink: Option<EventSink>,
    pub settings: Option<zerith_core::Settings>,
    /// The parent's gate. `None`: a call that needs approval is refused.
    pub approvals: Option<ApprovalGate>,
    /// `(id, name)` under which the child reports its live progress (step and
    /// tool) to the person while it runs. `None`: silent until it ends.
    pub announce: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildError {
    Cancelled,
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChildError::Cancelled => f.write_str("cancelado"),
            ChildError::TimedOut => f.write_str("tempo esgotado"),
            ChildError::Failed(m) => f.write_str(m),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOutput {
    pub text: String,
    pub session_id: String,
    /// What the child turn (its own children included) saved.
    pub saved: Saved,
}

/// Token Saver totals of one turn, by source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Saved {
    pub rtk: u64,
    pub ponytail: u64,
    pub caveman: u64,
}

impl Saved {
    fn add(&mut self, o: Saved) {
        self.rtk += o.rtk;
        self.ponytail += o.ponytail;
        self.caveman += o.caveman;
    }
}

/// Savings of the children a turn started, summed as they finish. The turn
/// adds them to its report; the database keeps one row per request.
pub(crate) type SavedTotals = Arc<Mutex<Saved>>;

/// Runs one child to completion on the calling thread, with no middleware
/// (the plan runner has none to hand down).
pub fn run_child(spec: ChildSpec) -> Result<ChildOutput, ChildError> {
    run_child_with(spec, &[])
}

/// [`run_child`] with the parent's middleware around every tool call.
pub(crate) fn run_child_with(
    spec: ChildSpec,
    middleware: &[SharedMiddleware],
) -> Result<ChildOutput, ChildError> {
    let fail = |e: &dyn std::fmt::Display| ChildError::Failed(e.to_string());
    let store = Store::open(&spec.db).map_err(|e| fail(&e))?;
    let session = store
        .create_subagent_session(&spec.parent, &spec.title, Some(&spec.model))
        .map_err(|e| fail(&e))?;
    let client = Client::new(spec.cfg.clone()).map_err(|e| fail(&e))?;
    let memory = spec
        .scope
        .as_ref()
        .and_then(|_| SqliteMemory::open(&spec.db).ok());
    let skills = spec.skills_root.clone();

    let cancel = Cancel::default();
    // Every child ends: with no deadline from the caller (plan runner without
    // a time budget) it gets the background limit.
    let deadline = spec
        .deadline
        .unwrap_or_else(|| Instant::now() + BACKGROUND_LIMIT);
    let watchdog = watch(spec.cancel.clone(), cancel.clone(), Some(deadline));
    let sink = spec.sink.clone();
    let opts = crate::AgentOptions {
        tools: Some(&spec.registry),
        policy: Some(&spec.policy),
        approvals: spec.approvals.as_ref(),
        middleware: boxed(middleware),
        max_steps: spec.max_steps.max(1),
        subagents: SubagentOptions {
            depth: spec.depth,
            gate: spec.gate.clone(),
            max_concurrent: None,
        },
        ..crate::AgentOptions::default()
    };
    // `..default()` stays so a field added to `TurnContext` later is not a
    // compile error here.
    #[allow(clippy::needless_update)]
    let ctx = TurnContext {
        memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
        scope: spec.scope.as_ref(),
        skills: skills.as_ref(),
        agent: spec.def.as_ref(),
        settings: spec.settings.as_ref(),
        ..TurnContext::default()
    };
    let mut step = 0usize;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::run_agent_turn(
            &store,
            &client,
            ctx,
            opts,
            &session.id,
            &spec.model,
            &spec.prompt,
            &cancel,
            |e| {
                // Live progress: what step the child is on and what it is
                // running, while it runs. The shared gate is pumped by the
                // root turn during the `agent` call; the sink only between
                // tool groups.
                let progress = match (&e, &spec.announce) {
                    (RunEvent::Step { n }, Some(_)) => {
                        step = *n;
                        Some(format!("passo {n} de {}", spec.max_steps))
                    }
                    (RunEvent::ToolCall { name, .. }, Some(_)) => {
                        Some(format!("passo {step} de {} · {name}", spec.max_steps))
                    }
                    _ => None,
                };
                if let (Some(summary), Some((id, name))) = (progress, &spec.announce) {
                    let ev = RunEvent::Subagent {
                        id: id.clone(),
                        name: name.clone(),
                        state: "running".into(),
                        summary,
                        model: spec.model.clone(),
                    };
                    match (&spec.approvals, &sink) {
                        (Some(g), _) => g.relay(ev),
                        (None, Some(s)) => {
                            if let RunEvent::Subagent {
                                id,
                                name,
                                state,
                                summary,
                                model,
                            } = ev
                            {
                                s.push(HostEvent::Subagent {
                                    id,
                                    name,
                                    state,
                                    summary,
                                    model,
                                });
                            }
                        }
                        (None, None) => {}
                    }
                }
                if let (
                    RunEvent::Subagent {
                        id,
                        name,
                        state,
                        summary,
                        model,
                    },
                    Some(s),
                ) = (e, &sink)
                {
                    s.push(HostEvent::Subagent {
                        id,
                        name,
                        state,
                        summary,
                        model,
                    });
                }
            },
        )
    }));
    let timed_out = watchdog.timed_out.load(Ordering::SeqCst);
    drop(watchdog);
    match run {
        Err(_) => Err(ChildError::Failed("o subagente entrou em pânico".into())),
        Ok(Err(RunError::Provider(ProviderError::Cancelled))) => Err(if timed_out {
            ChildError::TimedOut
        } else {
            ChildError::Cancelled
        }),
        Ok(Err(e)) => Err(fail(&e)),
        Ok(Ok(report)) => {
            if report.text.starts_with("[parado: limite") {
                // The note is not an answer: hand back what the child did say.
                let partial = last_real_text(&store, &session.id).unwrap_or_default();
                return Err(ChildError::Failed(format!(
                    "o subagente atingiu o limite de {} passos sem concluir{}",
                    spec.max_steps,
                    if partial.is_empty() {
                        String::new()
                    } else {
                        format!("; último texto: {partial}")
                    }
                )));
            }
            Ok(ChildOutput {
                text: report.text,
                session_id: session.id,
                saved: Saved {
                    rtk: report.saved_rtk_tokens,
                    ponytail: report.saved_ponytail_tokens,
                    caveman: report.saved_caveman_tokens,
                },
            })
        }
    }
}

fn last_real_text(store: &Store, session: &str) -> Option<String> {
    store
        .messages(session, 10_000, 0)
        .ok()?
        .into_iter()
        .rev()
        .find(|m| {
            m.role == Role::Assistant
                && !m.content.trim().is_empty()
                && !m.content.starts_with("[parado:")
        })
        .map(|m| m.content)
}

// --- the service the `agent` tool calls -----------------------------------------

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

enum Bg {
    Running,
    Done(String),
    Failed(String),
}

struct BgSlot {
    parent: String,
    name: String,
    /// The child's own token: the parent turn ending (or stopping) pulls it.
    cancel: Cancel,
    state: Mutex<Bg>,
}

impl BgSlot {
    fn result(&self) -> Option<Result<String, String>> {
        match &*self.state.lock().ok()? {
            Bg::Running => None,
            Bg::Done(t) => Some(Ok(t.clone())),
            Bg::Failed(e) => Some(Err(e.clone())),
        }
    }
}

/// What a background child left for its turn: id, display name, outcome.
pub(crate) type Joined = (String, String, Result<String, String>);

/// Waits for the background children of `session` its model never collected.
/// Called when the turn would end, so no child outlives its turn unseen: the
/// turn hands their results to the model and carries on. `pump` shows their
/// live progress and approval prompts meanwhile. A stop of the turn stops
/// them too; their own deadline bounds the wait otherwise.
pub(crate) fn join_background(
    session: &str,
    cancel: &Cancel,
    mut pump: impl FnMut(),
) -> Vec<Joined> {
    let mine: Vec<(String, Arc<BgSlot>)> = background()
        .lock()
        .map(|m| {
            m.iter()
                .filter(|(_, s)| s.parent == session)
                .map(|(k, s)| (k.clone(), s.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    for (id, slot) in mine {
        let r = loop {
            if let Some(r) = slot.result() {
                break r;
            }
            if cancel.is_cancelled() {
                return out;
            }
            pump();
            std::thread::sleep(Duration::from_millis(100));
        };
        if let Ok(mut m) = background().lock() {
            m.remove(&id);
        }
        out.push((id, slot.name.clone(), r));
    }
    pump();
    out
}

/// Stops and forgets every background child `session` still has. The turn
/// calls it on its way out (error, stop, step limit): returns who was
/// stopped, so the person is told instead of left with a child that never
/// reports.
pub(crate) fn abandon(session: &str) -> Vec<(String, String)> {
    let Ok(mut m) = background().lock() else {
        return Vec::new();
    };
    let gone: Vec<String> = m
        .iter()
        .filter(|(_, s)| s.parent == session)
        .map(|(k, _)| k.clone())
        .collect();
    gone.into_iter()
        .filter_map(|id| m.remove(&id).map(|s| (id, s)))
        .map(|(id, s)| {
            s.cancel.cancel();
            (id, s.name.clone())
        })
        .collect()
}

fn background() -> &'static Mutex<HashMap<String, Arc<BgSlot>>> {
    static M: OnceLock<Mutex<HashMap<String, Arc<BgSlot>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// What a turn binds into its registry's host.
struct Runtime {
    db: Option<PathBuf>,
    cfg: ProviderConfig,
    policy: Policy,
    base: Registry,
    skills_root: Option<zerith_skills::Library>,
    model: String,
    depth: usize,
    gate: Arc<Gate>,
    limit: Option<usize>,
    settings: Option<zerith_core::Settings>,
    approvals: Option<ApprovalGate>,
    middleware: Vec<SharedMiddleware>,
    saved: SavedTotals,
}

/// Binds the turn's session to its registry so the session tools work. Called
/// by the loop once per turn. Returns where the turn's children add up what
/// they saved.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bind(
    tools: &Registry,
    policy: &Policy,
    store: &Store,
    client: &Client,
    ctx: &TurnContext<'_>,
    model: &str,
    session_id: &str,
    opts: &SubagentOptions,
    approvals: Option<&ApprovalGate>,
    middleware: &[SharedMiddleware],
) -> SavedTotals {
    let db = store
        .conn()
        .path()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let rt = Runtime {
        db,
        cfg: client.config().clone(),
        policy: policy.clone(),
        base: tools.subset(|_| true),
        skills_root: ctx.skills.cloned(),
        model: model.to_string(),
        depth: opts.depth,
        gate: opts.gate.clone().unwrap_or_else(Gate::global),
        limit: opts.max_concurrent,
        settings: ctx.settings.cloned(),
        approvals: approvals.cloned(),
        middleware: middleware.to_vec(),
        saved: SavedTotals::default(),
    };
    let saved = rt.saved.clone();
    tools.host().bind(session_id, Arc::new(rt));
    saved
}

pub(crate) fn pick_candidate<'a>(
    db: &Path,
    candidates: &'a [zerith_providers::RouteCandidate],
) -> Option<&'a zerith_providers::RouteCandidate> {
    if let Ok(store) = Store::open(db) {
        if let Some(cand) = candidates.iter().find(|c| {
            store
                .is_account_available(
                    &c.account_id,
                    c.window_hours,
                    c.quota_tokens,
                    c.quota_requests,
                )
                .unwrap_or(true)
        }) {
            return Some(cand);
        }
    }
    candidates.first()
}

pub(crate) fn resolve_child_route(
    db: &Path,
    settings: &zerith_core::Settings,
    raw_model: &str,
    fallback_cfg: &ProviderConfig,
) -> (ProviderConfig, String) {
    let (gw, effective_model) = crate::resolve_gateway(settings, raw_model, fallback_cfg);
    match pick_candidate(db, &gw.candidates) {
        Some(cand) => (cand.config.clone(), cand.model.clone()),
        None => (fallback_cfg.clone(), effective_model),
    }
}

/// Turns what the tools queued into events: subagent states as they came, and
/// one `Tasks` snapshot if the list changed. Called by the loop between steps
/// and after each tool group.
pub(crate) fn flush(
    store: &Store,
    session_id: &str,
    tools: Option<&Registry>,
    on_event: &mut dyn FnMut(RunEvent),
) {
    let Some(tools) = tools else { return };
    let mut tasks = false;
    for e in tools.host().take_events() {
        match e {
            HostEvent::Subagent {
                id,
                name,
                state,
                summary,
                model,
            } => on_event(RunEvent::Subagent {
                id,
                name,
                state,
                summary,
                model,
            }),
            HostEvent::Tasks => tasks = true,
        }
    }
    if tasks {
        if let Ok(items) = store.todo_list(session_id) {
            on_event(RunEvent::Tasks { items });
        }
    }
}

fn profile_limit() -> usize {
    crate::active_profile().limits().max_subagents
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(200).collect()
}

impl Runtime {
    fn db(&self) -> Result<&PathBuf, String> {
        self.db
            .as_ref()
            .ok_or_else(|| "indisponível: o banco desta sessão é em memória".to_string())
    }
}

impl Services for Runtime {
    fn agent(
        &self,
        session: &str,
        req: AgentRequest,
        cancel: &Cancel,
        deadline: Instant,
        sink: EventSink,
    ) -> Result<String, String> {
        let db = self.db()?.clone();
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(format!(
                "profundidade máxima de {MAX_DEPTH} níveis de subagente atingida"
            ));
        }
        let resolved = resolve_agent(&self.policy.workspace, req.agent.as_deref())?;
        let registry = child_registry(&self.base, &resolved, depth);
        let mut policy = self.policy.clone();
        if resolved
            .def
            .permission_mode
            .as_deref()
            .and_then(Mode::parse)
            .is_some_and(|m| m == Mode::Plan)
        {
            // A definition may only make a child stricter, never looser. Its
            // own handle: the parent's live one would win over `mode`.
            policy = policy.with_handle(ModeHandle::new(Mode::Plan));
        }
        let id = format!("sub-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let settings = self.settings.clone().unwrap_or_else(|| {
            zerith_core::Settings::load(
                &zerith_core::Paths::resolve(),
                Some(&self.policy.workspace),
            )
            .settings
        });
        let raw_model = settings.routing.resolve_subagent_model(
            &resolved.name,
            req.model.as_deref(),
            resolved.def.model.as_deref(),
            &self.model,
        );
        let (cfg, model) = resolve_child_route(&db, &settings, &raw_model, &self.cfg);
        let steps = resolved
            .def
            .max_turns
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_CHILD_STEPS);
        let name = resolved.name.clone();
        let description = req.description.clone();
        let bg_cancel = Cancel::default();
        let spec = ChildSpec {
            db,
            cfg,
            policy,
            registry,
            parent: session.to_string(),
            title: format!("subagente: {}", req.description),
            prompt: req.prompt.clone(),
            model,
            def: Some(resolved.def),
            skills_root: self.skills_root.clone(),
            scope: None,
            max_steps: steps,
            depth,
            gate: Some(self.gate.clone()),
            // A background child outlives the call: its own token, pulled by
            // the turn that started it when that turn ends or stops.
            cancel: if req.background {
                bg_cancel.clone()
            } else {
                cancel.clone()
            },
            deadline: Some(if req.background {
                Instant::now() + BACKGROUND_LIMIT
            } else {
                deadline
            }),
            sink: Some(sink.clone()),
            settings: Some(settings),
            approvals: self.approvals.clone(),
            announce: Some((id.clone(), format!("{name}: {description}"))),
        };
        let totals = self.saved.clone();
        let middleware = self.middleware.clone();
        // Only the first level takes a permit; deeper children run under it.
        let permit_gate = (self.depth == 0).then(|| (self.gate.clone(), self.limit));
        let limit = self.limit.unwrap_or_else(profile_limit);
        let announce = {
            let sink = sink.clone();
            let (id, name, description) = (id.clone(), name.clone(), description.clone());
            let child_model = spec.model.clone();
            move |state: &str, summary: &str| {
                sink.push(HostEvent::Subagent {
                    id: id.clone(),
                    name: format!("{name}: {description}"),
                    state: state.into(),
                    summary: one_line(summary),
                    model: child_model.clone(),
                });
            }
        };
        let work = move || -> Result<String, String> {
            // The permit is held for the whole child, and never for a plain
            // wait: acquiring honours cancel and the call's deadline.
            let _permit = match permit_gate {
                Some((gate, _)) => Some(gate.acquire(limit, &spec.cancel, spec.deadline)?),
                None => None,
            };
            announce("running", &spec.prompt);
            match run_child_with(spec, &middleware) {
                Ok(o) => {
                    if let Ok(mut t) = totals.lock() {
                        t.add(o.saved);
                    }
                    announce("done", &o.text);
                    Ok(o.text)
                }
                Err(e) => {
                    let state = if e == ChildError::Cancelled {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    announce(state, &e.to_string());
                    Err(format!("subagente falhou: {e}"))
                }
            }
        };
        if !req.background {
            return work();
        }
        let slot = Arc::new(BgSlot {
            parent: session.to_string(),
            name: format!("{name}: {description}"),
            cancel: bg_cancel,
            state: Mutex::new(Bg::Running),
        });
        background()
            .lock()
            .map_err(|_| "registro corrompido".to_string())?
            .insert(id.clone(), slot.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("o subagente entrou em pânico".into()));
            if let Ok(mut st) = slot.state.lock() {
                *st = match r {
                    Ok(t) => Bg::Done(t),
                    Err(e) => Bg::Failed(e),
                };
            }
        });
        Ok(format!(
            "{id} iniciado em segundo plano; busque o resultado com agent_result"
        ))
    }

    fn agent_result(&self, session: &str, id: &str) -> Result<String, String> {
        let slot = background()
            .lock()
            .map_err(|_| "registro corrompido".to_string())?
            .get(id)
            .cloned()
            .filter(|s| s.parent == session)
            .ok_or_else(|| format!("subagente desconhecido: {id}"))?;
        let Some(r) = slot.result() else {
            return Ok(format!("{id}: running"));
        };
        // Collected: the turn's end has nothing left to hand over for it.
        if let Ok(mut m) = background().lock() {
            m.remove(id);
        }
        r
    }

    fn tasks(&self, session: &str, op: &str, args: &Value) -> Result<String, String> {
        crate::tasks::run(self.db()?, session, op, args)
    }

    fn skill(&self, name: &str) -> Result<SkillLoad, String> {
        let lib = self
            .skills_root
            .as_ref()
            .ok_or_else(|| "nenhuma biblioteca de skills neste turno".to_string())?;
        let body = lib
            .skill_body(name)
            .ok_or_else(|| format!("skill não encontrada ou desativada: {name}"))?;
        Ok(SkillLoad {
            body,
            allowed_tools: lib.allowed_tools(name).ok().flatten(),
        })
    }

    fn skills_root(&self) -> Option<zerith_skills::Library> {
        self.skills_root.clone()
    }
}
