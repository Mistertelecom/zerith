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
//! Children get no approval gate (the parent's is borrowed for one turn) and
//! none of the host's middleware: a call the policy would *ask* about is
//! refused. The policy itself (mode, rules, workspace) is the parent's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use zerith_core::agents::{self, AgentDef};
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client, ProviderConfig, ProviderError};
use zerith_security::{Mode, Policy};
use zerith_sessions::{Role, Store};
use zerith_tools::{AgentRequest, EventSink, HostEvent, Registry, Services, SkillLoad};

use crate::{RunError, RunEvent, TurnContext};

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
    pub skills_root: Option<PathBuf>,
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
}

/// Runs one child to completion on the calling thread.
pub fn run_child(spec: ChildSpec) -> Result<ChildOutput, ChildError> {
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
    let skills = spec
        .skills_root
        .as_ref()
        .and_then(|r| zerith_skills::Library::open(r).ok());

    let cancel = Cancel::default();
    let watchdog = watch(spec.cancel.clone(), cancel.clone(), spec.deadline);
    let sink = spec.sink.clone();
    let opts = crate::AgentOptions {
        tools: Some(&spec.registry),
        policy: Some(&spec.policy),
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
                if let (
                    RunEvent::Subagent {
                        id,
                        name,
                        state,
                        summary,
                    },
                    Some(s),
                ) = (e, &sink)
                {
                    s.push(HostEvent::Subagent {
                        id,
                        name,
                        state,
                        summary,
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
    state: Mutex<Bg>,
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
    skills_root: Option<PathBuf>,
    model: String,
    depth: usize,
    gate: Arc<Gate>,
    limit: Option<usize>,
    settings: Option<zerith_core::Settings>,
}

/// Binds the turn's session to its registry so the session tools work. Called
/// by the loop once per turn.
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
) {
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
        skills_root: ctx.skills.map(|l| l.root().to_path_buf()),
        model: model.to_string(),
        depth: opts.depth,
        gate: opts.gate.clone().unwrap_or_else(Gate::global),
        limit: opts.max_concurrent,
        settings: ctx.settings.cloned(),
    };
    tools.host().bind(session_id, Arc::new(rt));
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
            } => on_event(RunEvent::Subagent {
                id,
                name,
                state,
                summary,
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
            // A definition may only make a child stricter, never looser.
            policy.mode = Mode::Plan;
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
            cancel: cancel.clone(),
            deadline: Some(if req.background {
                Instant::now() + BACKGROUND_LIMIT
            } else {
                deadline
            }),
            sink: Some(sink.clone()),
            settings: Some(settings),
        };
        // Only the first level takes a permit; deeper children run under it.
        let permit_gate = (self.depth == 0).then(|| (self.gate.clone(), self.limit));
        let limit = self.limit.unwrap_or_else(profile_limit);
        let announce = {
            let sink = sink.clone();
            let (id, name, description) = (id.clone(), name.clone(), description.clone());
            move |state: &str, summary: &str| {
                sink.push(HostEvent::Subagent {
                    id: id.clone(),
                    name: format!("{name}: {description}"),
                    state: state.into(),
                    summary: one_line(summary),
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
            match run_child(spec) {
                Ok(o) => {
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
        let st = slot
            .state
            .lock()
            .map_err(|_| "estado corrompido".to_string())?;
        match &*st {
            Bg::Running => Ok(format!("{id}: running")),
            Bg::Done(t) => Ok(t.clone()),
            Bg::Failed(e) => Err(e.clone()),
        }
    }

    fn tasks(&self, session: &str, op: &str, args: &Value) -> Result<String, String> {
        crate::tasks::run(self.db()?, session, op, args)
    }

    fn skill(&self, name: &str) -> Result<SkillLoad, String> {
        let root = self
            .skills_root
            .as_ref()
            .ok_or_else(|| "nenhuma biblioteca de skills neste turno".to_string())?;
        let lib = zerith_skills::Library::open(root).map_err(|e| e.to_string())?;
        let body = lib
            .skill_body(name)
            .ok_or_else(|| format!("skill não encontrada ou desativada: {name}"))?;
        Ok(SkillLoad {
            body,
            allowed_tools: lib.allowed_tools(name).ok().flatten(),
        })
    }
}
