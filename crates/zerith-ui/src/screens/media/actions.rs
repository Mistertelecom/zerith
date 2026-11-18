//! What the Media buttons and the search box do. Plain state plus worker jobs
//! (`Shell::work`), so tests drive them directly with an inline shell.
//!
//! The search is a small state machine per kind:
//!
//! - typing schedules `Latest` (Rune's debounce timer, no polling): after
//!   [`DEBOUNCE`](super::state::DEBOUNCE) of quiet its answer wakes `poll`,
//!   which starts the search; Enter and `Buscar` start it at once;
//! - a search bumps `seq`, empties the rows and asks for page 0; an answer
//!   whose `seq` is not the slot's is dropped (but still cached);
//! - every `(kind, query, page)` is fetched once per run; a repeat is served
//!   from the cache with no request;
//! - `Carregar mais` asks for the next page of the same query.

use std::collections::BTreeMap;

use rune::Tx;
use serde_json::Value;
use zerith_providers::media::{Kind, MediaModel, ModelPage};

use super::params::{revalidate, Draft};
use super::state::{KeySource, MediaState, PAGE};
use crate::widgets::Load;
use crate::{wire, Shell};

/// The screen was opened: reads the key's presence and the descriptors of the
/// saved models, once, off the UI thread.
pub fn opened(s: &mut Shell) {
    if s.media.opened {
        return;
    }
    s.media.opened = true;
    check_key_presence(s);
    for kind in [Kind::Image, Kind::Video] {
        resolve_current(s, kind);
    }
}

/// The key is looked up in the keyring on a worker: the keyring can block.
pub fn check_key_presence(s: &mut Shell) {
    s.media.key_state = Load::Loading;
    let f = s.media.backend.resolve_key.clone();
    s.work(move || f(), |s, r| s.media.key_state = r.into());
}

/// Finds the saved model in the catalogue for its name, price and parameters.
fn resolve_current(s: &mut Shell, kind: Kind) {
    let id = s.media.slot(kind).model_id.clone();
    if id.trim().is_empty() {
        s.media.slot_mut(kind).current = Load::Idle;
        return;
    }
    s.media.slot_mut(kind).current = Load::Loading;
    let f = s.media.backend.models.clone();
    let q = id.clone();
    s.work(
        move || {
            f(kind, &q, 0, 10).and_then(|page| {
                page.models
                    .into_iter()
                    .find(|m| m.id == q)
                    .ok_or_else(|| "modelo não está mais no catálogo".to_string())
            })
        },
        move |s, r| {
            let slot = s.media.slot_mut(kind);
            // The user may have picked another model while this was out.
            if slot.model_id != id {
                return;
            }
            if let Ok(m) = &r {
                slot.draft = Draft::of(m, &slot.saved);
            }
            slot.current = r.into();
        },
    );
}

// ------------------------------------------------------------------ search

/// Typing in the search field: restarts the debounce timer. The search itself
/// starts in `poll` when the timer lands.
pub fn edit_query(s: &mut Shell, kind: Kind) {
    s.media.timer(kind).request((), |_| ());
}

/// Called every frame the window draws (`AppSpec::poll`). Cheap: it only takes
/// a timer's answer. Returns whether state changed.
pub fn poll(s: &mut Shell) -> bool {
    let mut changed = false;
    for kind in [Kind::Image, Kind::Video] {
        if s.media.timer(kind).take().is_some() {
            start_search(s, kind);
            changed = true;
        }
    }
    changed
}

/// `Trocar` / `Escolher modelo`: opens the search and lists the first page.
pub fn open_picker(s: &mut Shell, kind: Kind, tx: &mut Tx) {
    let slot = s.media.slot_mut(kind);
    slot.picking = true;
    if slot.applied.is_none() {
        start_search(s, kind);
    }
    tx.focus_when_shown(
        if kind == Kind::Image {
            "media-image-q"
        } else {
            "media-video-q"
        },
        true,
    );
}

pub fn close_picker(s: &mut Shell, kind: Kind) {
    s.media.slot_mut(kind).picking = false;
}

/// Enter or `Buscar`: searches now and drops a pending timer.
pub fn search_now(s: &mut Shell, kind: Kind) {
    s.media.timer(kind).cancel();
    start_search(s, kind);
}

/// Starts a new search for what the field holds: page 0, rows emptied.
pub fn start_search(s: &mut Shell, kind: Kind) {
    let slot = s.media.slot_mut(kind);
    let q = slot.query.text.trim().to_string();
    slot.seq += 1;
    slot.applied = Some(q);
    slot.rows.clear();
    slot.total = 0;
    slot.pages = 0;
    slot.status = Load::Idle;
    fetch_page(s, kind, 0);
}

/// `Carregar mais`: the next page of the query on screen.
pub fn load_more(s: &mut Shell, kind: Kind) {
    let slot = s.media.slot(kind);
    if !slot.has_more() || slot.status.is_loading() {
        return;
    }
    let page = slot.pages;
    fetch_page(s, kind, page);
}

/// Serves `page` of the slot's query from the cache, or asks the backend on a
/// worker. The answer lands only if the slot is still on the same search.
fn fetch_page(s: &mut Shell, kind: Kind, page: usize) {
    let slot = s.media.slot(kind);
    let Some(q) = slot.applied.clone() else {
        return;
    };
    let seq = slot.seq;
    let key = (kind, q.clone(), page);
    if let Some(hit) = s.media.cache.get(&key).cloned() {
        land_page(s, kind, seq, &q, page, Ok(hit));
        return;
    }
    s.media.slot_mut(kind).status = Load::Loading;
    s.media.requests += 1;
    let f = s.media.backend.models.clone();
    let query = q.clone();
    s.work(
        move || f(kind, &query, page * PAGE, PAGE),
        move |s, r| {
            // Cached even when stale: the user may come back to this query.
            if let Ok(p) = &r {
                s.media.cache.insert((kind, q.clone(), page), p.clone());
            }
            land_page(s, kind, seq, &q, page, r);
        },
    );
}

/// An answer for `page` of `q`, asked while the slot was on search `seq`.
/// Dropped when the slot has moved on. Public so tests can deliver a late
/// answer by hand.
pub fn land_page(
    s: &mut Shell,
    kind: Kind,
    seq: u64,
    q: &str,
    page: usize,
    r: Result<ModelPage, String>,
) {
    let slot = s.media.slot_mut(kind);
    // A newer search, or a page that is not the next one, makes this stale.
    if slot.seq != seq || slot.applied.as_deref() != Some(q) || slot.pages != page {
        return;
    }
    match r {
        Ok(p) => {
            slot.total = p.total;
            slot.rows.extend(p.models);
            slot.pages = page + 1;
            slot.status = Load::Ready(());
        }
        Err(e) => slot.status = Load::Failed(e),
    }
}

// ------------------------------------------------------------------- model

/// `Usar`: makes `model` the one for its kind, revalidates the saved
/// parameters against it (dropping what it cannot take, with a note) and
/// saves both.
pub fn use_model(s: &mut Shell, model: MediaModel) {
    let kind = model.kind;
    let slot = s.media.slot_mut(kind);
    let (kept, notes) = revalidate(&slot.saved, &model);
    slot.model_id = model.id.clone();
    slot.saved = kept;
    slot.notes = notes;
    slot.draft = Draft::of(&model, &slot.saved);
    slot.current = Load::Ready(model);
    slot.picking = false;
    slot.notice = Some(Ok("Salvando…".into()));
    persist(s, kind);
}

/// `Salvar parâmetros`: the form read back and saved. A bad value stays on
/// screen with the parameter named; nothing is written.
pub fn save_params(s: &mut Shell, kind: Kind) {
    let slot = s.media.slot_mut(kind);
    let Some(model) = slot.current.ready() else {
        return;
    };
    match slot.draft.to_map(model) {
        Ok(map) => {
            slot.saved = map;
            slot.notes.clear();
            slot.notice = Some(Ok("Salvando…".into()));
            persist(s, kind);
        }
        Err(e) => slot.notice = Some(Err(e)),
    }
}

/// Writes the slot's model and parameters to `settings.toml`, on a worker.
fn persist(s: &mut Shell, kind: Kind) {
    let slot = s.media.slot(kind);
    let (model, params) = (slot.model_id.clone(), slot.saved.clone());
    let root = s.root();
    s.work(
        move || wire::media_save(&root, kind, &model, &params),
        move |s, r| {
            s.media.slot_mut(kind).notice = Some(match r {
                Ok(()) => Ok("Salvo".into()),
                Err(e) => Err(format!("Não consegui salvar: {e}")),
            })
        },
    );
}

// --------------------------------------------------------------------- key

/// `Salvar chave`: the keyring, never a file. The field is wiped as soon as
/// the key is out of it; afterwards the window only knows that one exists.
pub fn save_key(s: &mut Shell) {
    let key = s.media.key.text.trim().to_string();
    if key.is_empty() {
        s.media.key_notice = Some(Err("Cole a chave do OpenRouter no campo.".into()));
        return;
    }
    if s.media.saving_key {
        return;
    }
    s.media.saving_key = true;
    s.media.key_notice = None;
    s.media.check = Load::Idle;
    s.media.key = rune::TextEdit::new("").secure();
    let f = s.media.backend.save_key.clone();
    s.work(
        move || f(&key),
        |s, r| {
            let m = &mut s.media;
            m.saving_key = false;
            match r {
                Ok(()) => {
                    m.key_state = Load::Ready(Some(KeySource::Keyring));
                    m.editing_key = false;
                    m.key_notice = Some(Ok("chave salva no cofre".into()));
                }
                Err(e) => m.key_notice = Some(Err(format!("Não consegui guardar a chave: {e}"))),
            }
        },
    );
}

/// `Testar`: asks the OpenRouter who the stored key is.
pub fn test_key(s: &mut Shell) {
    if s.media.check.is_loading() {
        return;
    }
    s.media.check = Load::Loading;
    let f = s.media.backend.check_key.clone();
    s.work(move || f(), |s, r| s.media.check = r.into());
}

pub fn edit_key(s: &mut Shell, tx: &mut Tx) {
    s.media.editing_key = true;
    s.media.check = Load::Idle;
    tx.focus_when_shown("media-key", true);
}

pub fn set_kind(s: &mut Shell, kind: Kind) {
    s.media.kind = kind;
}

/// A clean map: used by tests to compare saved parameters.
pub fn saved(s: &Shell, kind: Kind) -> BTreeMap<String, Value> {
    s.media.slot(kind).saved.clone()
}

/// Test hook: how many network requests the screen made.
pub fn requests(m: &MediaState) -> usize {
    m.requests
}
