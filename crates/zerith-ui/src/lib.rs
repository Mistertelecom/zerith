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

pub mod agentwork;
pub mod brand;
mod folders;
pub mod mediaout;
pub mod screens;
mod ui;
mod widgets;
mod wire;

use std::path::{Path, PathBuf};

use rune::{
    col, el, icon, row, rule, text, AppHandle, Color, Cx, El, GrowthSpec, KeyEvent, ScrollSpec, Tx,
};
use rune_kit::{Badge, BadgeVariant};
use ui::Ui;
use zerith_providers::{Cancel, ProviderConfig};
use zerith_runtime::RunEvent;
use zerith_sessions::{Session, Store};

pub use screens::agents::{AgentsState, NewAgent};
pub use screens::chat::{Approval, ChatState, Decision, Line};
pub use screens::history::{
    day_bucket, filter_rows, group_by_day, Bucket, HistoryState, SourceFilter,
};
pub use screens::media::MediaState;
pub use screens::memory::MemoryState;
pub use screens::providers::ProvidersState;
pub use screens::settings::SettingsState;
pub use screens::skills::SkillsState;
pub use widgets::Load;

/// The navigation column: 240 pt, like `SIDEBAR_WIDTH` in OpenHuman's shell.
const SIDEBAR: f32 = 248.0;
/// The title band the window draws for itself, in points.
const TITLEBAR: f32 = 44.0;
/// The sheet's inset from the window edge, OpenHuman's `m-2`.
const INSET: f32 = 8.0;
/// A sheet narrower than this many points is `compact` (a ~915 pt window).
const COMPACT_SHEET: f32 = 660.0;
/// `rounded-2xl`.
const SHEET_RADIUS: f32 = ui::R_LG;
/// Sessions read per page. The list is virtualized, so this bounds memory and
/// query time, not what is drawn.
pub const SESSION_PAGE: usize = 200;

/// The eight places the window can show. Order is the Ctrl+1..8 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Chat,
    History,
    Agents,
    Skills,
    Media,
    Memory,
    Providers,
    Settings,
}

impl Screen {
    pub const ALL: [Screen; 8] = [
        Screen::Chat,
        Screen::History,
        Screen::Agents,
        Screen::Skills,
        Screen::Media,
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
            Screen::Media => "Mídia",
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
            Screen::Media => ui::icon_media(),
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
    pub media: MediaState,
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
    /// Where generated files live: only files under it are shown or opened.
    pub media_dir: PathBuf,
    /// Decoder for the pictures of generated media (local files only).
    pub images: std::rc::Rc<mediaout::Images>,
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
        let accounts = wire::configured_accounts(&root);
        let routing = wire::load_routing(&root);
        let token_saver = wire::load_token_saver(&root);
        let env_model = std::env::var("ZERITH_MODEL")
            .ok()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let model = env_model
            .or_else(|| {
                routing
                    .main_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .or_else(|| providers.first().and_then(|p| p.default_model.clone()))
            .unwrap_or_default();
        let mut shell = Self {
            screen: Screen::Chat,
            sessions: Vec::new(),
            sessions_limit: SESSION_PAGE,
            chat: ChatState::new(model),
            history: HistoryState::default(),
            memory: MemoryState::new(wire::memory_paused(&db)),
            skills: SkillsState::default(),
            media: MediaState::open(&root),
            providers: ProvidersState::with_gateway(providers, accounts, routing),
            settings: SettingsState::new(prefs.profile, token_saver),
            agents: AgentsState::default(),
            plan_tasks: Vec::new(),
            theme: prefs.theme,
            reduce_motion: prefs.reduce_motion,
            inline: false,
            media_dir: wire::media_dir(&root),
            images: std::rc::Rc::new(mediaout::Images::new({
                let h = me.clone();
                std::sync::Arc::new(move || h.wake())
            })),
            db,
            me,
        };
        let handle = shell.me.clone();
        shell
            .media
            .set_waker(std::sync::Arc::new(move || handle.wake()));
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
                self.chat.status = folders::with_cause(
                    &self.root(),
                    &format!("Banco indisponível ({}): {e}", self.db.display()),
                );
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
            Screen::Media => screens::media::opened(self),
            // Accounts (cooldown badges) and Usage both read it.
            Screen::Providers => screens::providers::load_usage(self),
            Screen::Settings => {
                if self.settings.storage.is_idle() {
                    self.load_storage();
                }
                screens::settings::load_usage(self);
            }
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

    /// Picks an execution profile; the next turn and plan use its limits.
    pub fn set_profile(&mut self, p: Profile) {
        self.settings.profile = p;
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
/// Ctrl+1..8 switch screens, Ctrl+N starts a chat, Ctrl+K focuses search and
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
        .h(36.0)
        .pl(4.0)
        .pr(10.0)
        .gap(10.0)
        .center_y()
        .radius(ui::R_SM)
        .fill(if on { u.selected } else { u.chrome })
        .hover(u.hover)
        .press(u.active)
        .on_press(move |s: &mut Shell| s.navigate(screen))
        // The accent bar marks where you are; idle rows keep the same indent.
        .child(el().size(3.0, 16.0).radius(2.0).fill(if on {
            u.accent
        } else {
            Color::TRANSPARENT
        }))
        .child(
            el().size(26.0, 26.0)
                .radius(7.0)
                .center()
                .fill(if on { u.layer2 } else { Color::TRANSPARENT })
                .child(icon(screen.icon(), 16.0).color(if on { u.content } else { u.muted_text })),
        )
        .child(
            text(screen.label())
                .font(ui::SMALL)
                .color(if on { u.content } else { u.secondary })
                .grow(1.0),
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
                    .fill(if on { u.selected } else { u.chrome })
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
        .px(12.0)
        .pb(12.0)
        .pt(6.0)
        .gap(10.0)
        .child(
            row()
                .w_full()
                .h(TITLEBAR - 12.0)
                .pl(6.0)
                .center_y()
                .gap(10.0)
                .window_drag()
                .child(brand::zerith(24.0, &s.images))
                .child(
                    text("Zerith")
                        .font(rune::TextStyle::new(15.0, 650.0).lh(20.0))
                        .color(u.content)
                        .grow(1.0),
                )
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
    let mut u = ui::pick(dark);
    // Under ~900 pt of window (the sheet is the window minus the sidebar and
    // insets) screens stack instead of sitting side by side.
    u.compact = cx
        .layout_with("sheet", |r| r[2] < COMPACT_SHEET)
        .unwrap_or(false);
    cx.foreground(u.content);
    cx.caption_color(u.secondary);
    let insets = cx.chrome_insets();
    cx.focus_ring(2.0, 2.0, u.accent.alpha(0.7));

    let mut layer = Vec::new();
    let body = match s.screen {
        Screen::Chat => screens::chat::view(s, &u, &mut layer),
        Screen::History => screens::history::view(s, &u),
        Screen::Agents => screens::agents::view(s, &u),
        Screen::Skills => screens::skills::view(s, &u),
        Screen::Media => screens::media::view(s, &u),
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

    // Hover and press fade on the design's one curve, 100 ms.
    let base = col()
        .w_full()
        .h_full()
        .fill(u.canvas)
        .hover_motion_within(ui::FAST, ui::FAST)
        .child(
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
                        .child(
                            row()
                                .key("titleband")
                                .w_full()
                                .h(TITLEBAR - INSET)
                                .pr(insets.trailing)
                                .window_drag(),
                        )
                        .child(
                            col()
                                .grow(1.0)
                                .min_h(0.0)
                                .w_full()
                                .px(INSET)
                                .pb(INSET)
                                .child(sheet),
                        ),
                ),
        );
    rune_kit::root(base, layer)
}

/// Once per frame: takes what the typing timers have landed. Costs a lock
/// per timer and returns false when nothing is due, so an idle window draws
/// nothing.
pub fn poll(s: &mut Shell, _clock: f64, _tx: &mut Tx) -> bool {
    screens::media::poll(s)
}

/// The spec main and tests share, so the keyboard map is the same in both.
pub fn spec() -> rune::AppSpec<Shell> {
    rune::AppSpec {
        on_key: Some(on_key),
        poll: Some(poll),
        min_size: Some((760.0, 520.0)),
        ..rune::AppSpec::new("Zerith", (1180.0, 760.0), || unreachable!(), view)
    }
}

/// The app with its own title band instead of the native frame. On Windows
/// Rune paints the caption buttons over the band's trailing edge and the window
/// keeps its resize edges and Aero Snap; `cx.chrome_insets()` says what the
/// buttons take. The band is the top of the sidebar and of the content column.
pub fn app_with(spec: rune::AppSpec<Shell>) -> rune::App<Shell> {
    rune::App::new(spec).titlebar(rune::Titlebar::new(TITLEBAR))
}

/// Test hook for the media opener's guard: it refuses anything outside `dir`
/// before it asks the OS to open it.
pub fn open_media_for_test(dir: &Path, path: &Path) -> Result<(), String> {
    wire::open_media(dir, path)
}

/// Used by `Cancel` users outside the crate (tests drive `stop`).
pub type CancelToken = Cancel;
/// The provider the chat will talk to.
pub fn active_provider(s: &Shell) -> ProviderConfig {
    s.providers.active()
}
