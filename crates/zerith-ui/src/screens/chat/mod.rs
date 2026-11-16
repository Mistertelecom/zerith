//! The conversation. `mod.rs` owns the state and the turn; the rest is split by
//! what it draws: `events` adapts runtime events into state, `transcript`
//! draws the messages and tool/subagent rows, `panels` the cards above the
//! composer, `composer` the input row, popups and footer.

mod actions;
mod composer;
pub mod events;
mod panels;
mod transcript;

use std::collections::HashSet;

use rune::{col, el, row, rule, text, El, TextEdit};
use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::{approval_channel, run_agent_turn, AgentOptions, ApprovalSender, TurnContext};
use zerith_security::{Mode, ModeHandle, Policy};
use zerith_sessions::Store;
use zerith_tools::Registry;

use crate::agentwork::{self, ContextMeter, Item, PermissionMode, Price, QuestionState, TaskItem};
use crate::ui::{self, Ui};
use crate::widgets::{chip, dot, pill, Load};
use crate::{row_of, wire, Shell};

pub use actions::*;
pub use events::{answer_question, apply, apply_work, skip_question};
pub use panels::approval_card;

/// Messages read when a conversation opens. Older ones load on demand.
pub const MESSAGE_PAGE: usize = 200;

/// One line of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub role: &'static str,
    pub content: String,
    /// A tool call, a subagent or a note; plain messages carry none.
    pub item: Option<Item>,
}

impl Line {
    pub fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            item: None,
        }
    }
    pub fn item(item: Item) -> Self {
        Self {
            role: "item",
            content: String::new(),
            item: Some(item),
        }
    }
}

/// A tool waiting for the user's permission.
#[derive(Clone, Debug, PartialEq)]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub tool: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Once,
    Session,
    Deny,
}

/// Whether the provider answered. Drives the header dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Unknown,
    Checking,
    Up,
    Down(String),
}

pub struct ChatState {
    /// The open conversation; `None` is a new, unsaved chat.
    pub selected: Option<String>,
    pub lines: Vec<Line>,
    /// Older messages exist beyond the loaded page.
    pub more: bool,
    pub draft: TextEdit,
    pub status: String,
    pub busy: bool,
    pub cancel: Option<Cancel>,
    pub model: String,
    /// `Client::models()`, fetched once when the selector first opens.
    pub models: Load<Vec<String>>,
    pub model_menu: bool,
    /// The agent the next turn runs as; `None` is the default agent.
    pub agent: Option<String>,
    pub agent_menu: bool,
    pub link: Link,
    /// Transcript lines the user toggled open (or closed, for rows that open
    /// by default, like a diff).
    pub open: HashSet<usize>,
    pub pending_approval: Option<Approval>,
    /// Tools the user allowed for the rest of this conversation.
    pub allowed: HashSet<String>,
    /// Decisions taken, newest last: what the runtime was told.
    pub decisions: Vec<(String, Decision)>,
    /// `(prompt, completion)` tokens of the last model request, when the
    /// provider said. The prompt side is what sits in the context window.
    pub usage: Option<(u64, u64)>,
    /// The running turn's approval channel. `None` between turns.
    pub approvals: Option<ApprovalSender>,
    /// The running turn's `ask_user` channel. `None` between turns.
    pub questions: Option<zerith_runtime::QuestionSender>,

    // ---- agent work
    /// The live checklist, replaced whole by each update.
    pub tasks: Vec<TaskItem>,
    pub tasks_open: bool,
    /// The plan card's editor, while the plan is being edited.
    pub plan_edit: Option<TextEdit>,
    /// Plans the user approved with edits: `(approval id, edited text)`.
    pub plan_edits: Vec<(String, String)>,
    /// The open `ask_user` card.
    pub question: Option<QuestionState>,
    /// Answers handed to the runtime: `(question set id, answers)`.
    pub answers: Vec<(String, Vec<Vec<String>>)>,
    /// The live permission mode, shared with the policy of the running turn.
    pub mode: ModeHandle,
    pub mode_menu: bool,
    /// Files of the working directory, for `@`.
    pub files: Load<Vec<String>>,
    /// Row highlighted in the `/` or `@` popup.
    pub popup_active: usize,
    /// The draft text Esc closed the popup for; typing reopens it.
    pub popup_dismissed: Option<String>,
    /// USD per million tokens of the active model, when known.
    pub price: Option<Price>,
    /// A compaction is running.
    pub compacting: bool,
    /// The runtime can rewind to a message's checkpoint.
    pub rewind: bool,
    /// Active gateway route `(account_id, model)` for the current/last turn.
    pub active_route: Option<(String, String)>,
    /// Tokens saved by Token Saver (RTK, Ponytail, Caveman) in the active turn.
    pub saved_tokens: u64,
}

impl ChatState {
    pub fn new(model: String) -> Self {
        // The mode the policy would start in, so the chip is true before the
        // first turn builds a policy of its own.
        let start = std::env::current_dir()
            .ok()
            .and_then(|cwd| Policy::from_env(cwd).ok())
            .map_or(Mode::Default, |p| p.mode());
        Self {
            selected: None,
            lines: Vec::new(),
            more: false,
            draft: TextEdit::new(""),
            status: String::new(),
            busy: false,
            cancel: None,
            model,
            models: Load::Idle,
            model_menu: false,
            agent: None,
            agent_menu: false,
            link: Link::Unknown,
            open: HashSet::new(),
            pending_approval: None,
            allowed: HashSet::new(),
            decisions: Vec::new(),
            usage: None,
            approvals: None,
            questions: None,
            tasks: Vec::new(),
            tasks_open: true,
            plan_edit: None,
            plan_edits: Vec::new(),
            question: None,
            answers: Vec::new(),
            mode: ModeHandle::new(start),
            mode_menu: false,
            files: Load::Idle,
            popup_active: 0,
            popup_dismissed: None,
            price: None,
            compacting: false,
            rewind: wire::checkpoints_supported(),
            active_route: None,
            saved_tokens: 0,
        }
    }

    /// The permission mode in force right now.
    pub fn permission_mode(&self) -> PermissionMode {
        PermissionMode::from_security(self.mode.get())
    }

    /// Tokens in the window against the active provider's limit.
    pub fn meter(&self, limit: Option<u32>) -> Option<ContextMeter> {
        let (p, q) = self.usage?;
        Some(ContextMeter { used: p + q, limit })
    }

    /// Clears what belongs to one conversation.
    fn forget_turn_state(&mut self) {
        self.open.clear();
        self.lines.clear();
        self.more = false;
        self.usage = None;
        self.allowed.clear();
        self.pending_approval = None;
        self.tasks.clear();
        self.plan_edit = None;
        self.question = None;
        self.compacting = false;
        self.active_route = None;
        self.saved_tokens = 0;
    }
}

fn role_label(role: &str) -> &'static str {
    match role {
        "user" => "user",
        "assistant" => "assistant",
        "system" => "system",
        _ => "tool",
    }
}

pub fn load_selected(s: &mut Shell) {
    s.chat.forget_turn_state();
    let Some(id) = s.chat.selected.clone() else {
        s.chat.status.clear();
        return;
    };
    let db = s.db().to_path_buf();
    // The newest page: count first, then read the tail.
    let read = Store::open(&db).and_then(|st| {
        let n = st.message_count(&id)? as usize;
        let skip = n.saturating_sub(MESSAGE_PAGE);
        Ok((skip > 0, st.messages(&id, MESSAGE_PAGE, skip)?))
    });
    let c = &mut s.chat;
    match read {
        Ok((more, msgs)) => {
            c.more = more;
            c.lines = msgs
                .into_iter()
                .filter(|m| !m.content.trim().is_empty())
                .map(|m| Line::new(role_label(m.role.as_str()), m.content))
                .collect();
            c.status.clear();
        }
        Err(e) => c.status = format!("Não consegui ler a sessão: {e}"),
    }
}

/// Reads every message of the open session. Pagination keeps the first open
/// cheap; asking for the rest is explicit.
pub fn load_all(s: &mut Shell) {
    let Some(id) = s.chat.selected.clone() else {
        return;
    };
    let db = s.db().to_path_buf();
    s.work(
        move || {
            Store::open(&db)
                .and_then(|st| st.messages(&id, usize::MAX >> 1, 0))
                .map_err(|e| e.to_string())
        },
        |s, r| match r {
            Ok(msgs) => {
                s.chat.more = false;
                s.chat.open.clear();
                s.chat.lines = msgs
                    .into_iter()
                    .filter(|m| !m.content.trim().is_empty())
                    .map(|m| Line::new(role_label(m.role.as_str()), m.content))
                    .collect();
            }
            Err(e) => s.chat.status = e,
        },
    );
}

pub fn new_chat(s: &mut Shell) {
    let c = &mut s.chat;
    c.forget_turn_state();
    c.selected = None;
    c.draft = TextEdit::new("");
    c.status.clear();
}

pub fn stop(s: &mut Shell) {
    if let Some(c) = s.chat.cancel.take() {
        c.cancel();
        s.chat.status = "cancelado".to_string();
    }
    // A stopped turn cannot be waiting on a decision any more.
    if let Some(a) = s.chat.pending_approval.take() {
        wire::answer_approval(s.chat.approvals.as_ref(), &a, Decision::Deny);
    }
}

pub fn request_approval(s: &mut Shell, a: Approval) {
    if s.chat.allowed.contains(&a.tool) {
        wire::answer_approval(s.chat.approvals.as_ref(), &a, Decision::Session);
        s.chat.decisions.push((a.id, Decision::Session));
        return;
    }
    s.chat.pending_approval = Some(a);
}

pub fn decide(s: &mut Shell, d: Decision) {
    let Some(a) = s.chat.pending_approval.take() else {
        return;
    };
    if d == Decision::Session {
        s.chat.allowed.insert(a.tool.clone());
    }
    wire::answer_approval(s.chat.approvals.as_ref(), &a, d);
    s.chat.decisions.push((a.id.clone(), d));
    if d == Decision::Deny {
        s.chat.status = format!("{} negado", a.tool);
    }
}

fn fail(s: &mut Shell, error: String) {
    let c = &mut s.chat;
    c.busy = false;
    c.cancel = None;
    c.approvals = None;
    c.pending_approval = None;
    c.status = error.clone();
    c.lines.push(Line::new("error", error));
}

/// Fetches the model list once. The answer is cached for the window's life;
/// a failure is shown and can be retried by opening the menu again.
pub fn toggle_models(s: &mut Shell) {
    s.chat.model_menu = !s.chat.model_menu;
    if s.chat.model_menu {
        fetch_models(s);
    }
}

/// Asks the provider for its models once; the answer also sets the header
/// dot. Cached for the window's life; a failure can be retried.
pub fn fetch_models(s: &mut Shell) {
    if matches!(s.chat.models, Load::Loading | Load::Ready(_)) {
        return;
    }
    s.chat.models = Load::Loading;
    s.chat.link = Link::Checking;
    let cfg = s.providers.active();
    s.work(
        move || {
            Client::new(cfg)
                .and_then(|c| c.models())
                .map_err(|e| e.to_string())
        },
        |s, r| {
            s.chat.link = match &r {
                Ok(_) => Link::Up,
                Err(e) => Link::Down(e.clone()),
            };
            s.chat.models = r.into();
        },
    );
}

pub fn send(s: &mut Shell) {
    if s.chat.busy {
        return;
    }
    let prompt = s.chat.draft.text.trim().to_string();
    if prompt.is_empty() {
        return;
    }
    if s.chat.model.is_empty() {
        fail(
            s,
            "Nenhum modelo: escolha um no seletor ou defina ZERITH_MODEL".into(),
        );
        return;
    }

    // The session row is created on the UI thread: one local insert, and the
    // list shows the conversation before the first token arrives.
    let session_id = match s.chat.selected.clone() {
        Some(id) => id,
        None => {
            let title: String = prompt.chars().take(48).collect();
            let created = Store::open(s.db())
                .and_then(|st| st.create_session(&title, zerith_sessions::SessionSource::Native));
            match created {
                Ok(sess) => {
                    let id = sess.id.clone();
                    s.sessions.insert(0, row_of(sess));
                    s.chat.selected = Some(id.clone());
                    id
                }
                Err(e) => {
                    fail(s, e.to_string());
                    return;
                }
            }
        }
    };

    let db = s.db().to_path_buf();
    let model = s.chat.model.clone();
    let cfg = s.providers.active();
    let handle = s.me.clone();
    let mode = s.chat.mode.clone();
    let cancel = Cancel::default();
    let (approvals_tx, gate) = approval_channel();
    let (questions_tx, question_gate) = zerith_runtime::question_channel();
    let c = &mut s.chat;
    c.cancel = Some(cancel.clone());
    c.approvals = Some(approvals_tx);
    c.questions = Some(questions_tx);
    c.draft = TextEdit::new("");
    c.busy = true;
    c.status.clear();
    c.saved_tokens = 0;
    c.lines.push(Line::new("user", prompt.clone()));
    c.lines.push(Line::new("assistant", String::new()));
    let paused = s.memory.paused;
    // Resolved on the worker: reading agent files is disk I/O.
    let agent_name = s.chat.agent.clone();
    let root = s.root();
    let history = crate::wire::profile_limits(s.settings.profile).history_window;

    std::thread::spawn(move || {
        let prepared = (|| -> Result<(Store, Client), String> {
            let store = Store::open(&db).map_err(|e| e.to_string())?;
            let client = Client::new(cfg).map_err(|e| e.to_string())?;
            Ok((store, client))
        })();
        let (store, client) = match prepared {
            Ok(v) => v,
            Err(e) => {
                handle.update(move |s: &mut Shell, _| fail(s, e));
                return;
            }
        };
        let memory = if paused {
            None
        } else {
            SqliteMemory::open(&db).ok()
        };
        let scope = wire::memory_scope();
        // Tools act on the directory the window was started from; the policy
        // decides per call, and anything it marks "ask" comes back as a card.
        let workspace = std::env::current_dir().unwrap_or_else(|_| db.clone());
        let paths = zerith_core::Paths::with_root(&root);
        let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
            .and_then(|l| l.contributions())
            .ok();
        let mut settings = zerith_core::Settings::load(&paths, Some(&workspace)).settings;
        if let Some(c) = &contrib {
            c.apply_hooks(&mut settings);
        }
        let mut policy = Policy::from_settings(&settings, &workspace)
            .map(|p| p.with_handle(mode))
            .ok();
        let mut tools = Registry::new();
        // MCP servers from settings and plugins become `mcp__server__tool`.
        let mut mcp = zerith_mcp::McpRegistry::load(&paths, Some(&workspace));
        if let Some(c) = &contrib {
            mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
        }
        let _ = mcp.register_into(&mut tools);
        let prompt = if mcp.is_empty() {
            prompt
        } else {
            let (expanded, errors) = mcp.expand_mentions(&prompt);
            for e in &errors {
                eprintln!("zerith-ui: {e}");
            }
            expanded
        };
        // Hooks and rules ride the pipeline; a failure to build them leaves
        // the policy alone, which still gates every call.
        let guards = policy
            .as_mut()
            .map(|p| zerith_runtime::guards::Guards::new(settings.clone(), p, session_id.clone()));
        if let Some(g) = &guards {
            tools.register(g.enter_plan_mode_tool());
            tools.register(g.exit_plan_mode_tool());
        }
        let skills = zerith_skills::Library::open(db.parent().unwrap_or(&db).join("skills"))
            .ok()
            .map(|l| {
                if let Some(c) = &contrib {
                    l.with_extra(c.skill_dirs())
                } else {
                    l
                }
            });
        let extra_agents = contrib.as_ref().map(|c| c.agents.as_slice()).unwrap_or(&[]);
        let agent = agent_name.and_then(|n| {
            zerith_core::agents::find_with_extra(&paths, Some(&workspace), extra_agents, &n)
        });
        let reporter = handle.clone();
        let result = run_agent_turn(
            &store,
            &client,
            TurnContext {
                memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
                scope: Some(&scope),
                skills: skills.as_ref(),
                agent: agent.as_ref(),
                settings: Some(&settings),
            },
            AgentOptions {
                tools: policy.is_some().then_some(&tools),
                policy: policy.as_ref(),
                approvals: Some(&gate),
                questions: Some(&question_gate),
                middleware: guards
                    .into_iter()
                    .map(|g| Box::new(g) as Box<dyn zerith_runtime::Middleware + Send + Sync>)
                    .collect(),
                spill_dir: Some(root.join("spill")),
                history,
                ..AgentOptions::default()
            },
            &session_id,
            &model,
            &prompt,
            &cancel,
            move |event| reporter.update(move |s: &mut Shell, _| s.apply(event)),
        );
        match result {
            Ok(r) => {
                handle.update(move |s: &mut Shell, _| {
                    events::apply_report(s, &r);
                });
            }
            Err(e) => {
                let msg = e.to_string();
                handle.update(move |s: &mut Shell, _| {
                    s.chat.link = Link::Down(msg.clone());
                    fail(s, msg);
                });
            }
        }
    });
}

/// Opens or closes the agent menu, reading the definitions the first time.
pub fn toggle_agents(s: &mut Shell) {
    s.chat.agent_menu = !s.chat.agent_menu;
    if s.chat.agent_menu && s.agents.list.is_idle() {
        s.load_agents();
    }
}

pub const SUGGESTIONS: [&str; 3] = [
    "Resuma o que mudou neste projeto",
    "Explique um erro de compilação",
    "Planeje uma tarefa em passos",
];

fn empty_state(u: &Ui) -> El {
    let mut chips = row().gap(8.0).flex_wrap().justify_center();
    for (i, label) in SUGGESTIONS.iter().enumerate() {
        chips = chips.child(chip(
            format!("suggest-{i}"),
            label,
            u,
            move |s: &mut Shell| {
                s.chat.draft = TextEdit::new(*label);
                s.chat.draft.focused = true;
            },
        ));
    }
    col()
        .key("chat-empty")
        .w_full()
        .grow(1.0)
        .center()
        .gap(8.0)
        .child(
            text("Como posso ajudar?")
                .font(ui::TITLE.lh(24.0))
                .color(u.content),
        )
        .child(
            text("O turno roda aqui, na sua máquina, com o provedor que você escolher.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(chips.mt(16.0))
}

fn header(s: &Shell, u: &Ui) -> El {
    let c = &s.chat;
    let title = c
        .selected
        .as_deref()
        .and_then(|id| s.sessions.iter().find(|r| r.id == id))
        .map(|r| r.title.clone())
        .unwrap_or_else(|| "Nova conversa".to_string());
    let (color, word) = match &c.link {
        Link::Unknown => (u.idle, "não verificado".to_string()),
        Link::Checking => (u.warn, "verificando".to_string()),
        Link::Up => (u.ok, "conectado".to_string()),
        Link::Down(e) => (u.danger, format!("sem conexão: {e}")),
    };
    let (route_label, model_label) = match &c.active_route {
        Some((account_id, model)) => (account_id.clone(), model.clone()),
        None => (s.providers.active().name, c.model.clone()),
    };
    row()
        .w_full()
        .h(28.0)
        .center_y()
        .gap(10.0)
        .child(
            text(title)
                .font(ui::TITLE)
                .color(u.content)
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(
            row()
                .key("link")
                .role("status")
                .label(format!("{route_label}: {word}"))
                .gap(6.0)
                .center_y()
                .child(dot(color))
                .child(text(route_label).font(ui::LABEL).color(u.muted_text)),
        )
        .child(if model_label.is_empty() {
            el()
        } else {
            pill(model_label, u)
        })
}

pub fn view(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let body = if c.lines.is_empty() {
        empty_state(u)
    } else {
        transcript::list(s, u)
    };
    let mut page = col()
        .w_full()
        .h_full()
        .gap(12.0)
        .child(header(s, u))
        .child(rule(1.0, u.line).w_full())
        .child(body);
    // One card at a time asks something of the user; the checklist is
    // information and sits under it.
    if let Some(a) = &c.pending_approval {
        page = page.child(if agentwork::is_plan(&a.tool, &a.title) {
            panels::plan_card(s, a, u)
        } else {
            panels::approval_card(a, u)
        });
    }
    if let Some(q) = &c.question {
        page = page.child(panels::question_card(q, u));
    }
    if let Some(t) = panels::tasks(s, u) {
        page = page.child(t);
    }
    page.child(composer::composer(s, u, layer))
}
