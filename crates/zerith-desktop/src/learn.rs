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
use zerith_runtime::learn::{self, ClientReviewer, Outcome, Pass, Reviewer, TurnDigest};
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
    /// The conversation's memory scope (its project + the user).
    pub scope: zerith_memory::Scope,
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
        if store.session(&job.session_id).ok().flatten().is_none() {
            return;
        }
        let Some((client, model)) =
            learn::route_with_store(&job.settings, &job.model, &job.fallback, Some(&store))
        else {
            return;
        };
        let reviewer = ClientReviewer {
            client: &client,
            model: &model,
            record: Some((&store, &job.session_id)),
        };
        let Some(out) = run_for_existing_session(&store, &job, &reviewer) else {
            return;
        };
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
        if store.session(&job.session_id).ok().flatten().is_none() {
            return;
        }
        let items: Vec<_> = out.learned.iter().map(|l| l.to_json()).collect();
        if let Some(app) = APP.get() {
            let _ = app.emit("turn:learned", (&job.session_id, items));
        }
    });
}

fn run_for_existing_session(store: &Store, job: &Job, reviewer: &dyn Reviewer) -> Option<Outcome> {
    store.session(&job.session_id).ok().flatten()?;
    struct GuardedReviewer<'a> {
        store: &'a Store,
        session_id: &'a str,
        inner: &'a dyn Reviewer,
    }
    impl Reviewer for GuardedReviewer<'_> {
        fn review(&self, system: &str, digest: &str, cancel: &Cancel) -> Result<String, String> {
            let res = self.inner.review(system, digest, cancel)?;
            if self.store.session(self.session_id).ok().flatten().is_none() {
                return Err("sessão removida".into());
            }
            Ok(res)
        }
    }
    let memory = (!job.memory_paused)
        .then(|| SqliteMemory::open(&job.db).ok())
        .flatten();
    let scope = job.scope.clone();
    let skills = zerith_skills::Library::open(job.root.join("skills")).ok();
    let pass = Pass {
        memory: memory.as_ref().map(|m| (m as &dyn MemoryBackend, &scope)),
        skills: skills.as_ref(),
        session_id: &job.session_id,
    };
    let guarded = GuardedReviewer {
        store,
        session_id: &job.session_id,
        inner: reviewer,
    };
    let out = learn::after_turn(&job.digest, &pass, &guarded, &Cancel::default());
    store.session(&job.session_id).ok().flatten()?;
    Some(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use zerith_sessions::SessionSource;

    #[test]
    fn deleted_session_prevents_learning_writes_before_and_during_review() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/desktop-learn-deleted");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let db = scratch.join("zerith.db");
        let store = Store::open(&db).unwrap();
        let sess = store.create_session("temp", SessionSource::Native).unwrap();

        struct DeleteDuringReview {
            db: PathBuf,
            sid: String,
            delete_inside: bool,
            calls: Cell<usize>,
        }
        impl Reviewer for DeleteDuringReview {
            fn review(&self, _: &str, _: &str, _: &Cancel) -> Result<String, String> {
                self.calls.set(self.calls.get() + 1);
                if self.delete_inside {
                    let s2 = Store::open(&self.db).unwrap();
                    s2.delete_session(&self.sid).unwrap();
                }
                Ok(r##"{"memory":[{"op":"add","kind":"preference","text":"O usuário prefere tabs"}],"skill":{"op":"create","name":"deleted-sess-skill","description":"Use when testing","instructions":"body"}}"##.into())
            }
        }

        let job = Job {
            root: scratch.clone(),
            db: db.clone(),
            settings: Settings::default(),
            fallback: ProviderConfig::default(),
            session_id: sess.id.clone(),
            model: "m".into(),
            memory_paused: false,
            scope: zerith_memory::Scope::user("u"),
            digest: TurnDigest::new("prefiro tabs"),
        };

        // Case 1: Session deleted while review is in flight -> must not write skill or memory.
        let r1 = DeleteDuringReview {
            db: db.clone(),
            sid: sess.id.clone(),
            delete_inside: true,
            calls: Cell::new(0),
        };
        assert!(run_for_existing_session(&store, &job, &r1).is_none());
        assert_eq!(r1.calls.get(), 1);
        let mem = SqliteMemory::open(&db).unwrap();
        assert!(mem.list(&job.scope, 10).unwrap().is_empty());
        assert!(!scratch.join("skills/deleted-sess-skill").exists());

        // Case 2: Session already deleted before review -> reviewer never called.
        let r2 = DeleteDuringReview {
            db: db.clone(),
            sid: sess.id.clone(),
            delete_inside: false,
            calls: Cell::new(0),
        };
        assert!(run_for_existing_session(&store, &job, &r2).is_none());
        assert_eq!(r2.calls.get(), 0);
    }
}
