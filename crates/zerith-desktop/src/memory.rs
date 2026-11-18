//! The fact store the agent writes into: list, search, edit, forget.
//!
//! Every command here opens the database on a blocking worker: SQLite calls
//! are synchronous and the UI thread is what a turn's stream also rides on.

use tauri::State;
use zerith_memory::{MemoryBackend, SqliteMemory};

use crate::state::AppState;
use crate::wire::memory_scope;

/// Facts read per load. Bounded so a huge memory never stalls the window.
const FACT_PAGE: usize = 500;
const RECALL_HITS: usize = 100;

#[tauri::command]
pub async fn memory_facts(state: State<'_, AppState>) -> Result<Vec<zerith_memory::Fact>, String> {
    let db = state.db().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        SqliteMemory::open(&db)
            .and_then(|m| m.list(&memory_scope(), FACT_PAGE))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Returns the matching fact ids, best first, so the window reorders the list
/// it already has instead of shipping the whole store twice.
#[tauri::command]
pub async fn memory_search(state: State<'_, AppState>, query: String) -> Result<Vec<i64>, String> {
    let db = state.db().to_path_buf();
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        SqliteMemory::open(&db)
            .and_then(|m| m.recall(&memory_scope(), &query, RECALL_HITS))
            .map(|hits| hits.into_iter().map(|h| h.fact.id).collect())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn memory_update(
    state: State<'_, AppState>,
    id: i64,
    text: String,
) -> Result<(), String> {
    let new = text.trim().to_string();
    if new.is_empty() {
        return Err("o fato não pode ficar vazio".into());
    }
    let db = state.db().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_at(&db).and_then(|m| m.update(id, &new).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn memory_forget(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let db = state.db().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_at(&db).and_then(|m| m.forget(id).map_err(|e| e.to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn open_at(db: &std::path::Path) -> Result<SqliteMemory, String> {
    SqliteMemory::open(db).map_err(|e| e.to_string())
}
