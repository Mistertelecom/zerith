//! Appearance, privacy, execution profile and the housekeeping the window
//! offers: how much space the database takes and a consistent copy of it.
//!
//! Nothing here decides policy. The profile and the token saver are read by
//! the turn on its own thread; this module only moves values in and out of
//! the user settings layer and the store.

use serde::{Deserialize, Serialize};
use tauri::State;
use zerith_core::settings::{CavemanLevel, PonytailLevel, TokenSaverSettings};
use zerith_sessions::Store;

use crate::state::AppState;
use crate::wire;

type Cmd<T> = Result<T, String>;

/// The token saver as the window sees it: levels are plain strings, so the
/// window does not need a table of the enum variants.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaverView {
    pub ponytail: String,
    pub caveman: String,
    pub rtk_enabled: bool,
}

impl SaverView {
    fn of(s: &TokenSaverSettings) -> Self {
        Self {
            ponytail: s.ponytail.as_str().to_string(),
            caveman: s.caveman.as_str().to_string(),
            rtk_enabled: s.rtk_enabled,
        }
    }
}

#[derive(Deserialize)]
pub struct SaverPatch {
    pub ponytail: Option<String>,
    pub caveman: Option<String>,
    pub rtk_enabled: Option<bool>,
}

#[tauri::command]
pub async fn token_saver(state: State<'_, AppState>) -> Cmd<SaverView> {
    let root = state.root().to_path_buf();
    Ok(SaverView::of(&wire::load_token_saver(&root)))
}

/// Applies a patch and returns what was stored, so the window never shows a
/// value the file did not accept.
#[tauri::command]
pub async fn set_token_saver(state: State<'_, AppState>, patch: SaverPatch) -> Cmd<SaverView> {
    let root = state.root().to_path_buf();
    let mut ts = wire::load_token_saver(&root);
    if let Some(v) = patch.ponytail.as_deref() {
        ts.ponytail = PonytailLevel::parse(v).ok_or_else(|| format!("nível desconhecido: {v}"))?;
    }
    if let Some(v) = patch.caveman.as_deref() {
        ts.caveman = CavemanLevel::parse(v).ok_or_else(|| format!("nível desconhecido: {v}"))?;
    }
    if let Some(v) = patch.rtk_enabled {
        ts.rtk_enabled = v;
    }
    wire::save_token_saver(&root, &ts)?;
    Ok(SaverView::of(&ts))
}

/// Long conversations, as Ajustes › Geral shows them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongContextView {
    pub enabled: bool,
    /// `off`, `missing`, `starting`, `running`, `failed`.
    pub state: &'static str,
    pub detail: String,
    pub log: String,
}

fn long_context_view(root: &std::path::Path) -> LongContextView {
    use zerith_runtime::bili::Status;
    let enabled = wire::load_context(root).billion_context;
    let (state, detail) = match zerith_runtime::bili::status() {
        Status::Off => ("off", String::new()),
        Status::Missing => ("missing", "npm install -g billion-context".into()),
        Status::Starting => ("starting", String::new()),
        Status::Running(o) => ("running", o),
        Status::Failed(e) => ("failed", e),
    };
    LongContextView {
        enabled,
        state,
        detail,
        log: zerith_runtime::bili::log_path(root).display().to_string(),
    }
}

#[tauri::command]
pub async fn long_context(state: State<'_, AppState>) -> Cmd<LongContextView> {
    let root = state.root().to_path_buf();
    Ok(long_context_view(&root))
}

/// Saves the switch and starts or stops the proxy before answering, so the
/// screen shows what really happened.
#[tauri::command]
pub async fn set_long_context(state: State<'_, AppState>, enabled: bool) -> Cmd<LongContextView> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        wire::save_context(&root, enabled)?;
        zerith_runtime::bili::ensure(&root, enabled, true);
        Ok(long_context_view(&root))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `npm install -g billion-context`, then start it.
#[tauri::command]
pub async fn install_billion_context(state: State<'_, AppState>) -> Cmd<LongContextView> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        zerith_runtime::bili::install()?;
        zerith_runtime::bili::ensure(&root, wire::load_context(&root).billion_context, true);
        Ok(long_context_view(&root))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageView {
    pub root: String,
    pub db_path: String,
    pub db_bytes: u64,
    pub sessions: i64,
    pub messages: i64,
    pub backup_dir: String,
}

#[tauri::command]
pub async fn storage(state: State<'_, AppState>) -> Cmd<StorageView> {
    let root = state.root().to_path_buf();
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        let st = Store::open(&db).map_err(|e| e.to_string())?;
        let count = |sql: &str| -> Result<i64, String> {
            st.conn()
                .query_row(sql, [], |r| r.get(0))
                .map_err(|e| e.to_string())
        };
        Ok(StorageView {
            backup_dir: root.join("backups").display().to_string(),
            db_path: db.display().to_string(),
            root: root.display().to_string(),
            db_bytes,
            sessions: count("SELECT COUNT(*) FROM sessions")?,
            messages: count("SELECT COUNT(*) FROM messages")?,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copies the live database with SQLite's online backup. The timestamp is
/// seconds since the epoch, the same shape the old window used.
#[tauri::command]
pub async fn backup(state: State<'_, AppState>) -> Cmd<String> {
    let root = state.root().to_path_buf();
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = root.join("backups");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let to = dir.join(format!("zerith-{stamp}.db"));
        Store::open(&db)
            .and_then(|st| st.backup_to(&to))
            .map_err(|e| e.to_string())?;
        Ok(to.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The numbers behind an execution profile, already formatted for the hint
/// under the control.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    pub name: String,
    pub max_subagents: usize,
    pub history_window: usize,
}

#[tauri::command]
pub async fn profile_limits() -> Cmd<Vec<ProfileView>> {
    Ok([
        ("economy", crate::types::Profile::Economy),
        ("balanced", crate::types::Profile::Balanced),
        ("performance", crate::types::Profile::Performance),
    ]
    .into_iter()
    .map(|(name, p)| {
        let l = wire::profile_limits(p);
        ProfileView {
            name: name.into(),
            max_subagents: l.max_subagents,
            history_window: l.history_window,
        }
    })
    .collect())
}
