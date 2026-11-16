//! The conversation: transcript, approval card, composer.

use std::collections::HashSet;

use rune::{col, el, row, rule, text, text_input, El, GrowthSpec, ScrollSpec, TextEdit, Tx};
use rune_kit::{Collapsible, Markdown, Placement, Select};
use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::{
    approval_channel, run_agent_turn, AgentOptions, ApprovalSender, RunEvent, TurnContext,
};
use zerith_security::Policy;
use zerith_sessions::Store;
use zerith_tools::Registry;

use crate::ui::{self, Ui};
use crate::widgets::{self, button, chip, dot, eyebrow, pill, Load, Tone};
use crate::{row_of, wire, Shell};

/// Messages read when a conversation opens. Older ones load on demand.
pub const MESSAGE_PAGE: usize = 200;

/// One line of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub role: &'static str,
    pub content: String,
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
    pub link: Link,
    /// Transcript lines whose reasoning/tool section is expanded.
    pub open: HashSet<usize>,
    pub pending_approval: Option<Approval>,
    /// Tools the user allowed for the rest of this conversation.
    pub allowed: HashSet<String>,
    /// Decisions taken, newest last: what the runtime was told.
    pub decisions: Vec<(String, Decision)>,
    /// `(prompt, completion)` tokens of the last turn, when the provider said.
    pub usage: Option<(u64, u64)>,
    /// The running turn's approval channel. `None` between turns.
    pub approvals: Option<ApprovalSender>,
}

impl ChatState {
    pub fn new(model: String) -> Self {
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
            link: Link::Unknown,
            open: HashSet::new(),
            pending_approval: None,
            allowed: HashSet::new(),
            decisions: Vec::new(),
            usage: None,
            approvals: None,
        }
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
    let c = &mut s.chat;
    c.lines.clear();
    c.open.clear();
    c.more = false;
    c.usage = None;
    c.allowed.clear();
    c.pending_approval = None;
    let Some(id) = c.selected.clone() else {
        c.status.clear();
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
                .map(|m| Line {
                    role: role_label(m.role.as_str()),
                    content: m.content,
                })
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
                    .map(|m| Line {
                        role: role_label(m.role.as_str()),
                        content: m.content,
                    })
                    .collect();
            }
            Err(e) => s.chat.status = e,
        },
    );
}

pub fn new_chat(s: &mut Shell) {
    let c = &mut s.chat;
    c.selected = None;
    c.open.clear();
    c.lines.clear();
    c.more = false;
    c.usage = None;
    c.allowed.clear();
    c.pending_approval = None;
    c.draft = TextEdit::new("");
    c.status.clear();
}

pub fn apply(s: &mut Shell, event: RunEvent) {
    let c = &mut s.chat;
    match event {
        RunEvent::Delta(chunk) => {
            if let Some(last) = c.lines.last_mut().filter(|l| l.role == "assistant") {
                last.content.push_str(&chunk);
            } else {
                c.lines.push(Line {
                    role: "assistant",
                    content: chunk,
                });
            }
        }
        RunEvent::Reasoning(chunk) => {
            if let Some(last) = c.lines.last_mut().filter(|l| l.role == "thinking") {
                last.content.push_str(&chunk);
            } else {
                c.lines.push(Line {
                    role: "thinking",
                    content: chunk,
                });
            }
        }
        RunEvent::Done { .. } => {
            c.busy = false;
            c.cancel = None;
            c.approvals = None;
            c.status.clear();
            c.lines.retain(|l| {
                l.role != "thinking" && !(l.role == "assistant" && l.content.is_empty())
            });
            c.link = Link::Up;
            s.refresh_sessions();
        }
        RunEvent::ToolCall { name, args, .. } => {
            c.lines.push(Line {
                role: "tool",
                content: format!("{name} {args}"),
            });
        }
        RunEvent::ToolResult {
            name,
            ok,
            output_preview,
            ..
        } => {
            let mark = if ok { "ok" } else { "erro" };
            c.lines.push(Line {
                role: "tool",
                content: format!(
                    "{name} → {mark}
{output_preview}"
                ),
            });
            // The next text delta starts a fresh answer below the tool rows.
            c.lines.push(Line {
                role: "assistant",
                content: String::new(),
            });
        }
        RunEvent::ApprovalNeeded { id, tool, summary } => {
            request_approval(
                s,
                Approval {
                    id: id.to_string(),
                    title: format!("{tool} quer executar"),
                    tool,
                    summary,
                },
            );
        }
        RunEvent::Usage { input, output } => c.usage = Some((input, output)),
        #[allow(unreachable_patterns)]
        _ => {}
    }
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
    c.lines.push(Line {
        role: "error",
        content: error,
    });
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
    let cancel = Cancel::default();
    let (approvals_tx, gate) = approval_channel();
    let c = &mut s.chat;
    c.cancel = Some(cancel.clone());
    c.approvals = Some(approvals_tx);
    c.draft = TextEdit::new("");
    c.busy = true;
    c.status.clear();
    c.lines.push(Line {
        role: "user",
        content: prompt.clone(),
    });
    c.lines.push(Line {
        role: "assistant",
        content: String::new(),
    });
    let paused = s.memory.paused;

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
        let policy = Policy::from_env(&workspace);
        let tools = Registry::new();
        let skills = zerith_skills::Library::open(db.parent().unwrap_or(&db).join("skills")).ok();
        let reporter = handle.clone();
        let result = run_agent_turn(
            &store,
            &client,
            TurnContext {
                memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
                scope: Some(&scope),
                skills: skills.as_ref(),
                ..TurnContext::default()
            },
            AgentOptions {
                tools: policy.is_ok().then_some(&tools),
                policy: policy.as_ref().ok(),
                approvals: Some(&gate),
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
                let usage = (r.prompt_tokens, r.completion_tokens);
                handle.update(move |s: &mut Shell, _| {
                    s.chat.usage = (usage != (0, 0)).then_some(usage);
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

fn line_row(index: usize, line: &Line, u: &Ui, live: bool, open: bool) -> El {
    match line.role {
        "user" => row().w_full().justify_end().pb(18.0).child(
            col()
                .max_w(560.0)
                .py(9.0)
                .px(13.0)
                .radius(14.0)
                .fill(u.muted)
                .child(
                    text(line.content.clone())
                        .wrap()
                        .font(ui::BODY)
                        .color(u.content)
                        .selectable(true),
                ),
        ),
        "thinking" | "tool" => {
            let title = if line.role == "thinking" {
                "Raciocínio"
            } else {
                "Ferramenta"
            };
            col().w_full().pb(18.0).child(
                Collapsible::new(title, open)
                    .ghost()
                    .on_toggle(move |s: &mut Shell| s.toggle_open(index))
                    .child(
                        text(line.content.clone())
                            .wrap()
                            .font(ui::SMALL)
                            .color(u.muted_text)
                            .selectable(true),
                    )
                    .build(&ui::kit(u)),
            )
        }
        "error" => col()
            .w_full()
            .pb(18.0)
            .child(widgets::error_line(line.content.clone(), u)),
        _ => {
            let body = if line.content.is_empty() {
                "…".to_string()
            } else {
                line.content.clone()
            };
            let answer = if line.role == "assistant" {
                Markdown::new(body)
                    .streaming(live)
                    .selectable(true)
                    .build(&ui::kit(u))
            } else {
                text(body)
                    .wrap()
                    .font(ui::BODY)
                    .color(u.content)
                    .selectable(true)
            };
            col()
                .w_full()
                .pb(18.0)
                .gap(4.0)
                .child(eyebrow(
                    if line.role == "assistant" {
                        "Zerith"
                    } else {
                        "Sistema"
                    },
                    u,
                ))
                .child(answer)
        }
    }
}

/// The permission card. It sits above the composer, so the decision is the
/// next thing the eye meets, and holds the turn until answered.
pub fn approval_card(a: &Approval, u: &Ui) -> El {
    col()
        .key("approval")
        .role("alertdialog")
        .label(format!("Permissão pedida: {}", a.title))
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(12.0)
        .fill(u.muted)
        .border(1.0, u.line_strong)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(rune::icon(ui::icon_shield(), 16.0).color(u.warn))
                .child(
                    text(a.title.clone())
                        .font(ui::TITLE)
                        .color(u.content)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(pill(a.tool.clone(), u)),
        )
        .child(
            text(a.summary.clone())
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .selectable(true),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(button(
                    "approve-once",
                    "Permitir uma vez",
                    Tone::Primary,
                    true,
                    u,
                    |s, _| s.decide(Decision::Once),
                ))
                .child(button(
                    "approve-session",
                    "Permitir na sessão",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| s.decide(Decision::Session),
                ))
                .child(el().grow(1.0))
                .child(button("deny", "Negar", Tone::Danger, true, u, |s, _| {
                    s.decide(Decision::Deny)
                })),
        )
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
        .gap(6.0)
        .child(
            text("Como posso ajudar?")
                .font(ui::TITLE.lh(22.0))
                .color(u.content),
        )
        .child(
            text("O turno roda aqui, na sua máquina, com o provedor que você escolher.")
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
        )
        .child(chips.mt(14.0))
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
        Link::Unknown => (u.faint, "não verificado".to_string()),
        Link::Checking => (u.warn, "verificando".to_string()),
        Link::Up => (u.ok, "conectado".to_string()),
        Link::Down(e) => (u.danger, format!("sem conexão: {e}")),
    };
    let provider = s.providers.active().name;
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
                .label(format!("{provider}: {word}"))
                .gap(6.0)
                .center_y()
                .child(dot(color))
                .child(text(provider).font(ui::LABEL).color(u.muted_text)),
        )
        .child(if c.model.is_empty() {
            el()
        } else {
            pill(c.model.clone(), u)
        })
}

fn model_select(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let mut options: Vec<String> = match &c.models {
        Load::Ready(m) => m.clone(),
        _ => Vec::new(),
    };
    if !c.model.is_empty() && !options.contains(&c.model) {
        options.insert(0, c.model.clone());
    }
    let placeholder = match &c.models {
        Load::Loading => "carregando modelos…",
        Load::Failed(_) => "modelos indisponíveis",
        _ => "escolher modelo",
    };
    Select::new("model", &options)
        .label("Modelo")
        .placeholder(placeholder)
        .current((!c.model.is_empty()).then_some(c.model.as_str()))
        .open(c.model_menu && !options.is_empty())
        .placement(Placement::TopStart)
        .on_toggle(toggle_models)
        .on_close(|s: &mut Shell| s.chat.model_menu = false)
        .on_choose(|s: &mut Shell, v: &str, _: &mut Tx| {
            s.chat.model = v.to_string();
            s.chat.model_menu = false;
        })
        .build(&ui::kit(u), layer)
}

fn composer(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let field = text_input(&c.draft, ui::BODY)
        .key("draft")
        .grow(1.0)
        .min_w(0.0)
        .color(u.content)
        .label("Mensagem para o Zerith")
        .placeholder("Pergunte algo…", u.faint)
        .on_edit(|s: &mut Shell| &mut s.chat.draft)
        .on_submit(|s: &mut Shell, _: &mut Tx| send(s))
        // The editor would take Esc (blur) and Ctrl+digits; the window's
        // shortcuts win while a turn runs or for navigation chords.
        .on_input_key(|s: &mut Shell, ev: &rune::KeyEvent, tx: &mut Tx| {
            let global = (ev.key == rune::Key::Escape
                && (s.chat.cancel.is_some() || s.chat.model_menu))
                || "1234567nk".chars().any(|c| ev.is_command(c));
            global && crate::on_key(s, ev, tx)
        });
    let action = if c.busy {
        widgets::icon_button("stop", "Parar o turno (Esc)", ui::icon_stop(), u, |s| {
            s.stop()
        })
    } else {
        widgets::primary_icon("send", "Enviar a mensagem", ui::icon_send(), u, send)
    };
    let mut foot = row()
        .w_full()
        .h(24.0)
        .gap(8.0)
        .center_y()
        .child(model_select(s, u, layer));
    if let Load::Failed(e) = &c.models {
        foot = foot.child(
            text(e.clone())
                .font(ui::LABEL)
                .color(u.faint)
                .truncate()
                .shrink()
                .min_w(0.0),
        );
    }
    foot = foot.child(el().grow(1.0));
    if let Some((p, q)) = c.usage {
        foot = foot.child(
            text(format!("{p} entrada · {q} saída tokens"))
                .key("usage")
                .font(ui::LABEL)
                .color(u.faint),
        );
    }
    if !c.status.is_empty() {
        foot = foot.child(
            text(c.status.clone())
                .font(ui::LABEL)
                .color(u.faint)
                .truncate()
                .shrink()
                .min_w(0.0),
        );
    }
    col()
        .key("composer")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .min_h(44.0)
                .pl(14.0)
                .pr(8.0)
                .py(8.0)
                .gap(8.0)
                .center_y()
                .radius(12.0)
                .fill(u.muted)
                .border(
                    1.0,
                    if c.draft.focused {
                        u.line_strong
                    } else {
                        u.line
                    },
                )
                .child(field)
                .child(action),
        )
        .child(foot.px(2.0))
}

pub fn view(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let body = if c.lines.is_empty() {
        empty_state(u)
    } else {
        // Virtualized: a long conversation mounts only the visible messages.
        let n = c.lines.len();
        let busy = c.busy;
        let uu = *u;
        let mut inner = col().w_full();
        if c.more {
            inner = inner.child(row().w_full().pb(16.0).justify_center().child(button(
                "older",
                "Carregar mensagens anteriores",
                Tone::Plain,
                true,
                u,
                |s, _| load_all(s),
            )));
        }
        inner = inner.child(
            col()
                .key("transcript")
                .w_full()
                .growth(GrowthSpec::default())
                .virtual_rows(
                    n,
                    80.0,
                    |_: &Shell, i| format!("line-{i}").into(),
                    move |s: &Shell, i| {
                        let line = &s.chat.lines[i];
                        let live = busy && i + 1 == s.chat.lines.len();
                        line_row(i, line, &uu, live, s.chat.open.contains(&i))
                    },
                ),
        );
        inner
            .scroll(ScrollSpec { stick_bottom: true })
            .grow(1.0)
            .min_h(0.0)
    };
    let mut page = col()
        .w_full()
        .h_full()
        .gap(12.0)
        .child(header(s, u))
        .child(rule(1.0, u.line).w_full())
        .child(body);
    if let Some(a) = &c.pending_approval {
        page = page.child(approval_card(a, u));
    }
    page.child(composer(s, u, layer))
}
