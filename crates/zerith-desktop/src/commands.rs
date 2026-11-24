//! The commands the window calls. Each one is a thin translation between the
//! window's JSON and the core crates; no rules live here.
//!
//! Everything blocking is `async` so Tauri runs it off the UI thread: a
//! SQLite query or a keyring read must never stall a frame.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zerith_sessions::{Session, SessionFilter, SessionSource, Store};

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

fn list_with_store(st: &Store, limit: usize) -> Cmd<Vec<SessionView>> {
    let v = st
        .list_sessions_filtered(&SessionFilter {
            archived: Some(false),
            exclude_source: Some(SessionSource::Subagent),
            limit,
            ..SessionFilter::default()
        })
        .map_err(|e| e.to_string())?;
    Ok(with_workspaces(st, v))
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
    let sessions = state.with_store(|st| list_with_store(st, 200))?;
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
    state.with_store(|st| list_with_store(st, limit.unwrap_or(200)))
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
    let dir_str = dir.display().to_string();
    state.with_store(|st| {
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
        st.set_workspace(&s.id, &dir_str)
            .map_err(|e| e.to_string())?;
        Ok(SessionView {
            session: s,
            workspace: Some(dir_str),
        })
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
    state.with_store(|st| {
        st.set_workspace(&id, &dir.display().to_string())
            .map_err(|e| e.to_string())
    })
}

/// Folders used before, newest first, only those still on disk. Never empty:
/// the first one is where a new conversation starts.
#[tauri::command]
pub async fn recent_workspaces(state: tauri::State<'_, AppState>) -> Cmd<Vec<String>> {
    let db = state.db();
    let mut out: Vec<String> = state
        .with_store(|st| st.recent_workspaces(8).map_err(|e| e.to_string()))
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

/// The newest `limit` messages of a conversation, oldest first: a long chat
/// opens on its latest answer.
#[tauri::command]
pub async fn session_messages(
    state: tauri::State<'_, AppState>,
    id: String,
    limit: Option<usize>,
) -> Cmd<Vec<ChatMessage>> {
    state
        .with_store(|st| {
            st.recent_messages(&id, limit.unwrap_or(400))
                .map_err(|e| e.to_string())
        })
        .map(|v| {
            v.into_iter()
                .map(|m| ChatMessage {
                    id: m.id,
                    role: m.role.as_str().to_string(),
                    content: m.content,
                    reasoning: m.reasoning,
                    tool_name: m.tool_name,
                    tool_calls: m.tool_calls,
                })
                .collect()
        })
}

/// A background shell job (`shell run_in_background`) still running.
#[derive(Serialize)]
pub struct JobView {
    pub id: String,
    pub command: String,
    /// The conversation whose turn started it.
    pub session: Option<String>,
    /// Seconds since it started.
    pub secs: u64,
}

/// Background jobs of every running turn. A turn's tools (and so its jobs)
/// live only while the turn runs: the window polls this while one does.
#[tauri::command]
pub async fn background_jobs() -> Cmd<Vec<JobView>> {
    Ok(zerith_tools::background_jobs()
        .into_iter()
        .map(|j| JobView {
            id: j.id,
            command: j.command,
            session: j.session,
            secs: j.secs,
        })
        .collect())
}

/// The last lines a background job printed (a dev server's log), read-only.
#[tauri::command]
pub async fn background_job_tail(id: String) -> Cmd<String> {
    zerith_tools::background_job_tail(&id, 200).ok_or_else(|| format!("job desconhecido: {id}"))
}

/// Stops one background job and every process it started.
#[tauri::command]
pub async fn background_job_stop(id: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || zerith_tools::stop_background_job(&id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Stops a running subagent (and its own children) by id: what it already
/// produced goes back to the turn that started it.
#[tauri::command]
pub fn subagent_stop(id: String) -> bool {
    zerith_runtime::subagent::stop(&id)
}

/// A copy of a conversation up to `upto_message_id` (all when `None`), in the
/// same folder; the original is untouched (edit a prompt, branch an answer).
#[tauri::command]
pub async fn session_fork(
    state: tauri::State<'_, AppState>,
    session_id: String,
    upto_message_id: Option<i64>,
    title: String,
) -> Cmd<SessionView> {
    state.with_store(|st| {
        let session = st
            .fork_session(&session_id, upto_message_id, title.trim())
            .map_err(|e| e.to_string())?;
        let workspace = st.workspace(&session.id).map_err(|e| e.to_string())?;
        Ok(SessionView { session, workspace })
    })
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
pub async fn rename_session(
    state: tauri::State<'_, AppState>,
    id: String,
    title: String,
) -> Cmd<()> {
    state.with_store(|st| st.rename_session(&id, &title).map_err(|e| e.to_string()))
}

#[tauri::command]
pub async fn delete_session(
    state: tauri::State<'_, AppState>,
    turns: tauri::State<'_, crate::chat::Turns>,
    id: String,
) -> Cmd<()> {
    turns.with(&id, |r| r.cancel.cancel());
    turns.remove_grants(&id);
    turns.cancel_precompact(&id);
    state.with_store(|st| st.delete_session(&id).map_err(|e| e.to_string()))?;
    let _ = zerith_sessions::checkpoints::delete_all(state.root(), &id);
    Ok(())
}

#[tauri::command]
pub async fn pinned_sessions(state: tauri::State<'_, AppState>) -> Cmd<Vec<String>> {
    state.with_store(|st| st.pinned_sessions().map_err(|e| e.to_string()))
}

#[tauri::command]
pub async fn set_session_pinned(
    state: tauri::State<'_, AppState>,
    id: String,
    pinned: bool,
) -> Cmd<Vec<String>> {
    state.with_store(|st| {
        st.set_session_pinned(&id, pinned)
            .map_err(|e| e.to_string())
    })
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

#[tauri::command]
pub async fn terminals_list() -> Cmd<Vec<zerith_tools::TerminalSnapshot>> {
    Ok(zerith_tools::list_active_terminals())
}

#[tauri::command]
pub async fn terminal_tail(id: String, max_bytes: Option<usize>) -> Cmd<String> {
    zerith_tools::terminal_tail(&id, max_bytes.unwrap_or(4096))
}

#[tauri::command]
pub async fn terminal_interrupt(id: String) -> Cmd<()> {
    zerith_tools::interrupt_terminal(&id)
}

#[tauri::command]
pub async fn terminal_close(id: String) -> Cmd<()> {
    zerith_tools::close_terminal(&id)
}

#[tauri::command]
pub async fn terminal_send(id: String, input: String) -> Cmd<()> {
    zerith_tools::send_terminal_input(&id, &input)
}

#[tauri::command]
pub async fn session_usage(
    state: tauri::State<'_, crate::state::AppState>,
    session_id: String,
) -> Cmd<zerith_sessions::usage::SessionUsageTotals> {
    state.with_store(|s| s.session_usage(&session_id).map_err(|e| e.to_string()))
}
