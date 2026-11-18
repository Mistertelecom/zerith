//! What the Media screen holds: the connection, one `Slot` per kind (image,
//! video) and the backend the screen talks to.
//!
//! The backend is a handful of closures so the real one (OpenRouter, the OS
//! keyring, `settings.toml`) and a fake (tests, offline snapshots) are the same
//! shape. The window never sees the key after it is saved.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rune::{Latest, TextEdit, Waker};
use serde_json::Value;
use zerith_providers::media::{KeyInfo, Kind, MediaModel, ModelPage};

use super::params::Draft;
use crate::widgets::Load;
use crate::wire;

/// Models per request: a page.
pub const PAGE: usize = 30;
/// Typing waits this long without a newer keystroke before it searches.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Where the key the screen found comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// The OS credential store, entry `openrouter`.
    Keyring,
    /// `OPENROUTER_API_KEY`.
    Env,
}

type ModelsFn = Arc<dyn Fn(Kind, &str, usize, usize) -> Result<ModelPage, String> + Send + Sync>;
type ResolveFn = Arc<dyn Fn() -> Result<Option<KeySource>, String> + Send + Sync>;
type SaveFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
type CheckFn = Arc<dyn Fn() -> Result<KeyInfo, String> + Send + Sync>;

/// Everything the screen asks of the outside world. Cloned into worker jobs.
#[derive(Clone)]
pub struct Backend {
    /// `OpenRouter::models(kind, query, offset, limit)`. Needs no key.
    pub models: ModelsFn,
    /// Is there a key, and where is it. Never returns the key itself.
    pub resolve_key: ResolveFn,
    /// Writes the key to the OS keyring and nowhere else.
    pub save_key: SaveFn,
    /// `OpenRouter::check_key` with the stored key.
    pub check_key: CheckFn,
}

impl Backend {
    /// The real one, against `base_url` (empty means the public endpoint).
    pub fn real(base_url: String) -> Self {
        wire::media_backend(base_url)
    }
}

/// One generator: the saved choice and the screen state around it.
pub struct Slot {
    pub kind: Kind,
    /// Saved model id (`settings.media.<kind>.model`); empty when none.
    pub model_id: String,
    /// Saved fixed parameters.
    pub saved: BTreeMap<String, Value>,
    /// The saved model's descriptor: name, price, parameters.
    pub current: Load<MediaModel>,
    /// The search area is open (`Trocar`).
    pub picking: bool,
    pub query: TextEdit,
    /// The query the rows below answer; `None` before the first search.
    pub applied: Option<String>,
    pub rows: Vec<MediaModel>,
    /// Matches of `applied`, across every page.
    pub total: usize,
    /// Pages already in `rows`.
    pub pages: usize,
    /// The request in flight or its failure. `Ready` means the last page landed.
    pub status: Load<()>,
    /// Bumped by each new search; an answer for an older one is dropped.
    pub seq: u64,
    pub draft: Draft,
    /// Parameters dropped when the model changed, and why.
    pub notes: Vec<String>,
    pub notice: Option<Result<String, String>>,
}

impl Slot {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            model_id: String::new(),
            saved: BTreeMap::new(),
            current: Load::Idle,
            picking: false,
            query: TextEdit::new(""),
            applied: None,
            rows: Vec::new(),
            total: 0,
            pages: 0,
            status: Load::Idle,
            seq: 0,
            draft: Draft::default(),
            notes: Vec::new(),
            notice: None,
        }
    }

    pub fn configured(&self) -> bool {
        !self.model_id.trim().is_empty()
    }

    /// More matches exist than the rows loaded.
    pub fn has_more(&self) -> bool {
        self.applied.is_some() && self.rows.len() < self.total
    }
}

pub struct MediaState {
    pub backend: Backend,
    pub base_url: String,
    pub image: Slot,
    pub video: Slot,
    /// Which section is open.
    pub kind: Kind,
    /// The key field: masked, wiped on drop, never written to disk.
    pub key: TextEdit,
    pub key_state: Load<Option<KeySource>>,
    /// `Testar`: the key's label and spend, or why it failed.
    pub check: Load<KeyInfo>,
    pub saving_key: bool,
    /// The key form is open even though a key exists (`Trocar chave`).
    pub editing_key: bool,
    pub key_notice: Option<Result<String, String>>,
    /// Every page ever fetched this run, by (kind, query, page).
    pub cache: HashMap<(Kind, String, usize), ModelPage>,
    /// Network requests made, as opposed to cache hits.
    pub requests: usize,
    /// Typing timers, one per kind. They only schedule; the search itself runs
    /// on a worker through `Shell::work`.
    pub timers: [Latest<()>; 2],
    /// The screen was opened once: settings read, key looked up.
    pub opened: bool,
    /// Wakes the window when a typing timer lands (`AppHandle::wake`).
    waker: Option<Waker>,
}

impl MediaState {
    /// Reads the saved choices from `root`'s settings. Cheap: one file.
    pub fn open(root: &Path) -> Self {
        let saved = wire::media_load(root);
        let mut s = Self::with_backend(Backend::real(saved.base_url.clone()), saved.base_url);
        s.image.model_id = saved.image.model;
        s.image.saved = saved.image.params;
        s.video.model_id = saved.video.model;
        s.video.saved = saved.video.params;
        s
    }

    pub fn with_backend(backend: Backend, base_url: String) -> Self {
        Self {
            backend,
            base_url,
            image: Slot::new(Kind::Image),
            video: Slot::new(Kind::Video),
            kind: Kind::Image,
            key: TextEdit::new("").secure(),
            key_state: Load::Idle,
            check: Load::Idle,
            saving_key: false,
            editing_key: false,
            key_notice: None,
            cache: HashMap::new(),
            requests: 0,
            timers: [
                Latest::default().debounce(DEBOUNCE),
                Latest::default().debounce(DEBOUNCE),
            ],
            opened: false,
            waker: None,
        }
    }

    /// Connects the typing timers to the window: a landed timer wakes `poll`.
    pub fn set_waker(&mut self, waker: Waker) {
        for t in &self.timers {
            t.set_waker(waker.clone());
        }
        self.waker = Some(waker);
    }

    pub fn slot(&self, kind: Kind) -> &Slot {
        match kind {
            Kind::Image => &self.image,
            Kind::Video => &self.video,
        }
    }

    pub fn slot_mut(&mut self, kind: Kind) -> &mut Slot {
        match kind {
            Kind::Image => &mut self.image,
            Kind::Video => &mut self.video,
        }
    }

    pub fn timer(&self, kind: Kind) -> &Latest<()> {
        &self.timers[usize::from(kind == Kind::Video)]
    }

    /// Replaces the typing timers with ones that wait `wait`. Tests use a few
    /// milliseconds; the window uses [`DEBOUNCE`].
    pub fn set_debounce(&mut self, wait: Duration) {
        self.timers = [
            Latest::default().debounce(wait),
            Latest::default().debounce(wait),
        ];
        if let Some(w) = self.waker.clone() {
            self.set_waker(w);
        }
    }
}

pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "Imagem",
        Kind::Video => "Vídeo",
    }
}
