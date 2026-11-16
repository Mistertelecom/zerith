//! Native shell. Rune draws every pixel; there is no webview, no Chromium and
//! no JavaScript in this process.
//!
//! The layout copies OpenHuman's shell deliberately (`ContentSurface` and
//! `AppSidebar` in `app/src/components/layout/shell/`): a tinted chrome frame,
//! the session column sitting on it, and one inset content sheet with a 2xl
//! radius and a hairline edge. `view` is a pure function of `Shell`; the
//! database and the provider run on a worker thread and come back through
//! `AppHandle::update`, so a slow model never stalls a frame.

mod ui;

use std::path::PathBuf;

use rune::{
    col, el, icon, row, rule, text, text_input, AppHandle, Cx, El, ScrollSpec, TextEdit, Tx,
};
use ui::Ui;
use zerith_memory::{Fact, MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client, ProviderConfig};
use zerith_runtime::{run_turn_cancellable, RunEvent};
use zerith_sessions::{Session, Store};
use zerith_skills::discover;

/// The sidebar column: 240 pt, like `SIDEBAR_WIDTH` in OpenHuman's shell.
const SIDEBAR: f32 = 240.0;
/// The sheet's inset from the window edge, OpenHuman's `m-2`.
const INSET: f32 = 8.0;
/// `rounded-2xl`.
const SHEET_RADIUS: f32 = 16.0;

/// One conversation in the sidebar.
#[derive(Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub source: String,
}

/// One line of the transcript.
#[derive(Clone, PartialEq)]
pub struct Line {
    pub role: &'static str,
    pub content: String,
}

pub struct Shell {
    pub sessions: Vec<Row>,
    pub selected: usize,
    pub lines: Vec<Line>,
    /// Confirmed facts the user can see and delete. The same rows the turn
    /// recalls, read straight from the shared database.
    pub facts: Vec<Fact>,
    /// Skills found on disk. Listed, never executed: enabling one is a user
    /// action, and no code runs merely because a window opened.
    pub skills: Vec<String>,
    /// Tasks from plans that have unfinished work. Read from the store on
    /// reload, so a plan started in another process shows up here.
    pub plan_tasks: Vec<(String, String, String)>,
    pub draft: TextEdit,
    pub status: String,
    pub busy: bool,
    /// The token the worker thread is watching. `None` when no turn is in
    /// flight, so the stop button cannot exist without something to stop.
    pub cancel: Option<Cancel>,
    pub model: String,
    db: PathBuf,
    me: AppHandle<Shell>,
}

impl Shell {
    /// Opens the window against the real database.
    ///
    /// A broken database still opens a window: refusing to start would hide
    /// the reason, and `zerith doctor` is the command that reports it.
    pub fn open(db: PathBuf, me: AppHandle<Shell>) -> Self {
        let cfg = ProviderConfig::from_env();
        let mut shell = Self {
            sessions: Vec::new(),
            selected: 0,
            lines: Vec::new(),
            facts: Vec::new(),
            skills: Vec::new(),
            plan_tasks: Vec::new(),
            draft: TextEdit::new(""),
            status: String::new(),
            busy: false,
            cancel: None,
            model: cfg.default_model.clone().unwrap_or_default(),
            db,
            me,
        };
        shell.reload();
        shell
    }

    fn reload(&mut self) {
        let store = match Store::open(&self.db) {
            Ok(s) => s,
            // The reason matters: "not available" without the path and the
            // OS error leaves the user with nothing to fix.
            Err(e) => {
                self.status = format!("Banco indisponível ({}): {e}", self.db.display());
                return;
            }
        };
        self.sessions = store
            .list_sessions(100)
            .unwrap_or_default()
            .into_iter()
            .map(row_of)
            .collect();
        if self.selected >= self.sessions.len() {
            self.selected = 0;
        }
        if let Some(root) = self.db.parent() {
            self.skills = discover(&root.join("skills"))
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.frontmatter.name)
                .collect();
        }
        let mut rows = Vec::new();
        for plan in store.unfinished_plans().unwrap_or_default() {
            for t in store.tasks(&plan).unwrap_or_default() {
                rows.push((t.state.as_str().to_string(), t.title, plan.clone()));
            }
        }
        self.plan_tasks = rows;
        self.facts = SqliteMemory::open(&self.db)
            .and_then(|m| m.list(&self.scope(), 50))
            .unwrap_or_default();
        self.load_selected();
    }

    /// The memory scope of this install. One operator, one scope.
    fn scope(&self) -> Scope {
        Scope::user(&std::env::var("ZERITH_USER").unwrap_or_else(|_| "local".to_string()))
    }

    /// Deletes a fact permanently. The backend keeps a tombstone so the
    /// dedupe key cannot resurrect the deleted text.
    pub fn forget(&mut self, id: i64) {
        match SqliteMemory::open(&self.db).and_then(|m| m.forget(id)) {
            Ok(()) => {
                self.facts.retain(|f| f.id != id);
                self.status = format!("Fato {id} apagado");
            }
            Err(e) => self.status = format!("Não consegui apagar: {e}"),
        }
    }

    fn load_selected(&mut self) {
        self.lines.clear();
        let Some(s) = self.sessions.get(self.selected) else {
            self.status = String::new();
            return;
        };
        let id = s.id.clone();
        self.status = match Store::open(&self.db).and_then(|st| st.messages(&id, 200, 0)) {
            Ok(msgs) => {
                self.lines = msgs
                    .into_iter()
                    .filter(|m| !m.content.trim().is_empty())
                    .map(|m| Line {
                        role: role_label(m.role.as_str()),
                        content: m.content,
                    })
                    .collect();
                String::new()
            }
            Err(e) => format!("Não consegui ler a sessão: {e}"),
        };
    }

    fn select(&mut self, i: usize) {
        if i < self.sessions.len() && i != self.selected {
            self.selected = i;
            self.load_selected();
        }
    }

    /// Starts an empty conversation. The session row is only written when the
    /// first message is actually sent, so an abandoned draft leaves no empty
    /// conversation in the list.
    fn new_chat(&mut self) {
        self.selected = self.sessions.len();
        self.lines.clear();
        self.draft = TextEdit::new("");
        self.status = String::new();
    }

    /// Applies one worker event. Deltas grow the last assistant line in
    /// place, which is why the transcript stores text and not tokens.
    pub fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::Delta(chunk) => {
                if let Some(last) = self.lines.last_mut().filter(|l| l.role == "assistant") {
                    last.content.push_str(&chunk);
                } else {
                    self.lines.push(Line {
                        role: "assistant",
                        content: chunk,
                    });
                }
            }
            RunEvent::Reasoning(chunk) => {
                // Reasoning is shown while it streams and dropped once the
                // answer arrives: it is context for the user, not history.
                if let Some(last) = self.lines.last_mut().filter(|l| l.role == "thinking") {
                    last.content.push_str(&chunk);
                } else {
                    self.lines.push(Line {
                        role: "thinking",
                        content: chunk,
                    });
                }
            }
            RunEvent::Done { .. } => {
                self.busy = false;
                self.cancel = None;
                self.status = String::new();
                self.lines.retain(|l| l.role != "thinking");
                self.reload_sessions_only();
            }
        }
    }

    /// Re-reads the sidebar without touching the transcript, so a finished
    /// turn does not scroll the conversation back to the top.
    fn reload_sessions_only(&mut self) {
        if let Ok(store) = Store::open(&self.db) {
            self.sessions = store
                .list_sessions(100)
                .unwrap_or_default()
                .into_iter()
                .map(row_of)
                .collect();
        }
    }

    /// Stops the turn. The message already stored stays: the runtime commits
    /// the question before it asks, so a cancel loses the answer, never the
    /// prompt.
    pub fn stop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
            self.status = "cancelado".to_string();
        }
    }

    fn fail(&mut self, error: String) {
        self.busy = false;
        self.cancel = None;
        self.status = error.clone();
        self.lines.push(Line {
            role: "error",
            content: error,
        });
    }
}

/// Roles are matched as strings here because the view draws them, and a
/// `&'static str` keeps `Line` cheap to clone.
fn role_label(role: &str) -> &'static str {
    match role {
        "user" => "user",
        "assistant" => "assistant",
        "system" => "system",
        _ => "tool",
    }
}

fn row_of(s: Session) -> Row {
    Row {
        title: if s.title.trim().is_empty() {
            "(sem título)".to_string()
        } else {
            s.title
        },
        id: s.id,
        source: s.source.as_str().to_string(),
    }
}

/// Sends the draft. The session is created on first use, so an empty database
/// needs no setup step.
fn send(s: &mut Shell) {
    if s.busy {
        return;
    }
    let prompt = s.draft.text.trim().to_string();
    if prompt.is_empty() {
        return;
    }
    if s.model.is_empty() {
        s.fail("Nenhum modelo: defina ZERITH_MODEL".into());
        return;
    }

    // The session is created here, on the UI thread: it is a single local
    // insert, and doing it inline means the sidebar shows the conversation
    // before the first token arrives instead of after.
    let session_id = match s.sessions.get(s.selected) {
        Some(row) => row.id.clone(),
        None => {
            let title: String = prompt.chars().take(48).collect();
            let created = Store::open(&s.db)
                .and_then(|st| st.create_session(&title, zerith_sessions::SessionSource::Native));
            match created {
                Ok(sess) => {
                    s.sessions.push(row_of(sess.clone()));
                    s.selected = s.sessions.len() - 1;
                    sess.id
                }
                Err(e) => {
                    s.fail(e.to_string());
                    return;
                }
            }
        }
    };

    let db = s.db.clone();
    let model = s.model.clone();
    let handle = s.me.clone();

    let cancel = Cancel::default();
    s.cancel = Some(cancel.clone());
    s.draft = TextEdit::new("");
    s.busy = true;
    s.status = String::new();
    s.lines.push(Line {
        role: "user",
        content: prompt.clone(),
    });
    s.lines.push(Line {
        role: "assistant",
        content: String::new(),
    });

    std::thread::spawn(move || {
        // The worker opens its own connection: SQLite allows concurrent
        // readers, and the UI must not hold a connection across a request.
        let prepared = (|| -> Result<(Store, Client), String> {
            let store = Store::open(&db).map_err(|e| e.to_string())?;
            let client = Client::new(ProviderConfig::from_env()).map_err(|e| e.to_string())?;
            Ok((store, client))
        })();
        let (store, client) = match prepared {
            Ok(v) => v,
            Err(e) => {
                handle.update(move |s: &mut Shell, _| s.fail(e));
                return;
            }
        };

        // run_turn calls back for every delta on this worker thread; each
        // callback hops to the UI thread through the handle.
        // Memory shares the database: one file, one schema, one truth.
        let memory = SqliteMemory::open(&db).ok();
        let scope =
            Scope::user(&std::env::var("ZERITH_USER").unwrap_or_else(|_| "local".to_string()));
        let reporter = handle.clone();
        let result = run_turn_cancellable(
            &store,
            &client,
            memory.as_ref().map(|m| m as &dyn MemoryBackend),
            Some(&scope),
            &session_id,
            &model,
            &prompt,
            &cancel,
            move |event| {
                reporter.update(move |s: &mut Shell, _| s.apply(event));
            },
        );
        if let Err(e) = result {
            let msg = e.to_string();
            handle.update(move |s: &mut Shell, _| s.fail(msg));
        }
    });
}

/// One transcript entry.
///
/// A user turn is a bubble on the right, an answer is plain text on the
/// sheet: the two read differently at a glance without any colour coding.
fn line_row(index: usize, line: &Line, u: &Ui) -> El {
    match line.role {
        "user" => row()
            .key(format!("line-{index}"))
            .w_full()
            .justify_end()
            .child(
                col()
                    .max_w(560.0)
                    .padding(10.0)
                    .px(12.0)
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
        "thinking" => col()
            .key(format!("line-{index}"))
            .w_full()
            .gap(3.0)
            .child(eyebrow("raciocínio", u))
            .child(
                text(line.content.clone())
                    .wrap()
                    .font(ui::SMALL)
                    .color(u.faint),
            ),
        "error" => col()
            .key(format!("line-{index}"))
            .w_full()
            .gap(3.0)
            .padding(10.0)
            .radius(10.0)
            .fill(u.muted)
            .child(eyebrow("erro", u))
            .child(
                text(line.content.clone())
                    .wrap()
                    .font(ui::SMALL)
                    .color(u.content),
            ),
        _ => {
            let body = if line.content.is_empty() {
                "…".to_string()
            } else {
                line.content.clone()
            };
            col()
                .key(format!("line-{index}"))
                .w_full()
                .gap(3.0)
                .child(eyebrow(
                    if line.role == "assistant" {
                        "zerith"
                    } else if line.role == "system" {
                        "sistema"
                    } else {
                        "ferramenta"
                    },
                    u,
                ))
                .child(
                    text(body)
                        .wrap()
                        .font(ui::BODY)
                        .color(u.content)
                        .selectable(true),
                )
        }
    }
}

/// A section header: 11 pt, muted, uppercase-tracked in the source design.
fn eyebrow(label: &'static str, u: &Ui) -> El {
    text(label).font(ui::LABEL).color(u.muted_text)
}

/// A square icon button. `label` is what a screen reader announces, so it is
/// never optional here.
fn icon_button(
    key: &'static str,
    label: String,
    ic: &'static rune::Icon,
    u: &Ui,
    f: impl Fn(&mut Shell) + 'static,
) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .size(28.0, 28.0)
        .radius(8.0)
        .fill(u.muted)
        .hover(u.hover)
        .press(u.line_strong)
        .on_press(move |s: &mut Shell| f(s))
        .child(icon(ic, 16.0).color(u.secondary).center_y())
}

/// The one accent control in the window. OpenHuman fills its primary button
/// with `--primary-500` and writes `--content-inverted` on top.
fn primary_button(
    key: &'static str,
    label: String,
    ic: &'static rune::Icon,
    u: &Ui,
    f: impl Fn(&mut Shell) + 'static,
) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .size(28.0, 28.0)
        .radius(8.0)
        .fill(u.primary)
        .hover(u.primary_press)
        .press(u.primary_press)
        .on_press(move |s: &mut Shell| f(s))
        .child(icon(ic, 16.0).color(u.inverted).center_y())
}

/// The session column, on the chrome. Header pinned, list scrolls, footer
/// pinned — the layout OpenHuman's `AppSidebar` describes.
fn sidebar(s: &Shell, u: &Ui) -> El {
    let mut list = col().key("sessions").w_full().gap(2.0);
    for (i, r) in s.sessions.iter().enumerate() {
        let on = i == s.selected;
        list = list.child(
            el().key(format!("s-{i}"))
                .role("button")
                .label(r.title.clone())
                .checked(on)
                .focusable(true)
                .w_full()
                .padding(8.0)
                .px(10.0)
                .radius(8.0)
                .fill(if on { u.muted } else { u.chrome })
                .hover(u.hover)
                .on_press(move |s: &mut Shell| s.select(i))
                .child(
                    col()
                        .w_full()
                        .gap(2.0)
                        .child(
                            text(r.title.clone())
                                .wrap()
                                .max_lines(2)
                                .font(ui::SMALL)
                                .color(if on { u.content } else { u.secondary }),
                        )
                        .child(text(r.source.clone()).font(ui::LABEL).color(u.faint)),
                ),
        );
    }
    col()
        .key("sidebar")
        .w(SIDEBAR)
        .h_full()
        .fill(u.chrome)
        .child(
            col()
                .w_full()
                .h_full()
                .padding(12.0)
                .gap(10.0)
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .gap(6.0)
                        .child(text("Zerith").font(ui::TITLE).color(u.content).grow(1.0))
                        .child(icon_button(
                            "new",
                            "Nova conversa".to_string(),
                            ui::icon_new(),
                            u,
                            |s: &mut Shell| s.new_chat(),
                        )),
                )
                .child(
                    list.scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .grow(1.0)
                    .min_h(0.0),
                )
                .child(memory_panel(s, u))
                .child(plan_panel(s, u))
                .child(skills_panel(s, u)),
        )
}

/// What the user is allowed to see and delete about their own memory. A fact
/// shown here is one the turn really recalls — no separate preview store.
fn memory_panel(s: &Shell, u: &Ui) -> El {
    let mut rows = col().key("memory-rows").w_full().gap(2.0);
    for f in &s.facts {
        let id = f.id;
        rows = rows.child(
            row()
                .key(format!("m-{id}"))
                .w_full()
                .gap(4.0)
                .child(
                    text(f.text.clone())
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.secondary)
                        .grow(1.0),
                )
                .child(
                    el().role("button")
                        .label(format!("Apagar fato {id}"))
                        .focusable(true)
                        .size(18.0, 18.0)
                        .radius(6.0)
                        .fill(u.chrome)
                        .hover(u.hover)
                        .on_press(move |s: &mut Shell| s.forget(id))
                        .child(text("×").font(ui::SMALL).color(u.faint).center_y()),
                ),
        );
    }
    if s.facts.is_empty() {
        rows = rows.child(
            text("Diga \"lembre que …\" para guardar um fato.")
                .wrap()
                .font(ui::SMALL)
                .color(u.faint),
        );
    }
    col()
        .key("memory")
        .w_full()
        .gap(6.0)
        .child(eyebrow("memória", u))
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .max_h(120.0)
            .shrink()
            .min_h(0.0),
        )
}

/// What is installed. A name here means the directory parsed; it says nothing
/// about trust, which is why the window never enables one by itself.
fn skills_panel(s: &Shell, u: &Ui) -> El {
    let mut rows = col().key("skill-rows").w_full().gap(2.0);
    if s.skills.is_empty() {
        rows = rows.child(
            text("Nenhuma skill instalada.")
                .wrap()
                .font(ui::SMALL)
                .color(u.faint),
        );
    }
    for (i, name) in s.skills.iter().enumerate() {
        rows = rows.child(
            text(name.clone())
                .key(format!("sk-{i}"))
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary),
        );
    }
    col()
        .key("skills")
        .w_full()
        .gap(6.0)
        .child(eyebrow("skills", u))
        .child(rows.shrink().min_h(0.0))
}

/// What a plan is doing right now. A plan with no unfinished work is absent
/// rather than shown as finished work the user cannot act on.
fn plan_panel(s: &Shell, u: &Ui) -> El {
    if s.plan_tasks.is_empty() {
        return col().key("plan-none");
    }
    let mut rows = col().key("plan-rows").w_full().gap(2.0);
    for (i, (state, title, plan)) in s.plan_tasks.iter().enumerate() {
        rows = rows.child(
            text(format!("{state}  {title}"))
                .key(format!("pt-{i}"))
                .label(format!("{title}, estado {state}, plano {plan}"))
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary),
        );
    }
    col()
        .key("plan")
        .w_full()
        .gap(6.0)
        .child(eyebrow("execução", u))
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .max_h(90.0)
            .shrink()
            .min_h(0.0),
        )
}

pub fn view(s: &Shell, _: &mut Cx<Shell>) -> El {
    let u = ui::dark();

    let mut transcript = col().key("transcript").w_full().gap(18.0);
    for (i, line) in s.lines.iter().enumerate() {
        transcript = transcript.child(line_row(i, line, &u));
    }
    if s.lines.is_empty() {
        transcript = transcript.child(
            col()
                .w_full()
                .gap(4.0)
                .child(text("Nova conversa").font(ui::TITLE).color(u.content))
                .child(
                    text("Pergunte algo. O turno roda aqui, na sua máquina.")
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.muted_text),
                ),
        );
    }

    let title = s
        .sessions
        .get(s.selected)
        .map(|r| r.title.clone())
        .unwrap_or_else(|| "Nova conversa".to_string());

    let field = text_input(&s.draft, ui::BODY)
        .key("draft")
        .grow(1.0)
        .color(u.content)
        .label("Mensagem para o Zerith")
        .placeholder("Pergunte algo…", u.faint)
        .on_edit(|s: &mut Shell| &mut s.draft)
        .on_submit(|s: &mut Shell, _: &mut Tx| send(s));

    let action = if s.busy {
        icon_button(
            "stop",
            "Parar o turno".to_string(),
            ui::icon_stop(),
            &u,
            |s: &mut Shell| s.stop(),
        )
    } else {
        primary_button(
            "send",
            "Enviar a mensagem".to_string(),
            ui::icon_send(),
            &u,
            |s: &mut Shell| send(s),
        )
    };

    let composer = col()
        .key("composer")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .min_h(44.0)
                .px(12.0)
                .py(6.0)
                .gap(8.0)
                .center_y()
                .radius(12.0)
                .fill(u.muted)
                .border(1.0, u.line)
                .child(field)
                .child(action),
        )
        .child(
            row()
                .w_full()
                .gap(6.0)
                .child(
                    text(if s.model.is_empty() {
                        "sem modelo".to_string()
                    } else {
                        s.model.clone()
                    })
                    .font(ui::LABEL)
                    .color(u.faint)
                    .grow(1.0),
                )
                .child(text(s.status.clone()).font(ui::LABEL).color(u.faint)),
        );

    // The content sheet: inset on every side, rounded, hairline edge — the
    // `SidebarInset` geometry from OpenHuman's shell.
    let sheet = col()
        .key("sheet")
        .grow(1.0)
        .h_full()
        .gap(12.0)
        .padding(20.0)
        .radius(SHEET_RADIUS)
        .fill(u.surface)
        .border(1.0, u.line)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(text(title).font(ui::TITLE).color(u.content).grow(1.0)),
        )
        .child(rule(1.0, u.line).w_full())
        .child(
            transcript
                .scroll(ScrollSpec { stick_bottom: true })
                .grow(1.0)
                .min_h(0.0),
        )
        .child(composer);

    col().w_full().h_full().fill(u.canvas).child(
        row()
            .w_full()
            .h_full()
            .child(sidebar(s, &u))
            .child(col().grow(1.0).h_full().padding(INSET).child(sheet)),
    )
}
