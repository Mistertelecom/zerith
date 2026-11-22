//! One agent turn, assembled the way the core expects it.
//!
//! This is the only place the window touches tools, policy, MCP, skills and
//! plugins. It runs on a worker thread: opening the store, reading settings and
//! loading MCP servers are all disk or network work.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::{
    run_agent_turn, AgentOptions, ApprovalGate, InboxReceiver, QuestionGate, RunEvent, TurnContext,
};
use zerith_security::{Mode, Policy};
use zerith_sessions::Store;
use zerith_tools::Registry;

use crate::types::QuestionView;

/// What the turn needs from the window, resolved before the thread starts.
pub struct Setup {
    pub root: PathBuf,
    pub db: PathBuf,
    /// The folder tools act on, and where project instructions are read.
    pub workspace: PathBuf,
    pub mode: Mode,
    pub memory_paused: bool,
    pub agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub orchestrator: bool,
    pub executor_model: Option<String>,
    pub mcp_cache: Option<Arc<crate::state::McpCache>>,
    /// Registers and returns a cancellable token when background pre-compaction spawns.
    pub register_precompact: Option<Arc<dyn Fn() -> Cancel + Send + Sync>>,
}

/// Terminal classification of [`run`] for desktop notifications and callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Done,
    Cancelled,
    Failed(String),
}

/// One thing that happened during the turn, already shaped for the window.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TurnEvent {
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Step {
        n: usize,
    },
    ToolCall {
        id: String,
        name: String,
        args: String,
    },
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        preview: String,
    },
    Approval {
        id: u64,
        tool: String,
        summary: String,
    },
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        ttft_ms: u64,
    },
    Compacted {
        before: usize,
        after: usize,
    },
    /// The history was summarized before the turn because it neared the
    /// model's context window.
    AutoCompacted {
        before: usize,
        after: usize,
    },
    Tasks {
        items: Vec<zerith_sessions::Todo>,
    },
    Subagent {
        id: String,
        name: String,
        state: String,
        summary: String,
        model: String,
    },
    Question {
        id: u64,
        questions: Vec<QuestionView>,
    },
    Route {
        account_id: String,
        provider: String,
        model: String,
        attempts: usize,
    },
    Saved {
        tool: String,
        before: u64,
        after: u64,
    },
    ToolProgress {
        tool: String,
        chunk: String,
    },
    Steered {
        text: String,
    },
    /// The permission mode changed mid-turn (the plan was approved, or the
    /// model entered plan). `mode` is spelled like `send_turn`'s `mode` arg,
    /// so the window stores it and sends it back on the next turn.
    ModeChanged {
        mode: String,
    },
    Done {
        stored: Option<i64>,
        text: String,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        saved_tokens: u64,
        model_used: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// Translates the runtime's event into the window's shape. `None` means the
/// window does not care (it never does for an unknown event).
fn shape(ev: RunEvent) -> Option<TurnEvent> {
    Some(match ev {
        RunEvent::Delta(text) => TurnEvent::Delta { text },
        RunEvent::Reasoning(text) => TurnEvent::Reasoning { text },
        RunEvent::Done { .. } => return None,
        RunEvent::Step { n } => TurnEvent::Step { n },
        RunEvent::ToolCall { id, name, args } => TurnEvent::ToolCall { id, name, args },
        RunEvent::ToolResult {
            id,
            name,
            ok,
            output_preview,
        } => TurnEvent::ToolResult {
            id,
            name,
            ok,
            preview: output_preview,
        },
        RunEvent::ApprovalNeeded { id, tool, summary } => TurnEvent::Approval { id, tool, summary },
        RunEvent::Usage {
            input,
            output,
            cache_read,
            cache_write,
            ttft_ms,
        } => TurnEvent::Usage {
            input,
            output,
            cache_read,
            cache_write,
            ttft_ms,
        },
        RunEvent::Compacted { before, after } => TurnEvent::Compacted { before, after },
        RunEvent::Tasks { items } => TurnEvent::Tasks { items },
        RunEvent::Subagent {
            id,
            name,
            state,
            summary,
            model,
        } => TurnEvent::Subagent {
            id,
            name,
            state,
            summary,
            model,
        },
        RunEvent::Question { id, questions } => TurnEvent::Question {
            id,
            questions: questions.iter().map(QuestionView::of).collect(),
        },
        RunEvent::Route {
            account_id,
            provider,
            model,
            attempts,
        } => TurnEvent::Route {
            account_id,
            provider,
            model,
            attempts,
        },
        RunEvent::TokenSaved {
            tool,
            before_bytes,
            after_bytes,
            ..
        } => TurnEvent::Saved {
            tool,
            before: before_bytes as u64,
            after: after_bytes as u64,
        },
        RunEvent::ToolProgress { tool, chunk } => TurnEvent::ToolProgress { tool, chunk },
        RunEvent::Steered { text } => TurnEvent::Steered { text },
    })
}

/// Runs the turn to completion. Blocking: call it on a worker thread.
#[allow(clippy::too_many_arguments)]
pub fn run(
    setup: &Setup,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &Cancel,
    approvals: &ApprovalGate,
    questions: &QuestionGate,
    inbox: Option<&InboxReceiver>,
    on: &mut dyn FnMut(TurnEvent),
) -> Result<TurnOutcome, String> {
    let store = Store::open(&setup.db).map_err(|e| e.to_string())?;
    let paths = zerith_core::Paths::with_root(&setup.root);
    let cfg = zerith_core::Settings::load(&paths, Some(&setup.workspace)).settings;
    // Usually already up (started with the app). One that died restarts in the
    // background; this turn goes direct instead of waiting for the boot.
    zerith_runtime::bili::ensure(&setup.root, cfg.context.billion_context, false);
    // User messages replayed per turn: the execution profile's, as in the CLI.
    let limits = cfg.profile.limits();
    let history = limits.history_window;
    // `configured_providers_from` reuses the already-loaded `Settings` so we
    // never parse settings.toml twice per turn.
    let provider = crate::wire::configured_providers_from(&cfg, &setup.root)
        .into_iter()
        .next()
        .ok_or_else(|| "Nenhum provedor configurado".to_string())?;
    let client = Client::new(provider).map_err(|e| zerith_providers::explain::headline(&e))?;

    let memory = if setup.memory_paused {
        None
    } else {
        SqliteMemory::open(&setup.db).ok()
    };
    let scope = zerith_memory::Scope::workspace(&setup.workspace);
    let contrib = zerith_plugins::Library::open(&setup.root.join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    let mut settings = cfg;
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }
    if let Some(exec_m) = setup
        .executor_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        settings.routing.subagent_model = Some(exec_m.to_string());
    }
    // A broken rule fails the turn out loud: dropping it would run the turn
    // with no tools and no word why.
    let mut policy = Some(
        Policy::from_settings(&settings, &setup.workspace)
            .map_err(|e| format!("Permissões inválidas nas configurações: {e}"))?
            .with_handle(zerith_security::ModeHandle::new(setup.mode)),
    );

    let mut tools = Registry::new();
    let build_mcp = || {
        let mut reg = zerith_mcp::McpRegistry::load(&paths, Some(&setup.workspace));
        if let Some(c) = &contrib {
            reg.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
        }
        reg
    };
    let mcp: Arc<zerith_mcp::McpRegistry> = match &setup.mcp_cache {
        Some(cache) => {
            let fp = crate::state::mcp_config_fingerprint(&setup.root, &setup.workspace);
            cache.get_or_build(&setup.workspace, fp, limits.cache_entries, build_mcp)
        }
        None => Arc::new(build_mcp()),
    };
    let _ = mcp.register_into(&mut tools);
    let prompt = if mcp.is_empty() {
        prompt.to_string()
    } else {
        let (expanded, errors) = mcp.expand_mentions(prompt);
        for e in &errors {
            eprintln!("zerith-desktop: {e}");
        }
        expanded
    };

    let guards = policy
        .as_mut()
        .map(|p| zerith_runtime::guards::Guards::new(settings.clone(), p, session_id.to_string()));
    if let Some(g) = &guards {
        tools.register(g.enter_plan_mode_tool());
        tools.register(g.exit_plan_mode_tool());
    }
    if let Some(p) = policy.as_ref() {
        let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
        for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
            paths: zerith_core::Paths::with_root(&setup.root),
            project_dir: Some(setup.workspace.clone()),
            tools: have,
            mode: p.mode().settings_name().to_string(),
        }) {
            tools.register(t);
        }
    }

    let skills = zerith_skills::Library::open(setup.root.join("skills"))
        .ok()
        .map(|l| match &contrib {
            Some(c) => l.with_extra(c.skill_dirs()),
            None => l,
        });
    let extra = contrib.as_ref().map(|c| c.agents.as_slice()).unwrap_or(&[]);
    let agent = setup.agent.as_ref().and_then(|n| {
        zerith_core::agents::find_with_extra(&paths, Some(&setup.workspace), extra, n)
    });

    if cancel.is_cancelled() {
        return Ok(TurnOutcome::Cancelled);
    }

    auto_compact(
        &store, &settings, &client, session_id, model, history, cancel, on,
    );

    if cancel.is_cancelled() {
        return Ok(TurnOutcome::Cancelled);
    }

    let mut mode_watch = policy.as_mut().map(|p| {
        let h = p.mode_handle();
        let now = h.get();
        (h, now)
    });
    let mut digest = zerith_runtime::learn::TurnDigest::new(&prompt);
    let result = run_agent_turn(
        &store,
        &client,
        TurnContext {
            memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
            scope: Some(&scope),
            skills: skills.as_ref(),
            agent: agent.as_ref(),
            settings: Some(&settings),
            project_dir: Some(&setup.workspace),
            no_project_instructions: false,
        },
        AgentOptions {
            tools: policy.is_some().then_some(&tools),
            policy: policy.as_ref(),
            approvals: Some(approvals),
            questions: Some(questions),
            middleware: guards
                .into_iter()
                .map(|g| Box::new(g) as Box<dyn zerith_runtime::Middleware + Send + Sync>)
                .collect(),
            spill_dir: Some(setup.root.join("spill")),
            history,
            inbox,
            reasoning_effort: setup.reasoning_effort.clone(),
            orchestrator: setup.orchestrator,
            executor_model: setup.executor_model.clone(),
            ..AgentOptions::default()
        },
        session_id,
        model,
        &prompt,
        cancel,
        |ev| {
            digest.observe(&ev);
            if let Some(ev) = shape(ev) {
                on(ev);
            }
            // Tools (exit/enter_plan_mode) switch the handle while they run;
            // tell the window, or its next turn sends the old mode again.
            if let Some((h, seen)) = mode_watch.as_mut() {
                let now = h.get();
                if now != *seen {
                    *seen = now;
                    on(TurnEvent::ModeChanged {
                        mode: mode_name(now).into(),
                    });
                }
            }
        },
    );

    match result {
        Ok(r) => {
            digest.answer = r.text.clone();
            // The sidebar draws the provider's logo from these columns.
            let provider = r
                .account_id
                .as_deref()
                .and_then(|id| settings.account(id))
                .map(|a| a.provider.clone());
            let _ =
                store.set_session_model(session_id, r.model_used.as_deref(), provider.as_deref());
            on(TurnEvent::Done {
                stored: Some(r.message_id),
                text: r.text,
                prompt_tokens: r.prompt_tokens,
                completion_tokens: r.completion_tokens,
                cache_read_tokens: r.cache_read_tokens,
                cache_write_tokens: r.cache_write_tokens,
                saved_tokens: r.saved_tokens,
                model_used: r.model_used.clone(),
            });
            // Best-effort background pre-compaction when context usage sits in [70%, 85%).
            let model_for_win = r.model_used.as_deref().unwrap_or(model);
            let ctx_limit = zerith_runtime::resolve_gateway(&settings, model, client.config())
                .0
                .candidates
                .into_iter()
                .next()
                .and_then(|c| c.config.context_limit)
                .or(client.config().context_limit)
                .or_else(|| zerith_providers::models::context_window(model_for_win));
            if let Some(win) = ctx_limit.filter(|&w| w > 0) {
                // The last request's prompt is the window's fill; the
                // turn's sum counts the same history once per step.
                let pct = r.last_prompt_tokens.saturating_mul(100) / u64::from(win);
                if (70..85).contains(&pct) {
                    let db_bg = setup.db.clone();
                    let settings_bg = settings.clone();
                    let cfg_bg = client.config().clone();
                    let sid_bg = session_id.to_string();
                    let model_bg = model.to_string();
                    let bg_cancel = setup
                        .register_precompact
                        .as_ref()
                        .map_or_else(Cancel::default, |f| f());
                    std::thread::spawn(move || {
                        if bg_cancel.is_cancelled() {
                            return;
                        }
                        if let (Ok(bg_store), Ok(bg_client)) =
                            (Store::open(&db_bg), Client::new(cfg_bg))
                        {
                            let _ = zerith_runtime::compact::pre_compact_routed(
                                &bg_store,
                                &settings_bg,
                                &bg_client,
                                &sid_bg,
                                &model_bg,
                                history,
                                &bg_cancel,
                            );
                        }
                    });
                }
            }
            // After Done, on its own thread: the answer never waits for it.
            crate::learn::spawn(crate::learn::Job {
                root: setup.root.clone(),
                db: setup.db.clone(),
                fallback: client.config().clone(),
                settings,
                session_id: session_id.to_string(),
                model: model.to_string(),
                memory_paused: setup.memory_paused,
                scope: scope.clone(),
                digest,
            });
            Ok(TurnOutcome::Done)
        }
        Err(zerith_runtime::RunError::Provider(zerith_providers::ProviderError::Cancelled)) => {
            Ok(TurnOutcome::Cancelled)
        }
        Err(e) => {
            let msg = match &e {
                zerith_runtime::RunError::Provider(p) => zerith_providers::explain::headline(p),
                other => other.to_string(),
            };
            on(TurnEvent::Failed {
                message: msg.clone(),
            });
            Ok(TurnOutcome::Failed(msg))
        }
    }
}

/// Summarizes the history before the turn when it passed 75% of the model's
/// window, with the model and account the turn is routed to. Never fails the
/// turn: the runtime still trims under pressure if this did not run.
#[allow(clippy::too_many_arguments)]
fn auto_compact(
    store: &Store,
    settings: &zerith_core::Settings,
    client: &Client,
    session_id: &str,
    model: &str,
    history: usize,
    cancel: &Cancel,
    on: &mut dyn FnMut(TurnEvent),
) {
    match zerith_runtime::compact::auto_compact_routed(
        store, settings, client, session_id, model, history, cancel,
    ) {
        Ok(Some(r)) => on(TurnEvent::AutoCompacted {
            before: r.tokens_before,
            after: r.tokens_after,
        }),
        Ok(None) => {}
        Err(e) => eprintln!("zerith-desktop: resumo automático falhou: {e}"),
    }
}

/// The inverse of [`parse_mode`]: the spelling the window sends.
pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Default => "default",
        Mode::AcceptEdits => "acceptEdits",
        Mode::Plan => "plan",
        Mode::DontAsk | Mode::Bypass => "bypass",
    }
}

/// The mode a stored value means. Unknown strings fall back to the safest one.
pub fn parse_mode(s: &str) -> Mode {
    match s {
        "acceptEdits" | "accept_edits" => Mode::AcceptEdits,
        "plan" => Mode::Plan,
        // The window no longer offers `dontAsk`; an old saved value means the
        // mode that asks nothing.
        "dontAsk" | "dont_ask" => Mode::Bypass,
        "bypass" => Mode::Bypass,
        _ => Mode::Default,
    }
}

/// Where a conversation with no folder of its own starts: the folder the app
/// was launched from, unless that folder means nothing (the exe's own folder or
/// System32, which is what the Start menu hands a process), then the home.
pub fn default_workspace() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    launch_dir(std::env::current_dir().ok(), exe_dir.as_deref(), home)
}

fn launch_dir(cwd: Option<PathBuf>, exe_dir: Option<&Path>, home: PathBuf) -> PathBuf {
    let meaningless = |d: &Path| {
        let s = d.to_string_lossy().to_ascii_lowercase();
        Some(d) == exe_dir || s.ends_with("system32") || s.ends_with("syswow64")
    };
    match cwd {
        Some(d) if !meaningless(&d) => d,
        _ => home,
    }
}

/// The folder a new conversation gets: the newest one still on disk, else
/// [`default_workspace`].
pub fn last_workspace(db: &Path) -> PathBuf {
    // Never create a database just to read a folder name.
    if !db.is_file() {
        return default_workspace();
    }
    Store::open(db)
        .and_then(|st| st.recent_workspaces(10))
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .unwrap_or_else(default_workspace)
}

/// The folder a turn of `session` runs in: its own, else [`last_workspace`]
/// (sessions made before folders existed, or whose folder was deleted).
pub fn session_workspace(db: &Path, session: &str) -> PathBuf {
    Store::open(db)
        .and_then(|st| st.workspace(session))
        .ok()
        .flatten()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| last_workspace(db))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_sessions::SessionSource;

    fn scratch(tag: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(tag);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Each conversation runs in its own folder; one without a folder (or
    /// whose folder is gone) takes the newest folder still on disk.
    #[test]
    fn a_turn_runs_in_its_sessions_folder() {
        let d = scratch("session-workspace");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let db = d.join("zerith.db");
        let st = Store::open(&db).unwrap();
        let s1 = st.create_session("um", SessionSource::Native).unwrap();
        let s2 = st.create_session("dois", SessionSource::Native).unwrap();
        st.set_workspace(&s1.id, &a.to_string_lossy()).unwrap();
        assert_eq!(session_workspace(&db, &s1.id), a);
        assert_eq!(session_workspace(&db, &s2.id), a, "sem pasta: a última");
        st.set_workspace(&s2.id, &b.to_string_lossy()).unwrap();
        assert_eq!(session_workspace(&db, &s2.id), b);
        assert_eq!(session_workspace(&db, &s1.id), a, "cada uma a sua");
        std::fs::remove_dir_all(&b).unwrap();
        assert_eq!(session_workspace(&db, &s2.id), a, "pasta apagada");
    }

    #[test]
    fn mode_names_round_trip_through_parse_mode() {
        for m in [Mode::Default, Mode::AcceptEdits, Mode::Plan, Mode::Bypass] {
            assert_eq!(parse_mode(mode_name(m)), m);
        }
        let ev = TurnEvent::ModeChanged {
            mode: mode_name(Mode::AcceptEdits).into(),
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"type": "modeChanged", "mode": "acceptEdits"})
        );
    }

    #[test]
    fn the_start_menu_folder_falls_back_to_home() {
        let home = PathBuf::from("home");
        let exe = PathBuf::from("app");
        let proj = PathBuf::from("proj");
        let sys = PathBuf::from("C:/Windows/System32");
        assert_eq!(
            launch_dir(Some(proj.clone()), Some(&exe), home.clone()),
            proj
        );
        assert_eq!(
            launch_dir(Some(exe.clone()), Some(&exe), home.clone()),
            home
        );
        assert_eq!(launch_dir(Some(sys), Some(&exe), home.clone()), home);
        assert_eq!(launch_dir(None, None, home.clone()), home);
    }
}
