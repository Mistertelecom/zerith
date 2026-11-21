//! The fact store the agent writes into: list, search, edit, forget.
//!
//! Every command here opens the database on a blocking worker: SQLite calls
//! are synchronous and the UI thread is what a turn's stream also rides on.

use tauri::State;
use zerith_memory::{MemoryBackend, SqliteMemory};

use crate::state::AppState;
/// The memory screen manages every project's facts, not one chat's view.
fn memory_scope() -> zerith_memory::Scope {
    zerith_memory::Scope::all()
}

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

#[tauri::command]
pub async fn memory_add(
    state: State<'_, AppState>,
    kind: String,
    text: String,
) -> Result<zerith_memory::Fact, String> {
    let clean = text.trim().to_string();
    if clean.is_empty() {
        return Err("o fato não pode ficar vazio".into());
    }
    let fk = zerith_memory::FactKind::parse(kind.trim());
    let db = state.db().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let mem = open_at(&db)?;
        let scope = zerith_memory::default_scope();
        let id = mem
            .remember(
                &scope,
                fk,
                &clean,
                zerith_memory::Confidence::Confirmed,
                Some("manual"),
            )
            .map_err(|e| e.to_string())?;
        mem.get(id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Imports facts from local Hermes `MEMORY.md` and `USER.md` files (`~/.hermes/memories/`).
#[tauri::command]
pub async fn memory_import_hermes(state: State<'_, AppState>) -> Result<usize, String> {
    let db = state.db().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "pasta pessoal não encontrada".to_string())?;
        let hermes_dir = home.join(".hermes");
        let mut files = vec![
            hermes_dir.join("memories").join("USER.md"),
            hermes_dir.join("memories").join("MEMORY.md"),
        ];
        if let Ok(profiles) = std::fs::read_dir(hermes_dir.join("profiles")) {
            for entry in profiles.flatten() {
                let p = entry.path().join("memories");
                files.push(p.join("USER.md"));
                files.push(p.join("MEMORY.md"));
            }
        }
        let mem = open_at(&db)?;
        let scope = zerith_memory::default_scope();
        let mut count = 0usize;
        for file in files {
            let Ok(raw) = std::fs::read_to_string(&file) else {
                continue;
            };
            let is_user = file
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("USER.md"));
            for chunk in raw.split(['\n', '§']) {
                let line = chunk.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
                if line.len() < 4 || line.starts_with('#') || line.starts_with("<!--") {
                    continue;
                }
                let low = line.to_lowercase();
                let kind = if low.contains("sempre")
                    || low.contains("nunca")
                    || low.contains("always")
                    || low.contains("never")
                {
                    zerith_memory::FactKind::Instruction
                } else if low.contains("prefere") || low.contains("prefer") {
                    zerith_memory::FactKind::Preference
                } else if is_user {
                    zerith_memory::FactKind::Entity
                } else {
                    zerith_memory::FactKind::Fact
                };
                if mem
                    .remember(
                        &scope,
                        kind,
                        line,
                        zerith_memory::Confidence::Confirmed,
                        Some("hermes"),
                    )
                    .is_ok()
                {
                    count += 1;
                }
            }
        }
        Ok(count)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn open_at(db: &std::path::Path) -> Result<SqliteMemory, String> {
    SqliteMemory::open(db).map_err(|e| e.to_string())
}
