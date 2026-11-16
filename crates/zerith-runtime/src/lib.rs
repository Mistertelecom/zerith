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

pub use agent::{
    approval_channel, inbox_channel, run_agent_turn, AgentOptions, Approval, ApprovalGate,
    ApprovalSender, InboxReceiver, InboxSender, DEFAULT_MAX_PARALLEL_TOOLS, DEFAULT_MAX_STEPS,
};
pub use compact::{compact_now, CompactReport, Summarizer};
pub use events::{cancel_with, CancelCause, CauseSlot};
pub use pipeline::{Flow, Middleware, NoopMiddleware, ToolInvocation, ToolOutcome};
pub use question::{question_channel, QuestionGate, QuestionSender};
pub use subagent::{Gate, SubagentOptions};

use std::sync::mpsc;
use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{Client, ProviderError};
use zerith_sessions::Store;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("no such session: {0}")]
    NoSession(String),
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
}

/// The turn's outcome, with the numbers a UI needs to show it honestly.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnReport {
    pub session_id: String,
    pub message_id: i64,
    pub text: String,
    pub reasoning: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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
    let mut prompt_text = match std::env::current_dir() {
        Ok(cwd) => zerith_core::system_prompt_with_instructions(
            title,
            &zerith_core::load_instructions(&paths, &cwd),
        ),
        Err(_) => zerith_core::system_prompt(title),
    };
    if let Some(a) = ctx.agent {
        prompt_text.push_str(&format!("\n\n# Agente: {}\n\n{}", a.name, a.instructions));
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
