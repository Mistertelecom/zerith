//! Native shell. Rune draws every pixel; there is no webview, no Chromium and
//! no JavaScript in this process.
//!
//! `view` is a pure function of `Shell`. Everything that talks to the outside
//! world — the database and the provider — runs on a worker thread and pushes
//! results back through `AppHandle::update`, so a slow model never stalls a
//! frame. That is the whole concurrency design and it is deliberate: no async
//! runtime, no shared locks in the view path.

use std::path::PathBuf;

use rune::tok::{self, radius, type_};
use rune::{col, el, row, text, text_input, AppHandle, Cx, El, ScrollSpec, TextEdit, Tx};
use zerith_memory::{Fact, MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Client, ProviderConfig};
use zerith_runtime::{run_turn_with_memory, RunEvent};
use zerith_sessions::{Session, Store};
use zerith_skills::discover;

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
    pub model: String,
    db: PathBuf,
    me: AppHandle<Shell>,
}

impl Shell {
    /// Opens the window against the real database.
    ///
    /// A broken database still opens a window: refusing to start would hide the
    /// reason, and `zerith doctor` is the command that reports it.
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
            model: cfg.default_model.clone().unwrap_or_default(),
            db,
            me,
        };
        shell.reload();
        shell
    }

    fn reload(&mut self) {
        let Ok(store) = Store::open(&self.db) else {
            self.status = "Banco de dados indisponível".into();
            return;
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
        if let Ok(store) = Store::open(&self.db) {
            let mut rows = Vec::new();
            for plan in store.unfinished_plans().unwrap_or_default() {
                for t in store.tasks(&plan).unwrap_or_default() {
                    rows.push((t.state.as_str().to_string(), t.title, plan.clone()));
                }
            }
            self.plan_tasks = rows;
        }
        self.facts = SqliteMemory::open(&self.db)
            .and_then(|m| m.list(&self.scope(), 50))
            .unwrap_or_default();
        self.load_selected();
    }

    /// The memory scope of this install. One operator, one scope.
    fn scope(&self) -> Scope {
        Scope::user(&std::env::var("ZERITH_USER").unwrap_or_else(|_| "local".to_string()))
    }

    /// Deletes a fact permanently. The backend keeps a tombstone so the dedupe
    /// key cannot resurrect the deleted text.
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
            self.status = "Nenhuma sessão ainda".into();
            return;
        };
        let id = s.id.clone();
        let title = s.title.clone();
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
                title
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

    /// Applies one event from the running turn. Called on the UI thread by
    /// `AppHandle::update`, never from the worker.
    pub fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::Delta(text) => match self.lines.last_mut() {
                Some(line) if line.role == "assistant" => line.content.push_str(&text),
                _ => self.lines.push(Line {
                    role: "assistant",
                    content: text,
                }),
            },
            // Reasoning is dimmed and labelled, so it is never mistaken for the
            // answer the model actually committed to.
            RunEvent::Reasoning(text) => self.lines.push(Line {
                role: "thinking",
                content: text,
            }),
            RunEvent::Done { .. } => {
                self.busy = false;
                self.reload();
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.busy = false;
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

    s.draft = TextEdit::new("");
    s.busy = true;
    s.status = "pensando…".into();
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
        let result = run_turn_with_memory(
            &store,
            &client,
            memory.as_ref().map(|m| m as &dyn MemoryBackend),
            Some(&scope),
            &session_id,
            &model,
            &prompt,
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

fn line_row(index: usize, line: &Line, t: &tok::Theme) -> El {
    let (label, color) = match line.role {
        "user" => ("você", tok::accent(0, true)),
        "assistant" => ("zerith", t.text),
        "thinking" => ("raciocínio", t.text_secondary),
        "system" => ("sistema", t.text_secondary),
        _ => ("erro", tok::accent(3, true)),
    };
    let body = if line.content.is_empty() {
        "…".to_string()
    } else {
        line.content.clone()
    };
    col()
        .key(format!("line-{index}"))
        .w_full()
        .gap(2.0)
        .child(text(label).font(type_::LABEL).color(t.text_secondary))
        .child(text(body).font(type_::BODY.lh(20.0)).color(color))
}

fn sidebar(s: &Shell, t: &tok::Theme) -> El {
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
                .w_full()
                .h(40.0)
                .radius(radius::SM)
                .px(10.0)
                .fill(if on { t.control_off } else { t.bg })
                .on_press(move |s: &mut Shell| s.select(i))
                .child(
                    col()
                        .w_full()
                        .gap(1.0)
                        .child(text(r.title.clone()).font(type_::BODY).color(t.text))
                        .child(
                            text(r.source.clone())
                                .font(type_::LABEL)
                                .color(t.text_secondary),
                        ),
                ),
        );
    }
    col()
        .key("sidebar")
        .w(240.0)
        .h_full()
        .padding(12.0)
        .gap(8.0)
        .fill(t.card)
        .child(
            row()
                .w_full()
                .center_y()
                .child(text("Sessões").font(type_::TITLE).color(t.text).grow(1.0))
                .child(
                    text("+")
                        .font(type_::TITLE)
                        .color(tok::accent(0, true))
                        .on_press(|s: &mut Shell| {
                            s.selected = s.sessions.len();
                            s.lines.clear();
                            s.draft = TextEdit::new("");
                            s.status = "Nova conversa".into();
                        }),
                ),
        )
        .child(
            list.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .shrink()
            .min_h(0.0),
        )
        .child(memory_panel(s, t))
        .child(plan_panel(s, t))
        .child(skills_panel(s, t))
}

/// What is installed. A name here means the directory parsed; it says nothing
/// about trust, which is why the window never enables one by itself.
fn skills_panel(s: &Shell, t: &tok::Theme) -> El {
    let mut rows = col().key("skill-rows").w_full().gap(2.0);
    if s.skills.is_empty() {
        rows = rows.child(
            text("Nenhuma skill instalada.")
                .font(type_::LABEL)
                .color(t.text_secondary),
        );
    }
    for (i, name) in s.skills.iter().enumerate() {
        rows = rows.child(
            text(name.clone())
                .key(format!("sk-{i}"))
                .font(type_::LABEL)
                .color(t.text),
        );
    }
    col()
        .key("skills")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .center_y()
                .child(text("Skills").font(type_::TITLE).color(t.text).grow(1.0)),
        )
        .child(rows.shrink().min_h(0.0))
}

/// What the user is allowed to see and delete about their own memory. A fact
/// shown here is one the turn really recalls — no separate preview store.
fn memory_panel(s: &Shell, t: &tok::Theme) -> El {
    let mut rows = col().key("memory-rows").w_full().gap(2.0);
    for f in &s.facts {
        let id = f.id;
        rows = rows.child(
            row()
                .key(format!("m-{id}"))
                .w_full()
                .gap(6.0)
                .center_y()
                .child(
                    text(f.text.clone())
                        .font(type_::LABEL)
                        .color(t.text)
                        .grow(1.0),
                )
                .child(
                    el().role("button")
                        .label(format!("Apagar fato {id}"))
                        .focusable(true)
                        .size(20.0, 20.0)
                        .radius(radius::SM)
                        .fill(t.bg)
                        .on_press(move |s: &mut Shell| s.forget(id))
                        .child(
                            text("x")
                                .font(type_::LABEL)
                                .color(t.text_secondary)
                                .center_y(),
                        ),
                ),
        );
    }
    if s.facts.is_empty() {
        rows = rows.child(
            text("Digite \"lembre que …\" para guardar um fato.")
                .font(type_::LABEL)
                .color(t.text_secondary),
        );
    }
    col()
        .key("memory")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .center_y()
                .child(text("Memória").font(type_::TITLE).color(t.text).grow(1.0)),
        )
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .shrink()
            .min_h(0.0),
        )
}

pub fn view(s: &Shell, _: &mut Cx<Shell>) -> El {
    let t = tok::theme(true);

    let mut transcript = col().key("transcript").w_full().gap(14.0);
    for (i, line) in s.lines.iter().enumerate() {
        transcript = transcript.child(line_row(i, line, &t));
    }
    if s.lines.is_empty() {
        transcript = transcript.child(
            text("Escreva e pressione Enter.")
                .font(type_::BODY)
                .color(t.text_secondary),
        );
    }

    let field = text_input(&s.draft, type_::BODY.lh(22.0))
        .key("draft")
        .grow(1.0)
        .color(t.text)
        .label("Mensagem para o Zerith")
        .placeholder("Pergunte algo…", t.text_secondary)
        .on_edit(|s: &mut Shell| &mut s.draft)
        .on_submit(|s: &mut Shell, _: &mut Tx| send(s));

    let title = s
        .sessions
        .get(s.selected)
        .map(|r| r.title.clone())
        .unwrap_or_else(|| "Nova conversa".to_string());

    col().w_full().h_full().fill(t.bg).child(
        row().w_full().child(sidebar(s, &t)).child(
            col()
                .grow(1.0)
                .h_full()
                .padding(18.0)
                .gap(10.0)
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .gap(8.0)
                        .child(text(title).font(type_::TITLE).color(t.text).grow(1.0))
                        .child(
                            text(if s.busy {
                                "pensando…".to_string()
                            } else {
                                s.status.clone()
                            })
                            .font(type_::LABEL)
                            .color(t.text_secondary),
                        ),
                )
                .child(
                    transcript
                        .scroll(ScrollSpec { stick_bottom: true })
                        .shrink()
                        .min_h(0.0),
                )
                .child(
                    row()
                        .w_full()
                        .h(52.0)
                        .px(12.0)
                        .gap(8.0)
                        .center_y()
                        .radius(radius::MD)
                        .fill(t.card)
                        .border(1.0, t.separator)
                        .child(field)
                        .child(
                            text("enviar")
                                .font(type_::LABEL)
                                .color(tok::accent(0, true))
                                .on_press(|s: &mut Shell| send(s)),
                        ),
                ),
        ),
    )
}

/// What a plan is doing right now. A plan with no unfinished work is absent
/// rather than shown as finished work the user cannot act on.
fn plan_panel(s: &Shell, t: &tok::Theme) -> El {
    if s.plan_tasks.is_empty() {
        return col().key("plan-none");
    }
    let mut rows = col().key("plan-rows").w_full().gap(2.0);
    for (i, (state, title, plan)) in s.plan_tasks.iter().enumerate() {
        rows = rows.child(
            text(format!("{state}  {title}"))
                .key(format!("pt-{i}"))
                .label(format!("{title}, estado {state}, plano {plan}"))
                .font(type_::LABEL)
                .color(t.text),
        );
    }
    col()
        .key("plan")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .center_y()
                .child(text("Execução").font(type_::TITLE).color(t.text).grow(1.0)),
        )
        .child(rows.shrink().min_h(0.0))
}
