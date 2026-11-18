//! The commands the window calls. Each one is a thin translation between the
//! window's JSON and the core crates; no rules live here.
//!
//! Everything blocking is `async` so Tauri runs it off the UI thread: a
//! SQLite query or a keyring read must never stall a frame.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zerith_sessions::{Session, SessionSource, Store};

use crate::state::AppState;
use crate::types::{ChatMessage, Profile, ProviderView, Theme};
use crate::wire;

type Cmd<T> = Result<T, String>;

/// A session as the window lists it: the stored row plus its folder.
#[derive(Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub session: Session,
    pub workspace: Option<String>,
}

fn with_workspaces(st: &Store, list: Vec<Session>) -> Vec<SessionView> {
    let mut dirs = st.workspaces().unwrap_or_default();
    list.into_iter()
        .map(|s| SessionView {
            workspace: dirs.remove(&s.id),
            session: s,
        })
        .collect()
}

fn list(db: &Path, limit: usize) -> Cmd<Vec<SessionView>> {
    let st = Store::open(db).map_err(|e| e.to_string())?;
    let v = st.list_sessions(limit).map_err(|e| e.to_string())?;
    Ok(with_workspaces(&st, v))
}

/// Everything the window needs to draw its first frame.
#[derive(Serialize)]
pub struct Boot {
    pub root: String,
    pub db: String,
    pub theme: Theme,
    pub profile: Profile,
    pub reduce_motion: bool,
    pub sessions: Vec<SessionView>,
    pub providers: Vec<ProviderView>,
    pub memory_paused: bool,
}

#[tauri::command]
pub async fn boot(state: tauri::State<'_, AppState>) -> Cmd<Boot> {
    let root = state.root().to_path_buf();
    let db = state.db();
    state.ensure().map_err(|e| e.to_string())?;
    let prefs = wire::load_prefs(&root);
    let sessions = list(&db, 200)?;
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
) -> Cmd<Vec<SessionView>> {
    list(&state.db(), limit.unwrap_or(200))
}

/// A folder the window sent, checked: it must be an existing directory.
fn folder(raw: &str) -> Cmd<PathBuf> {
    let p = PathBuf::from(raw.trim());
    if raw.trim().is_empty() || !p.is_dir() {
        return Err(format!("Pasta não encontrada: {}", raw.trim()));
    }
    Ok(p)
}

/// A new conversation, in `workspace` when given, else in the last folder used.
#[tauri::command]
pub async fn create_session(
    state: tauri::State<'_, AppState>,
    title: String,
    workspace: Option<String>,
) -> Cmd<SessionView> {
    let db = state.db();
    let title: String = title.trim().chars().take(80).collect();
    let dir = match workspace.as_deref() {
        Some(w) if !w.trim().is_empty() => folder(w)?,
        _ => crate::turn::last_workspace(&db),
    };
    let st = Store::open(&db).map_err(|e| e.to_string())?;
    let s = st
        .create_session(
            if title.is_empty() {
                "Nova conversa"
            } else {
                &title
            },
            SessionSource::Native,
        )
        .map_err(|e| e.to_string())?;
    let dir = dir.display().to_string();
    st.set_workspace(&s.id, &dir).map_err(|e| e.to_string())?;
    Ok(SessionView {
        session: s,
        workspace: Some(dir),
    })
}

/// Moves a conversation to another folder; the next turn runs there.
#[tauri::command]
pub async fn set_session_workspace(
    state: tauri::State<'_, AppState>,
    id: String,
    workspace: String,
) -> Cmd<()> {
    let dir = folder(&workspace)?;
    Store::open(state.db())
        .and_then(|st| st.set_workspace(&id, &dir.display().to_string()))
        .map_err(|e| e.to_string())
}

/// Folders used before, newest first, only those still on disk. Never empty:
/// the first one is where a new conversation starts.
#[tauri::command]
pub async fn recent_workspaces(state: tauri::State<'_, AppState>) -> Cmd<Vec<String>> {
    let db = state.db();
    let mut out: Vec<String> = Store::open(&db)
        .and_then(|st| st.recent_workspaces(8))
        .unwrap_or_default()
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect();
    if out.is_empty() {
        out.push(crate::turn::last_workspace(&db).display().to_string());
    }
    Ok(out)
}

/// The OS folder picker. `None`: the person cancelled.
#[tauri::command]
pub async fn pick_folder(start: Option<String>) -> Cmd<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || crate::workspace::pick_folder(start.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// Opens a conversation's folder in the file manager.
#[tauri::command]
pub async fn reveal_workspace(path: String) -> Cmd<()> {
    // Native separators: `explorer C:/a/b` opens Documents instead.
    let native: PathBuf = folder(&path)?.components().collect();
    wire::reveal(&native)
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
