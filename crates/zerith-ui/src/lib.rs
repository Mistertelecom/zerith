//! Native shell. Rune draws every pixel; there is no webview, no Chromium and
//! no JavaScript in this process.
//!
//! The layout copies OpenHuman's shell deliberately (`ContentSurface` and
//! `AppSidebar` in `app/src/components/layout/shell/`): a tinted chrome frame,
//! the navigation column sitting on it, and one inset content sheet with a 2xl
//! radius and a hairline edge. `view` is a pure function of `Shell` and builds
//! only the open screen. The database, the disk and the network run on worker
//! threads and come back through `AppHandle::update`, so nothing slow ever
//! happens inside a frame.

pub mod screens;
mod ui;
mod widgets;
mod wire;

use std::path::{Path, PathBuf};

use rune::{
    col, el, icon, row, rule, text, AppHandle, Cx, El, GrowthSpec, KeyEvent, ScrollSpec, Tx,
};
use rune_kit::{Badge, BadgeVariant};
use ui::Ui;
use zerith_providers::{Cancel, ProviderConfig};
use zerith_runtime::RunEvent;
use zerith_sessions::{Session, Store};

pub use screens::agents::AgentsState;
pub use screens::chat::{Approval, ChatState, Decision, Line};
pub use screens::history::{
    day_bucket, filter_rows, group_by_day, Bucket, HistoryState, SourceFilter,
};
pub use screens::memory::MemoryState;
pub use screens::providers::ProvidersState;
pub use screens::settings::SettingsState;
pub use screens::skills::SkillsState;
pub use widgets::Load;

/// The navigation column: 240 pt, like `SIDEBAR_WIDTH` in OpenHuman's shell.
const SIDEBAR: f32 = 240.0;
/// The sheet's inset from the window edge, OpenHuman's `m-2`.
const INSET: f32 = 8.0;
/// `rounded-2xl`.
const SHEET_RADIUS: f32 = 16.0;
/// Sessions read per page. The list is virtualized, so this bounds memory and
/// query time, not what is drawn.
pub const SESSION_PAGE: usize = 200;

/// The seven places the window can show. Order is the Ctrl+1..7 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Chat,
    History,
    Agents,
    Skills,
    Memory,
    Providers,
    Settings,
}

impl Screen {
    pub const ALL: [Screen; 7] = [
        Screen::Chat,
        Screen::History,
        Screen::Agents,
        Screen::Skills,
        Screen::Memory,
        Screen::Providers,
        Screen::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Screen::Chat => "Chat",
            Screen::History => "Histórico",
            Screen::Agents => "Agentes",
            Screen::Skills => "Skills",
            Screen::Memory => "Memória",
            Screen::Providers => "Provedores",
            Screen::Settings => "Configurações",
        }
    }

    fn icon(self) -> &'static rune::Icon {
        match self {
            Screen::Chat => ui::icon_chat(),
            Screen::History => ui::icon_history(),
            Screen::Agents => ui::icon_agents(),
            Screen::Skills => ui::icon_skills(),
            Screen::Memory => ui::icon_memory(),
            Screen::Providers => ui::icon_providers(),
            Screen::Settings => ui::icon_settings(),
        }
    }
}

/// Appearance. `System` follows the OS through `Cx::system_dark`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }
    /// Unknown or missing values fall back to dark, the window's original look.
    pub fn parse(s: Option<&str>) -> Theme {
        match s {
            Some("light") => Theme::Light,
            Some("system") => Theme::System,
            _ => Theme::Dark,
        }
    }
}

/// How much the runtime may spend per turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Economy,
    Balanced,
    Performance,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Economy => "economy",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
        }
    }
    pub fn parse(s: Option<&str>) -> Profile {
        match s {
            Some("economy") => Profile::Economy,
            Some("performance") => Profile::Performance,
            _ => Profile::Balanced,
        }
    }
}

/// One conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub source: String,
    pub updated_at: i64,
    pub archived: bool,
}

pub struct Shell {
    pub screen: Screen,
    /// Every session read so far, newest first, archived included. The chat
    /// column hides archived ones; the history screen filters them.
    pub sessions: Vec<Row>,
    pub sessions_limit: usize,
    pub chat: ChatState,
    pub history: HistoryState,
    pub memory: MemoryState,
    pub skills: SkillsState,
    pub providers: ProvidersState,
    pub settings: SettingsState,
    pub agents: AgentsState,
    /// Tasks from plans that have unfinished work.
    pub plan_tasks: Vec<(String, String, String)>,
    pub theme: Theme,
    pub reduce_motion: bool,
    /// When true, worker jobs run on the calling thread. Tests set it so a
    /// state transition finishes before the assertion.
    pub inline: bool,
    db: PathBuf,
    me: AppHandle<Shell>,
}

impl Shell {
    /// Opens the window against the real database.
    ///
    /// A broken database still opens a window: refusing to start would hide
    /// the reason, and `zerith doctor` is the command that reports it.
    pub fn open(db: PathBuf, me: AppHandle<Shell>) -> Self {
        let root = db.parent().map(Path::to_path_buf).unwrap_or_default();
        let prefs = wire::load_prefs(&root);
        let providers = wire::configured_providers(&root);
        let model = providers
            .first()
            .and_then(|p| p.default_model.clone())
            .unwrap_or_default();
        let mut shell = Self {
            screen: Screen::Chat,
            sessions: Vec::new(),
            sessions_limit: SESSION_PAGE,
            chat: ChatState::new(model),
            history: HistoryState::default(),
            memory: MemoryState::new(wire::memory_paused(&db)),
            skills: SkillsState::default(),
            providers: ProvidersState::new(providers),
            settings: SettingsState::new(prefs.profile),
            agents: AgentsState::default(),
            plan_tasks: Vec::new(),
            theme: prefs.theme,
            reduce_motion: prefs.reduce_motion,
            inline: false,
            db,
            me,
        };
        shell.reload();
        shell
    }

    pub fn db(&self) -> &Path {
        &self.db
    }

    /// The data directory: the database's folder.
    pub fn root(&self) -> PathBuf {
        self.db.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    /// Kept for callers that only know "dark or not".
    pub fn dark(&self) -> bool {
        self.theme != Theme::Light
    }

    /// Runs `job` off the UI thread and lands its result with `land`.
    pub(crate) fn work<T: Send + 'static>(
        &mut self,
        job: impl FnOnce() -> T + Send + 'static,
        land: impl FnOnce(&mut Shell, T) + Send + 'static,
    ) {
        if self.inline {
            let v = job();
            land(self, v);
            return;
        }
        let me = self.me.clone();
        std::thread::spawn(move || {
            let v = job();
            me.update(move |s: &mut Shell, _| land(s, v));
        });
    }

    /// The first read, before the first frame: the chat column and the open
    /// conversation. Other screens load when they are opened.
    fn reload(&mut self) {
        let store = match Store::open(&self.db) {
            Ok(s) => s,
            Err(e) => {
                self.chat.status = format!("Banco indisponível ({}): {e}", self.db.display());
                return;
            }
        };
        self.sessions = store
            .list_sessions(self.sessions_limit)
            .unwrap_or_default()
            .into_iter()
            .map(row_of)
            .collect();
        let mut rows = Vec::new();
        for plan in store.unfinished_plans().unwrap_or_default() {
            for t in store.tasks(&plan).unwrap_or_default() {
                rows.push((t.state.as_str().to_string(), t.title, plan.clone()));
            }
        }
        self.plan_tasks = rows;
        self.chat.selected = self.visible_sessions().first().map(|r| r.id.clone());
        self.load_selected();
    }

    /// Sessions the chat column lists: not archived.
    pub fn visible_sessions(&self) -> Vec<&Row> {
        self.sessions.iter().filter(|r| !r.archived).collect()
    }

    /// Re-reads the session list off the UI thread.
    pub fn refresh_sessions(&mut self) {
        let db = self.db.clone();
        let limit = self.sessions_limit;
        self.work(
            move || {
                Store::open(&db)
                    .and_then(|st| st.list_sessions(limit))
                    .map_err(|e| e.to_string())
            },
            |s, r| match r {
                Ok(list) => {
                    s.sessions = list.into_iter().map(row_of).collect();
                    s.history.rebuild(&s.sessions);
                }
                Err(e) => s.chat.status = e,
            },
        );
    }

    pub fn more_sessions(&mut self) {
        self.sessions_limit += SESSION_PAGE;
        self.refresh_sessions();
    }

    /// Switches screen, loading its data the first time it opens.
    pub fn navigate(&mut self, to: Screen) {
        self.screen = to;
        match to {
            Screen::History => {
                self.history.rebuild(&self.sessions);
                self.refresh_sessions();
            }
            Screen::Memory if self.memory.facts.is_idle() => self.load_facts(),
            Screen::Skills if self.skills.installed.is_idle() => self.load_skills(),
            Screen::Agents if self.agents.list.is_idle() => self.load_agents(),
            Screen::Settings if self.settings.storage.is_idle() => self.load_storage(),
            _ => {}
        }
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.theme = t;
        self.save_prefs();
    }

    /// Flips between light and dark (System resolves to dark here).
    pub fn toggle_theme(&mut self) {
        let t = if self.theme == Theme::Light {
            Theme::Dark
        } else {
            Theme::Light
        };
        self.set_theme(t);
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        self.reduce_motion = on;
        self.save_prefs();
    }

    pub(crate) fn save_prefs(&self) {
        wire::save_prefs(
            &self.root(),
            wire::UiPrefs {
                theme: self.theme,
                reduce_motion: self.reduce_motion,
                profile: self.settings.profile,
            },
            self.inline,
        );
    }

    /// Applies one worker event.
    pub fn apply(&mut self, event: RunEvent) {
        screens::chat::apply(self, event);
    }

    /// Stops the turn. The question already stored stays.
    pub fn stop(&mut self) {
        screens::chat::stop(self);
    }

    pub fn new_chat(&mut self) {
        self.screen = Screen::Chat;
        screens::chat::new_chat(self);
    }

    pub fn select_session(&mut self, id: &str) {
        self.screen = Screen::Chat;
        if self.chat.selected.as_deref() != Some(id) {
            self.chat.selected = Some(id.to_string());
            self.load_selected();
        }
    }

    pub(crate) fn load_selected(&mut self) {
        screens::chat::load_selected(self);
    }

    pub fn toggle_open(&mut self, i: usize) {
        if !self.chat.open.remove(&i) {
            self.chat.open.insert(i);
        }
    }

    /// A tool asks for permission. Tools already allowed for this session are
    /// answered at once and never show a card.
    pub fn request_approval(&mut self, a: Approval) {
        screens::chat::request_approval(self, a);
    }

    pub fn decide(&mut self, d: Decision) {
        screens::chat::decide(self, d);
    }

    pub fn load_facts(&mut self) {
        screens::memory::load(self);
    }

    pub fn forget(&mut self, id: i64) {
        screens::memory::forget(self, id);
    }

    pub fn load_skills(&mut self) {
        screens::skills::load(self);
    }

    pub fn load_agents(&mut self) {
        screens::agents::load(self);
    }

    pub fn load_storage(&mut self) {
        screens::settings::load_storage(self);
    }
}

pub(crate) fn row_of(s: Session) -> Row {
    Row {
        title: if s.title.trim().is_empty() {
            "(sem título)".to_string()
        } else {
            s.title
        },
        id: s.id,
        source: s.source.as_str().to_string(),
        updated_at: s.updated_at,
        archived: s.archived,
    }
}

/// The window's keyboard map, for keys no focused control took:
/// Ctrl+1..7 switch screens, Ctrl+N starts a chat, Ctrl+K focuses search and
/// Escape stops the turn (or closes what is open).
pub fn on_key(s: &mut Shell, ev: &KeyEvent, tx: &mut Tx) -> bool {
    for (i, screen) in Screen::ALL.iter().enumerate() {
        let digit = char::from(b'1' + i as u8);
        if ev.is_command(digit) {
            s.navigate(*screen);
            return true;
        }
    }
    if ev.is_command('n') {
        s.new_chat();
        tx.focus_when_shown("draft", true);
        return true;
    }
    if ev.is_command('k') {
        let key = if s.screen == Screen::Memory {
            "memory-q"
        } else {
            s.navigate(Screen::History);
            "history-q"
        };
        tx.focus_when_shown(key, true);
        return true;
    }
    if ev.key == rune::Key::Escape {
        if s.chat.model_menu || s.chat.agent_menu {
            s.chat.model_menu = false;
            s.chat.agent_menu = false;
            return true;
        }
        if s.chat.cancel.is_some() {
            s.stop();
            return true;
        }
    }
    false
}

/// One navigation entry: icon, label, a muted fill when current.
fn nav_item(screen: Screen, current: Screen, u: &Ui) -> El {
    let on = screen == current;
    let index = Screen::ALL.iter().position(|s| *s == screen).unwrap_or(0) + 1;
    row()
        .key(format!("nav-{}", screen.label()))
        .role("tab")
        .label(screen.label())
        .shortcut_hint(format!("Ctrl+{index}"))
        .checked(on)
        .focusable(true)
        .focus_selected(on)
        .w_full()
        .h(30.0)
        .px(8.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .fill(if on { u.muted } else { u.chrome })
        .hover(u.hover)
        .press(u.line_strong)
        .on_press(move |s: &mut Shell| s.navigate(screen))
        .child(icon(screen.icon(), 16.0).color(if on { u.content } else { u.muted_text }))
        .child(
            text(screen.label())
                .font(ui::SMALL)
                .color(if on { u.content } else { u.secondary }),
        )
}

/// What a plan is doing right now. Absent when nothing is unfinished.
fn plan_panel(s: &Shell, u: &Ui) -> El {
    let mut rows = col().key("plan-rows").w_full().gap(4.0);
    for (i, (state, title, plan)) in s.plan_tasks.iter().enumerate() {
        let variant = if state == "running" {
            BadgeVariant::Accent
        } else {
            BadgeVariant::Neutral
        };
        rows = rows.child(
            row()
                .key(format!("pt-{i}"))
                .label(format!("{title}, estado {state}, plano {plan}"))
                .w_full()
                .center_y()
                .gap(6.0)
                .child(
                    Badge::new(state.clone())
                        .variant(variant)
                        .build(&ui::kit(u)),
                )
                .child(
                    text(title.clone())
                        .truncate()
                        .font(ui::SMALL)
                        .color(u.secondary)
                        .grow(1.0)
                        .min_w(0.0),
                ),
        );
    }
    col()
        .key("plan")
        .w_full()
        .gap(6.0)
        .child(widgets::eyebrow("Execução", u))
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .max_h(90.0)
            .shrink()
            .min_h(0.0),
        )
}

/// The conversation list, only on the Chat screen. Virtualized: a thousand
/// sessions mount the same twenty rows as ten.
fn session_list(s: &Shell, u: Ui) -> El {
    let ids: Vec<String> = s.visible_sessions().iter().map(|r| r.id.clone()).collect();
    if ids.is_empty() {
        return col().key("sessions-empty").w_full().grow(1.0).child(
            text("Nenhuma conversa ainda.")
                .font(ui::SMALL)
                .color(u.faint),
        );
    }
    let more = s.sessions.len() >= s.sessions_limit;
    let n = ids.len();
    let list = col()
        .key("sessions")
        .w_full()
        .role("list")
        .label("Conversas")
        .growth(GrowthSpec::default())
        .virtual_rows(
            n,
            34.0,
            |s: &Shell, i| {
                s.visible_sessions()
                    .get(i)
                    .map(|r| format!("s-{}", r.id))
                    .unwrap_or_default()
                    .into()
            },
            move |s: &Shell, i| {
                let rows = s.visible_sessions();
                let Some(r) = rows.get(i) else {
                    return col();
                };
                let on = s.chat.selected.as_deref() == Some(r.id.as_str());
                let id = r.id.clone();
                el().role("button")
                    .label(r.title.clone())
                    .checked(on)
                    .focusable(true)
                    .w_full()
                    .h(32.0)
                    .mb(2.0)
                    .px(10.0)
                    .radius(8.0)
                    .center_y()
                    .fill(if on { u.muted } else { u.chrome })
                    .hover(u.hover)
                    .on_press(move |s: &mut Shell| s.select_session(&id))
                    .child(
                        row()
                            .w_full()
                            .gap(6.0)
                            .center_y()
                            .child(
                                text(r.title.clone())
                                    .truncate()
                                    .font(ui::SMALL)
                                    .color(if on { u.content } else { u.secondary })
                                    .grow(1.0)
                                    .min_w(0.0),
                            )
                            .child(if r.source == "native" {
                                el()
                            } else {
                                text(r.source.clone()).font(ui::LABEL).color(u.faint)
                            }),
                    )
            },
        );
    let mut body = col().w_full().child(list);
    if more {
        body = body.child(widgets::button(
            "sessions-more",
            "Carregar mais",
            widgets::Tone::Plain,
            true,
            &u,
            |s, _| s.more_sessions(),
        ));
    }
    body.scroll(ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
    .min_h(0.0)
}

fn sidebar(s: &Shell, u: &Ui) -> El {
    let mut nav = col()
        .key("nav")
        .role("tablist")
        .label("Navegação")
        .focus_group()
        .w_full()
        .gap(1.0);
    for screen in Screen::ALL {
        nav = nav.child(nav_item(screen, s.screen, u));
    }
    let mut column = col()
        .w_full()
        .h_full()
        .padding(12.0)
        .gap(12.0)
        .child(
            row()
                .w_full()
                .h(28.0)
                .pl(6.0)
                .center_y()
                .gap(6.0)
                .child(text("Zerith").font(ui::TITLE).color(u.content).grow(1.0))
                .child(widgets::icon_button(
                    "new",
                    "Nova conversa (Ctrl+N)",
                    ui::icon_new(),
                    u,
                    |s: &mut Shell| s.new_chat(),
                )),
        )
        .child(nav);
    if s.screen == Screen::Chat {
        column = column
            .child(rule(1.0, u.line).w_full())
            .child(widgets::eyebrow("Conversas", u).pl(8.0))
            .child(session_list(s, *u));
        if !s.plan_tasks.is_empty() {
            column = column.child(plan_panel(s, u));
        }
    }
    col()
        .key("sidebar")
        .w(SIDEBAR)
        .h_full()
        .fill(u.chrome)
        .child(column)
}

pub fn view(s: &Shell, cx: &mut Cx<Shell>) -> El {
    let dark = match s.theme {
        Theme::Dark => true,
        Theme::Light => false,
        Theme::System => cx.system_dark(),
    };
    if s.reduce_motion {
        cx.reduced_motion(true);
    }
    let u = ui::pick(dark);
    cx.foreground(u.content);
    cx.focus_ring(2.0, 2.0, u.primary.alpha(0.55));

    let mut layer = Vec::new();
    let body = match s.screen {
        Screen::Chat => screens::chat::view(s, &u, &mut layer),
        Screen::History => screens::history::view(s, &u),
        Screen::Agents => screens::agents::view(s, &u),
        Screen::Skills => screens::skills::view(s, &u),
        Screen::Memory => screens::memory::view(s, &u),
        Screen::Providers => screens::providers::view(s, &u),
        Screen::Settings => screens::settings::view(s, &u, cx.system_reduced_motion()),
    };

    let sheet = col()
        .key("sheet")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .padding(20.0)
        .radius(SHEET_RADIUS)
        .fill(u.surface)
        .border(1.0, u.line)
        .child(body);

    let base = col().w_full().h_full().fill(u.canvas).child(
        row()
            .w_full()
            .h_full()
            .fill(u.chrome)
            .child(sidebar(s, &u))
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .h_full()
                    .padding(INSET)
                    .child(sheet),
            ),
    );
    rune_kit::root(base, layer)
}

/// The spec main and tests share, so the keyboard map is the same in both.
pub fn spec() -> rune::AppSpec<Shell> {
    rune::AppSpec {
        on_key: Some(on_key),
        min_size: Some((760.0, 520.0)),
        ..rune::AppSpec::new("Zerith", (1100.0, 720.0), || unreachable!(), view)
    }
}

/// Used by `Cancel` users outside the crate (tests drive `stop`).
pub type CancelToken = Cancel;
/// The provider the chat will talk to.
pub fn active_provider(s: &Shell) -> ProviderConfig {
    s.providers.active()
}
