//! The commands the window calls. Each one is a thin translation between the
//! window's JSON and the core crates; no rules live here.
//!
//! Everything blocking is `async` so Tauri runs it off the UI thread: a
//! SQLite query or a keyring read must never stall a frame.

use serde::{Deserialize, Serialize};
use zerith_sessions::{Session, SessionSource, Store};

use crate::state::AppState;
use crate::types::{ChatMessage, Profile, ProviderView, Theme};
use crate::wire;

type Cmd<T> = Result<T, String>;

/// Everything the window needs to draw its first frame.
#[derive(Serialize)]
pub struct Boot {
    pub root: String,
    pub db: String,
    pub theme: Theme,
    pub profile: Profile,
    pub reduce_motion: bool,
    pub sessions: Vec<Session>,
    pub providers: Vec<ProviderView>,
    pub memory_paused: bool,
}

#[tauri::command]
pub async fn boot(state: tauri::State<'_, AppState>) -> Cmd<Boot> {
    let root = state.root().to_path_buf();
    let db = state.db();
    state.ensure().map_err(|e| e.to_string())?;
    let prefs = wire::load_prefs(&root);
    let sessions = Store::open(&db)
        .and_then(|st| st.list_sessions(200))
        .map_err(|e| e.to_string())?;
    Ok(Boot {
        root: root.display().to_string(),
        db: db.display().to_string(),
        theme: prefs.theme,
        profile: prefs.profile,
        reduce_motion: prefs.reduce_motion,
        sessions,
        providers: wire::configured_providers(&root)
            .iter()
            .map(ProviderView::of)
            .collect(),
        memory_paused: wire::memory_paused(&db),
    })
}

#[tauri::command]
pub async fn list_sessions(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
) -> Cmd<Vec<Session>> {
    let db = state.db();
    Store::open(&db)
        .and_then(|st| st.list_sessions(limit.unwrap_or(200)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_session(state: tauri::State<'_, AppState>, title: String) -> Cmd<Session> {
    let db = state.db();
    let title: String = title.trim().chars().take(80).collect();
    Store::open(&db)
        .and_then(|st| {
            st.create_session(
                if title.is_empty() {
                    "Nova conversa"
                } else {
                    &title
                },
                SessionSource::Native,
            )
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn session_messages(
    state: tauri::State<'_, AppState>,
    id: String,
    limit: Option<usize>,
) -> Cmd<Vec<ChatMessage>> {
    let db = state.db();
    Store::open(&db)
        .and_then(|st| st.messages(&id, limit.unwrap_or(400), 0))
        .map(|v| {
            v.into_iter()
                .map(|m| ChatMessage {
                    role: m.role.as_str().to_string(),
                    content: m.content,
                    reasoning: m.reasoning,
                    tool_name: m.tool_name,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_archived(
    state: tauri::State<'_, AppState>,
    id: String,
    archived: bool,
) -> Cmd<()> {
    let db = state.db();
    wire::set_archived(&db, &id, archived)
}

#[tauri::command]
pub async fn export_session(state: tauri::State<'_, AppState>, id: String) -> Cmd<String> {
    let db = state.db();
    wire::export_session(&db, &id).map(|p| p.display().to_string())
}

/// Theme and motion. Written to the user settings layer, off the UI thread.
#[derive(Deserialize)]
pub struct PrefsPatch {
    pub theme: Option<Theme>,
    pub profile: Option<Profile>,
    pub reduce_motion: Option<bool>,
}

#[tauri::command]
pub async fn save_prefs(state: tauri::State<'_, AppState>, patch: PrefsPatch) -> Cmd<Theme> {
    let root = state.root().to_path_buf();
    let mut prefs = wire::load_prefs(&root);
    if let Some(t) = patch.theme {
        prefs.theme = t;
    }
    if let Some(p) = patch.profile {
        prefs.profile = p;
    }
    if let Some(m) = patch.reduce_motion {
        prefs.reduce_motion = m;
    }
    wire::save_prefs(&root, prefs, true);
    Ok(prefs.theme)
}

#[tauri::command]
pub async fn providers(state: tauri::State<'_, AppState>) -> Cmd<Vec<ProviderView>> {
    let root = state.root().to_path_buf();
    Ok(wire::configured_providers(&root)
        .iter()
        .map(ProviderView::of)
        .collect())
}

#[tauri::command]
pub async fn memory_paused(state: tauri::State<'_, AppState>) -> Cmd<bool> {
    let db = state.db();
    Ok(wire::memory_paused(&db))
}

#[tauri::command]
pub async fn set_memory_paused(state: tauri::State<'_, AppState>, paused: bool) -> Cmd<()> {
    let db = state.db();
    wire::set_memory_paused(&db, paused)
}

#[tauri::command]
pub async fn search_sessions(state: tauri::State<'_, AppState>, query: String) -> Cmd<Vec<String>> {
    let db = state.db();
    wire::search_sessions(&db, &query, 500)
}

#[tauri::command]
pub async fn reveal_root(state: tauri::State<'_, AppState>) -> Cmd<()> {
    let root = state.root().to_path_buf();
    wire::reveal(&root)
}
