//! One agent turn, assembled the way the core expects it.
//!
//! This is the only place the window touches tools, policy, MCP, skills and
//! plugins. It runs on a worker thread: opening the store, reading settings and
//! loading MCP servers are all disk or network work.

use std::path::{Path, PathBuf};

use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::{
    run_agent_turn, AgentOptions, ApprovalGate, QuestionGate, RunEvent, TurnContext,
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
    pub history: usize,
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
    },
    Compacted {
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
    Done {
        stored: Option<i64>,
        text: String,
        prompt_tokens: u64,
        completion_tokens: u64,
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
        RunEvent::Usage { input, output } => TurnEvent::Usage { input, output },
        RunEvent::Compacted { before, after } => TurnEvent::Compacted { before, after },
        RunEvent::Tasks { items } => TurnEvent::Tasks { items },
        RunEvent::Subagent {
            id,
            name,
            state,
            summary,
        } => TurnEvent::Subagent {
            id,
            name,
            state,
            summary,
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
    on: &mut dyn FnMut(TurnEvent),
) -> Result<(), String> {
    let store = Store::open(&setup.db).map_err(|e| e.to_string())?;
    let paths = zerith_core::Paths::with_root(&setup.root);
    let cfg = zerith_core::Settings::load(&paths, Some(&setup.workspace)).settings;
    // `configured_providers` already puts the routed primary first, so the head
    // of the list is the provider this turn should use.
    let provider = crate::wire::configured_providers(&setup.root)
        .into_iter()
        .next()
        .ok_or_else(|| "Nenhum provedor configurado".to_string())?;
    let client = Client::new(provider).map_err(|e| zerith_providers::explain::headline(&e))?;

    let memory = if setup.memory_paused {
        None
    } else {
        SqliteMemory::open(&setup.db).ok()
    };
    let scope = crate::wire::memory_scope();
    let contrib = zerith_plugins::Library::open(&setup.root.join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    let mut settings = cfg;
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }
    let mut policy = Policy::from_settings(&settings, &setup.workspace)
        .map(|p| p.with_handle(zerith_security::ModeHandle::new(setup.mode)))
        .ok();

    let mut tools = Registry::new();
    let mut mcp = zerith_mcp::McpRegistry::load(&paths, Some(&setup.workspace));
    if let Some(c) = &contrib {
        mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
    }
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
            history: setup.history,
            ..AgentOptions::default()
        },
        session_id,
        model,
        &prompt,
        cancel,
        |ev| {
            if let Some(ev) = shape(ev) {
                on(ev);
            }
        },
    );

    match result {
        Ok(r) => {
            on(TurnEvent::Done {
                stored: Some(r.message_id),
                text: r.text,
                prompt_tokens: r.prompt_tokens,
                completion_tokens: r.completion_tokens,
                saved_tokens: r.saved_tokens,
                model_used: r.model_used,
            });
            Ok(())
        }
        Err(e) => {
            let msg = match &e {
                zerith_runtime::RunError::Provider(p) => zerith_providers::explain::headline(p),
                other => other.to_string(),
            };
            on(TurnEvent::Failed { message: msg });
            Ok(())
        }
    }
}

/// The mode a stored value means. Unknown strings fall back to the safest one.
pub fn parse_mode(s: &str) -> Mode {
    match s {
        "acceptEdits" | "accept_edits" => Mode::AcceptEdits,
        "plan" => Mode::Plan,
        "dontAsk" | "dont_ask" => Mode::DontAsk,
        "bypass" => Mode::Bypass,
        _ => Mode::Default,
    }
}

/// The folder the window's tools act on.
pub fn workspace() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}
