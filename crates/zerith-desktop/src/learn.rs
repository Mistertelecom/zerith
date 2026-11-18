//! Learning after a turn, desktop side: runs the review of
//! `zerith_runtime::learn` on its own thread once the answer is done, tells
//! the window what was learned (`turn:learned`) and undoes an item on request.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use zerith_core::{Paths, Settings};
use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, ProviderConfig};
use zerith_runtime::learn::{self, ClientReviewer, Pass, TurnDigest};
use zerith_sessions::Store;

use crate::state::AppState;

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Called once from `setup`, so a turn thread can emit without a handle.
pub fn bind(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// Everything the review needs, owned, so it outlives the turn.
pub struct Job {
    pub root: PathBuf,
    pub db: PathBuf,
    pub settings: Settings,
    pub fallback: ProviderConfig,
    pub session_id: String,
    pub model: String,
    pub memory_paused: bool,
    pub digest: TurnDigest,
}

/// Starts the review on its own thread when the turn had a signal. Returns
/// at once: the answer is never held back by it.
pub fn spawn(job: Job) {
    if !job.settings.learning.enabled {
        return;
    }
    let signals = learn::gate(&job.digest);
    if signals.is_empty() {
        eprintln!("zerith-desktop: aprender: turno sem sinal, nenhuma revisão");
        return;
    }
    std::thread::spawn(move || {
        let Ok(store) = Store::open(&job.db) else {
            return;
        };
        let Some((client, model)) = learn::route(&job.settings, &job.model, &job.fallback) else {
            return;
        };
        let memory = (!job.memory_paused)
            .then(|| SqliteMemory::open(&job.db).ok())
            .flatten();
        let scope = crate::wire::memory_scope();
        let skills = zerith_skills::Library::open(job.root.join("skills")).ok();
        let pass = Pass {
            memory: memory.as_ref().map(|m| (m as &dyn MemoryBackend, &scope)),
            skills: skills.as_ref(),
            session_id: &job.session_id,
        };
        let reviewer = ClientReviewer {
            client: &client,
            model: &model,
            record: Some((&store, &job.session_id)),
        };
        let out = learn::after_turn(&job.digest, &pass, &reviewer, &Cancel::default());
        eprintln!(
            "zerith-desktop: aprender: sinais {:?}, {} aprendido(s), recusado(s) {:?}{}",
            out.signals,
            out.learned.len(),
            out.refused,
            out.error
                .map(|e| format!(", erro: {e}"))
                .unwrap_or_default()
        );
        if out.learned.is_empty() {
            return;
        }
        let items: Vec<_> = out.learned.iter().map(|l| l.to_json()).collect();
        if let Some(app) = APP.get() {
            let _ = app.emit("turn:learned", (&job.session_id, items));
        }
    });
}

#[derive(Serialize)]
pub struct LearningView {
    pub enabled: bool,
    pub model: String,
}

#[derive(Deserialize)]
pub struct LearningPatch {
    pub enabled: Option<bool>,
    pub model: Option<String>,
}

fn load(root: &std::path::Path) -> LearningView {
    let paths = Paths::with_root(root);
    let l = Settings::load(&paths, None).settings.learning;
    LearningView {
        enabled: l.enabled,
        model: l.model,
    }
}

#[tauri::command]
pub async fn learning(state: State<'_, AppState>) -> Result<LearningView, String> {
    Ok(load(state.root()))
}

#[tauri::command]
pub async fn set_learning(
    state: State<'_, AppState>,
    patch: LearningPatch,
) -> Result<LearningView, String> {
    let root = state.root().to_path_buf();
    Settings::save_user(&Paths::with_root(&root), |s| {
        if let Some(v) = patch.enabled {
            s.learning.enabled = v;
        }
        if let Some(m) = patch.model {
            s.learning.model = m.trim().to_string();
        }
    })
    .map_err(|e| e.to_string())?;
    Ok(load(&root))
}

/// One item of a `turn:learned` payload, as the window sends it back.
#[derive(Deserialize)]
pub struct UndoItem {
    pub kind: String,
    pub id: Option<i64>,
    pub name: Option<String>,
    pub previous: Option<String>,
}

/// Forgets a learned fact (or restores its old text), or deletes / restores
/// an AI skill. Skills the person wrote are refused by the author API.
#[tauri::command]
pub async fn learn_undo(state: State<'_, AppState>, item: UndoItem) -> Result<(), String> {
    let db = state.db();
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let memory = SqliteMemory::open(&db).ok();
        let skills = zerith_skills::Library::open(root.join("skills")).ok();
        learn::undo(
            &item.kind,
            item.id,
            item.name.as_deref(),
            item.previous.as_deref(),
            memory.as_ref().map(|m| m as &dyn MemoryBackend),
            skills.as_ref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
