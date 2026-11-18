//! One agent turn, from user text to a stored assistant message.
//!
//! The whole harness lives here rather than in the CLI, because the same turn
//! has to run from the shell, from the Rune window and from a future RPC host.
//!
//! Two rules, both from the spec:
//!
//! 1. **A message the user saw is committed.** The user message is appended
//!    before the request goes out, so a crash mid-turn loses the answer, never
//!    the question.
//! 2. **Streaming does not write per delta.** Tokens are shown as they arrive
//!    and the assistant message is written once, whole, when the turn closes.
//!
//! The tool loop is in [`agent`]; the plain turn is that loop with tools off.

pub mod agent;
pub mod compact;
pub mod events;
pub mod guards;
pub mod pipeline;
pub mod plan;
pub mod question;
pub mod spill;
pub mod subagent;
mod tasks;
pub mod tokensaver;

pub use agent::{
    approval_channel, inbox_channel, run_agent_turn, AgentOptions, Approval, ApprovalGate,
    ApprovalSender, InboxReceiver, InboxSender, DEFAULT_MAX_PARALLEL_TOOLS, DEFAULT_MAX_STEPS,
};
pub use compact::{compact_now, CompactReport, Summarizer};
pub use events::{cancel_with, CancelCause, CauseSlot};
pub use pipeline::{Flow, Middleware, NoopMiddleware, ToolInvocation, ToolOutcome};
pub use question::{question_channel, QuestionGate, QuestionSender};
pub use subagent::{Gate, SubagentOptions};

use std::collections::HashSet;
use std::sync::mpsc;
use zerith_core::{secrets, settings::WireProtocol, AccountEntry, Settings};
use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{
    infer_provider, Client, Gateway, ModelSpec, Protocol, ProviderConfig, ProviderError,
    ProviderKind, RouteCandidate,
};
use zerith_sessions::Store;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("no such session: {0}")]
    NoSession(String),
    #[error("blocked by hook: {0}")]
    Blocked(String),
}

pub type Result<T> = std::result::Result<T, RunError>;

/// Where the turn's output goes while it is being produced.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Delta(String),
    Reasoning(String),
    Done {
        stored: Option<i64>,
    },
    /// A model request is about to go out; `n` starts at 1.
    Step {
        n: usize,
    },
    /// The model asked for a tool. `args` is the raw JSON the model wrote.
    ToolCall {
        id: String,
        name: String,
        args: String,
    },
    /// The call finished (or was refused); the full output is in the store.
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        output_preview: String,
    },
    /// The turn is blocked until `ApprovalSender::answer(id, ..)` is called.
    ApprovalNeeded {
        id: u64,
        tool: String,
        summary: String,
    },
    /// Token counts of one model request, as the provider reported them.
    Usage {
        input: u64,
        output: u64,
    },
    /// The history was summarized to fit the context window (token estimates).
    Compacted {
        before: usize,
        after: usize,
    },
    /// The session's task list changed; `items` is the whole live list (the
    /// UI replaces its checklist, it does not merge).
    Tasks {
        items: Vec<zerith_sessions::Todo>,
    },
    /// A subagent started, finished or failed. `state` is `running`, `done`,
    /// `failed` or `cancelled`; `summary` is one line (the task, or the result).
    Subagent {
        id: String,
        name: String,
        state: String,
        summary: String,
    },
    /// The turn is blocked until `QuestionSender::answer(id, ..)` is called.
    Question {
        id: u64,
        questions: Vec<zerith_tools::Question>,
    },
    /// The local gateway chose an account/provider/model route for a step.
    Route {
        account_id: String,
        provider: String,
        model: String,
        attempts: usize,
    },
    /// Token Saver RTK compressed a tool output before sending it to the model.
    TokenSaved {
        tool: String,
        before_bytes: usize,
        after_bytes: usize,
        filter: Option<String>,
    },
}

/// How many past messages are replayed to the model, newest first, when no
/// profile says otherwise (`AgentOptions::history`). Further trimming to the
/// context window happens in `agent::fit_context`.
pub const HISTORY_LIMIT: usize = 40;

/// How much of the prompt may be spent on recalled facts. A rough 4 chars per
/// token, so the number stays predictable without a tokenizer dependency.
pub const MEMORY_BUDGET_CHARS: usize = 2000;

/// How many facts one turn asks the backend for. Recall is a ranking, not a
/// dump; the budget trims what the ranking offered.
pub const MEMORY_HITS: usize = 12;

/// How much of the prompt skills may occupy. Metadata for many, bodies for
/// the few the user named.
pub const SKILLS_BUDGET_CHARS: usize = 3000;

/// Everything a turn may consult besides the conversation itself.
///
/// Grouped rather than passed one by one: the turn already took seven
/// arguments, and each new capability (memory, skills, later tools) would add
/// another.
#[derive(Default, Clone, Copy)]
pub struct TurnContext<'a> {
    pub memory: Option<&'a dyn MemoryBackend>,
    pub scope: Option<&'a Scope>,
    pub skills: Option<&'a zerith_skills::Library>,
    /// The agent this turn runs as. `None` is the default agent; a named one
    /// adds its instructions after the project's.
    pub agent: Option<&'a zerith_core::agents::AgentDef>,
    pub settings: Option<&'a zerith_core::Settings>,
    /// Where to look for `AGENTS.md` / `CLAUDE.md`. `None` uses the process
    /// working directory, which is what a person running the app wants. A
    /// host that runs turns for somewhere else (and the tests, whose working
    /// directory is the repository) names the folder, or turns the lookup off
    /// with [`TurnContext::no_project_instructions`].
    pub project_dir: Option<&'a std::path::Path>,
    /// Do not load project instructions at all.
    pub no_project_instructions: bool,
}

/// The turn's outcome, with the numbers a UI needs to show it honestly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnReport {
    pub session_id: String,
    pub message_id: i64,
    pub text: String,
    pub reasoning: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub saved_rtk_tokens: u64,
    pub saved_ponytail_tokens: u64,
    pub saved_caveman_tokens: u64,
    pub saved_tokens: u64,
    pub account_id: Option<String>,
    pub model_used: Option<String>,
}

/// Runs one turn and appends the answer.
///
/// `on_event` receives deltas as they arrive; it is also the only chance to see
/// partial output, since nothing half-written is persisted.
pub fn run_turn(
    store: &Store,
    client: &Client,
    session_id: &str,
    model: &str,
    prompt: &str,
    on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    run_turn_with_memory(
        store, client, None, None, session_id, model, prompt, on_event,
    )
}

/// The same turn, with durable memory attached.
///
/// Recall happens once per turn, against the user's text, and the result lands
/// *after* the static system prompt: the provider's prefix cache stays warm
/// while the facts change.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with_memory(
    store: &Store,
    client: &Client,
    memory: Option<&dyn MemoryBackend>,
    scope: Option<&Scope>,
    session_id: &str,
    model: &str,
    prompt: &str,
    on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    let cancel = zerith_providers::Cancel::default();
    run_turn_cancellable(
        store, client, memory, scope, session_id, model, prompt, &cancel, on_event,
    )
}

/// The same turn, with a cancel token the host can pull from another thread —
/// a window needs a stop button, and the only honest stop is the one that
/// reaches the network loop.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_cancellable(
    store: &Store,
    client: &Client,
    memory: Option<&dyn MemoryBackend>,
    scope: Option<&Scope>,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &zerith_providers::Cancel,
    on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    run_turn_in(
        store,
        client,
        TurnContext {
            memory,
            scope,
            ..TurnContext::default()
        },
        session_id,
        model,
        prompt,
        cancel,
        on_event,
    )
}

/// The execution profile in the merged settings (user, project, env). Read
/// per turn so a change in the window applies to the next turn, CLI included.
pub fn active_profile() -> zerith_core::settings::Profile {
    let cwd = std::env::current_dir().ok();
    zerith_core::Settings::load(&zerith_core::Paths::resolve(), cwd.as_deref())
        .settings
        .profile
}

/// The turn itself, without tools: one request, one stored answer. Every
/// entry point above funnels here; it is `run_agent_turn` with tools off, so
/// there is exactly one place that decides what the model sees.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_in(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &zerith_providers::Cancel,
    on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    run_agent_turn(
        store,
        client,
        ctx,
        AgentOptions::for_profile(active_profile()),
        session_id,
        model,
        prompt,
        cancel,
        on_event,
    )
}

/// The system prompt: identity, then recalled facts, then skills. Facts land
/// *after* the static part so the provider's prefix cache stays warm.
///
/// `skill_tool` says whether the model has the `skill` tool this turn. With it
/// the prompt lists skills by name and description and the model loads a body
/// on demand; without it (a plain turn) a skill named in the request is
/// inlined, the only way a tool-less turn can use one.
pub(crate) fn system_prompt(
    title: &str,
    ctx: TurnContext<'_>,
    skill_tool: bool,
    session_id: &str,
    prompt: &str,
) -> String {
    // Project instructions (AGENTS.md / CLAUDE.md) from the working directory
    // up to the git root, plus the user-level file. Static per project, so it
    // sits with the identity and keeps the prefix cache warm.
    let paths = zerith_core::Paths::resolve();
    let dir = ctx
        .project_dir
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let mut prompt_text = match dir {
        Some(cwd) if !ctx.no_project_instructions => zerith_core::system_prompt_with_instructions(
            title,
            &zerith_core::load_instructions(&paths, &cwd),
        ),
        _ => zerith_core::system_prompt(title),
    };
    if let Some(a) = ctx.agent {
        prompt_text.push_str(&format!("\n\n# Agente: {}\n\n{}", a.name, a.instructions));
    }
    if let Some(st) = ctx.settings {
        if let Some(pt) = tokensaver::ponytail_prompt(st.token_saver.ponytail) {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(pt);
        }
        if let Some(cv) = tokensaver::caveman_prompt(st.token_saver.caveman) {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(cv);
        }
    }
    if let (Some(mem), Some(scope)) = (ctx.memory, ctx.scope) {
        // A fact the user stated is stored before the answer is asked for, so
        // the same turn can already recall it — and a crash after this point
        // still keeps what the user said.
        if let Some(f) = zerith_memory::extract_allowed(mem, session_id, prompt) {
            let _ = mem.remember(scope, f.kind, &f.text, f.confidence, Some(session_id));
        }
        let known = recall_block(mem, scope, prompt);
        if !known.is_empty() {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(&known);
        }
    }
    if let Some(lib) = ctx.skills {
        let block = if skill_tool {
            skills_index(lib, SKILLS_BUDGET_CHARS)
        } else {
            skills_block(lib, prompt, SKILLS_BUDGET_CHARS)
        };
        if !block.is_empty() {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(&block);
        }
    }
    prompt_text
}

/// A helper for hosts that drive a turn on a worker thread and read events on
/// the UI thread: a turn must never block a frame.
/// Renders the recalled facts as a prompt section, trimmed to the budget.
///
/// A failed recall is not a failed turn: memory is an aid, and losing it must
/// degrade the answer, not block it.
pub(crate) fn recall_block(memory: &dyn MemoryBackend, scope: &Scope, query: &str) -> String {
    let Ok(hits) = memory.recall(scope, query, MEMORY_HITS) else {
        return String::new();
    };
    let mut block = String::from("# O que você já sabe sobre o usuário\n");
    let mut used = block.len();
    let mut any = false;
    for hit in hits {
        if hit.fact.superseded_by.is_some() {
            continue;
        }
        let line = format!(
            "- [{}] {}\n",
            hit.fact.confidence.as_str(),
            hit.fact.text.trim()
        );
        if used + line.len() > MEMORY_BUDGET_CHARS {
            break;
        }
        used += line.len();
        block.push_str(&line);
        any = true;
    }
    if any {
        block
    } else {
        String::new()
    }
}

pub fn run_turn_async(
    store: Store,
    client: Client,
    session_id: String,
    model: String,
    prompt: String,
) -> (
    mpsc::Receiver<RunEvent>,
    std::thread::JoinHandle<Result<TurnReport>>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        run_turn(&store, &client, &session_id, &model, &prompt, |e| {
            let _ = tx.send(e);
        })
    });
    (rx, handle)
}

fn map_protocol(p: WireProtocol) -> Protocol {
    match p {
        WireProtocol::OpenAi => Protocol::OpenAi,
        WireProtocol::Anthropic => Protocol::Anthropic,
    }
}

fn canonical_provider(raw: &str) -> String {
    let kind = ProviderKind::parse(raw);
    if kind == ProviderKind::Custom {
        raw.trim().to_string()
    } else {
        kind.as_str().to_string()
    }
}

fn same_provider(provider: &str, cfg: &ProviderConfig) -> bool {
    if provider.eq_ignore_ascii_case(&cfg.name) {
        return true;
    }
    let kind = ProviderKind::parse(provider);
    kind != ProviderKind::Custom && kind == cfg.kind
}

fn compatible_provider(
    acc_provider: &str,
    target_provider: Option<&str>,
    fallback_cfg: &ProviderConfig,
) -> bool {
    match target_provider {
        Some(tp) => {
            if acc_provider.eq_ignore_ascii_case(tp) {
                return true;
            }
            let tk = ProviderKind::parse(tp);
            let ak = ProviderKind::parse(acc_provider);
            tk != ProviderKind::Custom && tk == ak
        }
        None => same_provider(acc_provider, fallback_cfg),
    }
}

/// Keeps the last account entry per non-empty `id` so project/local layers
/// override user-layer accounts with the same id.
fn effective_accounts(settings: &Settings) -> Vec<&AccountEntry> {
    let mut seen = HashSet::new();
    let mut rev = Vec::new();
    for acc in settings.accounts.iter().rev() {
        if !acc.id.is_empty() && seen.insert(acc.id.as_str()) {
            rev.push(acc);
        }
    }
    rev.reverse();
    rev
}

fn resolve_account_key(
    acc: &AccountEntry,
    base_url: &str,
    fallback_cfg: &ProviderConfig,
    store: &dyn zerith_core::secrets::SecretStore,
) -> Option<String> {
    let key_ref = acc.api_key_ref.as_deref().or(Some(acc.id.as_str()));
    let read = |r: Option<&str>| {
        zerith_core::secrets::resolve_api_key_with(
            store,
            |k| std::env::var(k).ok(),
            &acc.provider,
            r,
        )
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
    };
    if let Some(k) = read(key_ref) {
        return Some(k);
    }
    if acc.api_key_ref.is_none() {
        if let Some(k) = read(None) {
            return Some(k);
        }
        let same_url = !base_url.trim().is_empty()
            && base_url.trim_end_matches('/') == fallback_cfg.base_url.trim_end_matches('/');
        if same_provider(&acc.provider, fallback_cfg) || same_url {
            return fallback_cfg.api_key.clone();
        }
    }
    None
}

fn candidate_from_account(
    settings: &Settings,
    acc: &AccountEntry,
    spec_model: &str,
    fallback_cfg: &ProviderConfig,
) -> RouteCandidate {
    candidate_from_account_with(
        settings,
        acc,
        spec_model,
        fallback_cfg,
        &zerith_core::secrets::OsKeyring,
    )
}

/// Reads the account's OAuth credential and renews it when inside the skew.
///
/// A failed refresh returns the stale credential on purpose: the provider then
/// answers 401 and the gateway fails over to the next account — the same end
/// state as any dead account, with no extra retry path to keep alive.
///
/// ponytail: renews only pre-emptively (300 s before expiry). If that ever
/// proves tight, the upgrade is a refresher callback injected into `Gateway`.
pub fn hydrate_oauth(
    acc: &AccountEntry,
    store: &dyn zerith_core::secrets::SecretStore,
) -> Option<zerith_providers::OAuthCredential> {
    let mut cred = zerith_core::secrets::load_oauth(store, &acc.id)
        .ok()
        .flatten()?;
    if cred.needs_refresh(oauth_now()) {
        if let Ok(fresh) = zerith_providers::oauth::refresh(cred.tool, &cred) {
            // ponytail: a failed write only costs one extra refresh next turn.
            let _ = zerith_core::secrets::save_oauth(store, &acc.id, &fresh);
            cred = fresh;
        }
    }
    Some(cred)
}

fn oauth_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Same as `candidate_from_account`, with the cofre injected. Test seam.
pub fn candidate_from_account_with(
    settings: &Settings,
    acc: &AccountEntry,
    spec_model: &str,
    fallback_cfg: &ProviderConfig,
    store: &dyn zerith_core::secrets::SecretStore,
) -> RouteCandidate {
    let prov_name = canonical_provider(&acc.provider);
    let entry = settings
        .provider_entry(&acc.provider)
        .or_else(|| settings.provider_entry(&prov_name));
    let kind = ProviderKind::parse(&acc.provider);
    let same_fb = same_provider(&acc.provider, fallback_cfg);

    let protocol = acc
        .protocol
        .map(map_protocol)
        .or_else(|| entry.and_then(|e| e.protocol).map(map_protocol))
        .unwrap_or_else(|| {
            if kind == ProviderKind::Custom && same_fb {
                fallback_cfg.protocol
            } else {
                kind.default_protocol()
            }
        });

    let base_url = acc
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .and_then(|e| e.base_url.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| {
            if same_fb && !fallback_cfg.base_url.trim().is_empty() {
                fallback_cfg.base_url.clone()
            } else {
                kind.default_base_url().to_string()
            }
        });

    let api_key = resolve_account_key(acc, &base_url, fallback_cfg, store);
    let default_model = acc
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            entry
                .and_then(|e| e.default_model.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            same_fb
                .then(|| fallback_cfg.default_model.clone())
                .flatten()
        });

    let model = if !spec_model.trim().is_empty() {
        spec_model.trim().to_string()
    } else {
        default_model.clone().unwrap_or_default()
    };

    let mut cfg = ProviderConfig::new(kind);
    cfg.name = prov_name.clone();
    cfg.protocol = protocol;
    cfg.base_url = base_url;
    cfg.api_key = api_key;
    cfg.default_model = default_model;
    cfg.timeout_ms = entry
        .and_then(|e| e.timeout_ms)
        .unwrap_or(fallback_cfg.timeout_ms);
    cfg.context_limit = entry
        .and_then(|e| e.context_limit)
        .or(fallback_cfg.context_limit);
    cfg.account_id = Some(acc.id.clone());

    // An OAuth account carries no key: the credential replaces it, and a stale
    // credential is left in place so the gateway fails over instead of the turn
    // dying here.
    if acc.auth == zerith_core::settings::AuthMode::Oauth {
        cfg.auth = zerith_providers::AuthMode::Oauth;
        cfg.oauth = hydrate_oauth(acc, store);
        cfg.api_key = None;
    }

    RouteCandidate {
        account_id: acc.id.clone(),
        provider: prov_name,
        model,
        config: cfg,
        priority: acc.priority,
        quota_tokens: acc.quota_tokens,
        quota_requests: acc.quota_requests,
        window_hours: acc.window_hours,
    }
}

fn candidate_from_provider(
    settings: &Settings,
    provider: &str,
    spec_model: &str,
    fallback_cfg: &ProviderConfig,
) -> RouteCandidate {
    let prov_name = canonical_provider(provider);
    let entry = settings
        .provider_entry(provider)
        .or_else(|| settings.provider_entry(&prov_name));
    let kind = ProviderKind::parse(provider);
    let mut cfg = ProviderConfig::new(kind);
    cfg.name = prov_name.clone();
    cfg.timeout_ms = fallback_cfg.timeout_ms;
    cfg.context_limit = fallback_cfg.context_limit;
    if let Some(e) = entry {
        if let Some(p) = e.protocol {
            cfg.protocol = map_protocol(p);
        }
        if let Some(u) = e
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            cfg.base_url = u.to_string();
        }
        cfg.default_model = e.default_model.clone();
        if let Some(t) = e.timeout_ms {
            cfg.timeout_ms = t;
        }
        if e.context_limit.is_some() {
            cfg.context_limit = e.context_limit;
        }
    }
    let key_ref = entry.and_then(|e| e.api_key_ref.as_deref());
    cfg.api_key = secrets::resolve_api_key(&prov_name, key_ref).ok().flatten();
    let model = if !spec_model.trim().is_empty() {
        spec_model.trim().to_string()
    } else {
        cfg.default_model.clone().unwrap_or_default()
    };
    RouteCandidate {
        account_id: prov_name.clone(),
        provider: prov_name,
        model,
        config: cfg,
        priority: 0,
        quota_tokens: None,
        quota_requests: None,
        window_hours: 5,
    }
}

fn candidate_from_fallback(spec_model: &str, fallback_cfg: &ProviderConfig) -> RouteCandidate {
    let provider = if fallback_cfg.name.trim().is_empty() {
        fallback_cfg.kind.as_str().to_string()
    } else {
        fallback_cfg.name.clone()
    };
    let account_id = fallback_cfg
        .account_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| provider.clone());
    let model = if !spec_model.trim().is_empty() {
        spec_model.trim().to_string()
    } else {
        fallback_cfg.default_model.clone().unwrap_or_default()
    };
    RouteCandidate {
        account_id,
        provider,
        model,
        config: fallback_cfg.clone(),
        priority: 0,
        quota_tokens: None,
        quota_requests: None,
        window_hours: 5,
    }
}

/// Resolves a model string (plain model, `provider/model`, `account:model`, or
/// combo name) into an ordered [`Gateway`] of [`RouteCandidate`]s plus the
/// primary display model name.
pub fn resolve_gateway(
    settings: &Settings,
    requested_model: &str,
    fallback_cfg: &ProviderConfig,
) -> (Gateway, String) {
    let trimmed = requested_model.trim();
    let combo_models = settings
        .routing
        .combo(trimmed)
        .filter(|c| !c.models.is_empty())
        .map(|c| c.models.as_slice());
    let targets: Vec<&str> = match combo_models {
        Some(ms) => ms.iter().map(String::as_str).collect(),
        None => vec![trimmed],
    };

    let accounts = effective_accounts(settings);
    let known_account_ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();

    let mut candidates: Vec<RouteCandidate> = Vec::new();
    let mut saw_explicit_disabled = false;

    for target in targets {
        let spec = ModelSpec::parse_with_accounts(target, &known_account_ids);
        if let Some(ref acc_id) = spec.account_id {
            if let Some(acc) = accounts.iter().copied().find(|a| a.id == *acc_id) {
                if acc.enabled {
                    candidates.push(candidate_from_account(
                        settings,
                        acc,
                        &spec.model,
                        fallback_cfg,
                    ));
                } else {
                    saw_explicit_disabled = true;
                }
            }
            continue;
        }

        let (target_provider, provider_explicit_or_configured) = if let Some(ref p) = spec.provider
        {
            (Some(p.clone()), true)
        } else if let Some(inferred) = infer_provider(&spec.model) {
            let name = inferred.as_str();
            let has_entry = settings.provider_entry(name).is_some();
            if !settings.accounts.is_empty() || has_entry {
                (Some(name.to_string()), has_entry)
            } else {
                (None, false)
            }
        } else {
            (None, false)
        };

        let mut matched: Vec<&AccountEntry> = accounts
            .iter()
            .copied()
            .filter(|a| {
                a.enabled
                    && compatible_provider(&a.provider, target_provider.as_deref(), fallback_cfg)
            })
            .collect();
        matched.sort_by_key(|a| a.priority);

        if !matched.is_empty() {
            for acc in matched {
                candidates.push(candidate_from_account(
                    settings,
                    acc,
                    &spec.model,
                    fallback_cfg,
                ));
            }
        } else if let Some(ref tp) = target_provider {
            if provider_explicit_or_configured && !same_provider(tp, fallback_cfg) {
                candidates.push(candidate_from_provider(
                    settings,
                    tp,
                    &spec.model,
                    fallback_cfg,
                ));
            } else {
                candidates.push(candidate_from_fallback(&spec.model, fallback_cfg));
            }
        } else {
            candidates.push(candidate_from_fallback(&spec.model, fallback_cfg));
        }
    }

    if candidates.is_empty() && !saw_explicit_disabled {
        let parsed = ModelSpec::parse_with_accounts(trimmed, &known_account_ids);
        candidates.push(candidate_from_fallback(&parsed.model, fallback_cfg));
    }

    let display_model = candidates
        .first()
        .map(|c| c.model.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let parsed = ModelSpec::parse_with_accounts(trimmed, &known_account_ids);
            if parsed.model.is_empty() {
                trimmed.to_string()
            } else {
                parsed.model
            }
        });

    (Gateway::new(candidates), display_model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_memory::backend::{Fact, FactKind, MemoryError, RecallHit, Result as MemResult};

    /// A backend that answers from a fixed list, so the prompt rendering can be
    /// tested without a database or a network.
    struct Fixed(Vec<String>);

    impl MemoryBackend for Fixed {
        fn remember(
            &self,
            _scope: &Scope,
            _kind: FactKind,
            _text: &str,
            _confidence: zerith_memory::Confidence,
            _source: Option<&str>,
        ) -> MemResult<i64> {
            Ok(1)
        }

        fn recall(&self, _s: &Scope, _q: &str, limit: usize) -> MemResult<Vec<RecallHit>> {
            Ok(self
                .0
                .iter()
                .take(limit)
                .enumerate()
                .map(|(i, text)| RecallHit {
                    fact: Fact {
                        id: i as i64 + 1,
                        scope: "user:test".into(),
                        kind: FactKind::Fact,
                        text: text.clone(),
                        source: None,
                        session_id: None,
                        confidence: zerith_memory::Confidence::Confirmed,
                        created_at: 0,
                        updated_at: 0,
                        superseded_by: None,
                    },
                    score: 0.0,
                })
                .collect())
        }

        fn list(&self, _s: &Scope, _l: usize) -> MemResult<Vec<Fact>> {
            Err(MemoryError::Empty)
        }
        fn get(&self, id: i64) -> MemResult<Fact> {
            Err(MemoryError::NotFound(id))
        }
        fn update(&self, _id: i64, _t: &str) -> MemResult<()> {
            Ok(())
        }
        fn forget(&self, _id: i64) -> MemResult<()> {
            Ok(())
        }
        fn supersede(&self, _o: i64, _b: i64) -> MemResult<()> {
            Ok(())
        }
        fn export(&self, _s: &Scope) -> MemResult<Vec<Fact>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn facts_are_rendered_with_their_confidence() {
        let mem = Fixed(vec!["o projeto usa Rust".into()]);
        let block = recall_block(&mem, &Scope::user("t"), "x");
        assert!(block.contains("[confirmed] o projeto usa Rust"), "{block}");
    }

    #[test]
    fn an_empty_memory_adds_no_section_at_all() {
        let mem = Fixed(vec![]);
        assert_eq!(recall_block(&mem, &Scope::user("t"), "x"), "");
    }

    #[test]
    fn the_block_stops_at_the_budget_instead_of_growing_forever() {
        let big = "x".repeat(500);
        let mem = Fixed(vec![big; 20]);
        let block = recall_block(&mem, &Scope::user("t"), "x");
        assert!(block.len() <= MEMORY_BUDGET_CHARS + 128, "{}", block.len());
    }
}

/// What to tell the model about the working-method tools it was given. Only
/// the tools present are mentioned; static text, so the prompt cache stays warm.
pub(crate) fn tool_guidance(tools: &zerith_tools::Registry) -> String {
    let has = |n: &str| tools.get(n).is_some();
    let mut out = String::new();
    if has("task_create") {
        out.push_str(
            "\n\n# Lista de tarefas\nEm trabalho com vários passos, crie as tarefas com `task_create` antes de começar e atualize-as com `task_update` conforme avança. Mantenha exatamente uma tarefa `in_progress` por vez; marque `completed` assim que terminar, sem acumular.",
        );
    }
    if has("agent") {
        out.push_str(
            "\n\n# Subagentes\nUse `agent` para explorar o código ou pesquisar sem encher o seu contexto: só a resposta final volta. Para trabalho independente, chame vários `agent` na mesma mensagem. O prompt precisa bastar sozinho.",
        );
    }
    if has("ask_user") {
        out.push_str(
            "\n\n# Perguntas\nSe falta uma decisão que só o usuário pode tomar, use `ask_user` em vez de adivinhar.",
        );
    }
    if has("edit_file") {
        out.push_str(
            "\n\n# Edição\nLeia um arquivo com `read_file` antes de alterá-lo com `edit_file` ou `write_file`.",
        );
    }
    if has("create_skill") || has("create_agent") {
        out.push_str(
            "\n\n# Criar skills e agentes\nSó crie uma skill ou um agente quando a pessoa pedir, ou quando um procedimento claramente se repetir; nunca por conta própria em silêncio. O que você cria nasce desativado e a pessoa precisa revisar e ativar. Skill: a descrição é UMA frase dizendo QUANDO usar; o corpo traz passos numerados, armadilhas e como verificar; detalhes longos vão em references/. Agente: um papel focado, o mínimo de ferramentas, um critério de parada e o formato da resposta. Nunca coloque chaves, senhas nem caminhos da pasta pessoal.",
        );
    }
    out
}

/// The skills index for a turn with the `skill` tool: name and description of
/// each enabled skill, capped at `budget`, and how to load one. Bodies are never
/// inlined; that is what the tool is for.
pub fn skills_index(lib: &zerith_skills::Library, budget: usize) -> String {
    let Ok(catalog) = lib.catalog() else {
        return String::new();
    };
    if catalog.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Skills disponíveis\nPara usar uma, chame a ferramenta `skill` com o nome e siga as instruções que ela devolver.",
    );
    let mut cut = 0;
    for (name, description) in &catalog {
        let line = format!("\n- {name}: {description}");
        if out.len() + line.len() > budget {
            cut = catalog.len() - out.matches("\n- ").count();
            break;
        }
        out.push_str(&line);
    }
    if cut > 0 {
        out.push_str(&format!("\n[+{cut} skills omitidas pelo limite do prompt]"));
    }
    out
}

/// The skills section of the system prompt.
///
/// The spec is specific: do not load every skill body into context. So this
/// lists name and description for enabled skills only, and inlines the body
/// of a skill only when the user named it. Until the tool loop exists that is
/// the only honest reading of "load on demand".
pub fn skills_block(lib: &zerith_skills::Library, prompt: &str, budget: usize) -> String {
    let catalog = match lib.catalog() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    if catalog.is_empty() {
        return String::new();
    }
    let lower = prompt.to_ascii_lowercase();
    let mut out = String::from("# Skills disponíveis");
    let mut spent = 0usize;
    for (name, description) in &catalog {
        let line = format!("\n- {name}: {description}");
        if spent + line.len() > budget {
            break;
        }
        spent += line.len();
        out.push_str(&line);
        // Named in the request: the body is what the user is asking for.
        if lower.contains(&name.to_ascii_lowercase()) {
            if let Ok(body) = lib.load_body(name) {
                let room = budget.saturating_sub(spent);
                let take: String = body.chars().take(room).collect();
                if !take.is_empty() {
                    out.push_str("\n\n## ");
                    out.push_str(name);
                    out.push('\n');
                    out.push_str(&take);
                    spent += take.len();
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod skills_tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let p =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn skill(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d-{name}\nlicense: MIT\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn only_names_and_descriptions_are_listed_by_default() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        skill(&src, "demo", "CORPO-SECRETO");
        let lib = zerith_skills::Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "t").unwrap();
        let block = skills_block(&lib, "nada a ver", 2000);
        assert!(block.contains("demo: d-demo"));
        assert!(!block.contains("CORPO-SECRETO"));
    }

    #[test]
    fn naming_the_skill_inlines_its_body() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        skill(&src, "demo", "CORPO-SECRETO");
        let lib = zerith_skills::Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "t").unwrap();
        let block = skills_block(&lib, "use a skill demo agora", 2000);
        assert!(block.contains("CORPO-SECRETO"));
    }

    #[test]
    fn disabled_skills_do_not_reach_the_prompt() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        skill(&src, "demo", "x");
        let lib = zerith_skills::Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "t").unwrap();
        lib.set_status("demo", zerith_skills::Status::Disabled)
            .unwrap();
        assert!(skills_block(&lib, "demo", 2000).is_empty());
    }
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    use zerith_core::settings::ProviderEntry;
    use zerith_core::ComboEntry;

    #[test]
    fn custom_fallback_is_preserved_when_accounts_and_provider_entries_are_empty() {
        let settings = Settings::default();
        let mut fb = ProviderConfig::new(ProviderKind::Custom).with_key("k");
        fb.base_url = "http://127.0.0.1:9999/v1".into();

        let (gw, model) = resolve_gateway(&settings, "claude-test", &fb);
        assert_eq!(model, "claude-test");
        assert_eq!(gw.candidates.len(), 1);
        assert_eq!(gw.candidates[0].config.kind, ProviderKind::Custom);
        assert_eq!(gw.candidates[0].config.base_url, "http://127.0.0.1:9999/v1");
    }

    #[test]
    fn accounts_sort_stably_by_priority_and_combos_expand_in_order() {
        let mut settings = Settings {
            accounts: vec![
                AccountEntry {
                    id: "openai-b".into(),
                    provider: "openai".into(),
                    base_url: Some("http://127.0.0.1:2002/v1".into()),
                    priority: 20,
                    enabled: true,
                    ..Default::default()
                },
                AccountEntry {
                    id: "openai-a".into(),
                    provider: "openai".into(),
                    base_url: Some("http://127.0.0.1:2001/v1".into()),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                },
                AccountEntry {
                    id: "openai-disabled".into(),
                    provider: "openai".into(),
                    base_url: Some("http://127.0.0.1:2003/v1".into()),
                    priority: 1,
                    enabled: false,
                    ..Default::default()
                },
                AccountEntry {
                    id: "ds-1".into(),
                    provider: "deepseek".into(),
                    base_url: Some("http://127.0.0.1:3001/v1".into()),
                    priority: 5,
                    enabled: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        settings.routing.combos.push(ComboEntry {
            name: "fast".into(),
            models: vec![
                "openai-b:gpt-4o-mini".into(),
                "deepseek/deepseek-chat".into(),
            ],
            subagent_model: None,
        });

        let fb = ProviderConfig::new(ProviderKind::OpenAi).with_key("fb-key");

        // Bare openai model picks enabled openai accounts ordered by priority.
        let (gw, model) = resolve_gateway(&settings, "gpt-4o", &fb);
        assert_eq!(model, "gpt-4o");
        let ids: Vec<&str> = gw
            .candidates
            .iter()
            .map(|c| c.account_id.as_str())
            .collect();
        assert_eq!(ids, ["openai-a", "openai-b"]);
        assert_eq!(gw.candidates[0].config.api_key.as_deref(), Some("fb-key"));

        // Combo expands each target in declared order.
        let (gw, model) = resolve_gateway(&settings, "fast", &fb);
        assert_eq!(model, "gpt-4o-mini");
        assert_eq!(gw.candidates.len(), 2);
        assert_eq!(gw.candidates[0].account_id, "openai-b");
        assert_eq!(gw.candidates[0].model, "gpt-4o-mini");
        assert_eq!(gw.candidates[1].account_id, "ds-1");
        assert_eq!(gw.candidates[1].model, "deepseek-chat");

        // Explicitly targeting a disabled account leaves candidates empty.
        let (gw, _) = resolve_gateway(&settings, "openai-disabled:gpt-4o", &fb);
        assert!(gw.candidates.is_empty());
    }

    #[test]
    fn catalogue_account_is_reached_by_account_pin_not_by_bare_model() {
        // What the Contas card saves for a provider the core has no preset for.
        let groq = AccountEntry {
            id: "groq".into(),
            provider: "groq".into(),
            base_url: Some("https://api.groq.com/openai/v1".into()),
            protocol: Some(zerith_core::settings::WireProtocol::OpenAi),
            api_key_ref: Some("groq".into()),
            priority: 1,
            ..Default::default()
        };
        let claude = AccountEntry {
            id: "claude".into(),
            provider: "anthropic".into(),
            priority: 0,
            ..Default::default()
        };
        let settings = Settings {
            accounts: vec![claude, groq],
            ..Default::default()
        };
        let fb = ProviderConfig::new(ProviderKind::Anthropic);

        let (gw, model) = resolve_gateway(&settings, "groq:llama-3.3-70b-versatile", &fb);
        assert_eq!(model, "llama-3.3-70b-versatile");
        assert_eq!(gw.candidates.len(), 1);
        assert_eq!(gw.candidates[0].account_id, "groq");
        assert_eq!(
            gw.candidates[0].config.base_url,
            "https://api.groq.com/openai/v1"
        );
        assert_eq!(gw.candidates[0].config.protocol, Protocol::OpenAi);

        // A bare model has no provider prefix the core knows: it rides the
        // primary (anthropic). This is why the UI must send `account:model`.
        let (gw, _) = resolve_gateway(&settings, "llama-3.3-70b-versatile", &fb);
        assert_eq!(gw.candidates[0].account_id, "claude");
    }

    #[test]
    fn explicit_provider_entry_builds_config_when_no_account_matches() {
        let mut settings = Settings::default();
        settings.providers.push(ProviderEntry {
            name: "deepseek".into(),
            base_url: Some("http://127.0.0.1:4444/v1".into()),
            ..Default::default()
        });
        let fb = ProviderConfig::new(ProviderKind::Custom).with_key("fb-key");

        let (gw, model) = resolve_gateway(&settings, "deepseek-chat", &fb);
        assert_eq!(model, "deepseek-chat");
        assert_eq!(gw.candidates.len(), 1);
        assert_eq!(gw.candidates[0].provider, "deepseek");
        assert_eq!(gw.candidates[0].config.kind, ProviderKind::DeepSeek);
        assert_eq!(gw.candidates[0].config.base_url, "http://127.0.0.1:4444/v1");
    }

    #[test]
    fn system_prompt_places_token_saver_after_agent_and_before_memory() {
        let mut settings = Settings::default();
        settings.token_saver.ponytail = zerith_core::PonytailLevel::Full;
        settings.token_saver.caveman = zerith_core::CavemanLevel::Full;
        let agent = zerith_core::agents::AgentDef {
            name: "rev".into(),
            description: String::new(),
            model: None,
            instructions: "INSTRUCAO-AGENTE".into(),
            path: std::path::PathBuf::new(),
            layer: "user",
            tools: None,
            disallowed_tools: Vec::new(),
            permission_mode: None,
            max_turns: None,
            skills: Vec::new(),
            isolation: zerith_core::agents::Isolation::None,
            background: false,
            created_by_ai: false,
            disabled: false,
        };
        let prompt = system_prompt(
            "t",
            TurnContext {
                agent: Some(&agent),
                settings: Some(&settings),
                ..TurnContext::default()
            },
            false,
            "s1",
            "oi",
        );
        let ag_pos = prompt.find("INSTRUCAO-AGENTE").unwrap();
        let pt_pos = prompt.find("Ponytail Full").unwrap();
        let cv_pos = prompt.find("Caveman Full").unwrap();
        assert!(ag_pos < pt_pos && pt_pos < cv_pos, "{prompt}");
    }
}
