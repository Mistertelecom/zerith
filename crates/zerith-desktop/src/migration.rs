//! "Migrar do Hermes": o mesmo plano do `zerith import hermes --all`.
//!
//! `migration_plan` só lê (nada é gravado, nem o backup); `migration_apply`
//! faz backup, aplica e devolve o relatório. A tela mostra o plano por tipo e
//! só aplica com a confirmação da pessoa.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;
use tauri::State;
use zerith_core::Paths;
use zerith_importers::hermes_migrate::{run, Options};
use zerith_importers::plan::ImportPlan;

use crate::state::AppState;

/// O que a tela escolhe. Tipos ausentes = todos.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MigrationOpts {
    pub kinds: Option<Vec<String>>,
    pub migrate_secrets: bool,
    pub overwrite: bool,
    pub only_pinned_visible: bool,
    pub customized_skills: bool,
}

fn home() -> Result<PathBuf, String> {
    zerith_importers::hermes_setup::hermes_home()
        .ok_or_else(|| "Hermes não encontrado neste computador".to_string())
}

fn options(o: MigrationOpts, dry_run: bool) -> Options {
    let extra = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".hermes").join("creds"));
    Options {
        dry_run,
        overwrite: o.overwrite,
        migrate_secrets: o.migrate_secrets,
        only_pinned_visible: o.only_pinned_visible,
        customized_skills: o.customized_skills,
        kinds: o.kinds.map(|k| k.into_iter().collect::<BTreeSet<_>>()),
        extra_creds: extra.into_iter().collect(),
        ..Default::default()
    }
}

/// Ainda sem conversas e com um Hermes instalado: a tela inicial oferece a
/// migração.
#[tauri::command]
pub async fn migration_offer(state: State<'_, AppState>) -> Result<bool, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        if zerith_importers::hermes_setup::hermes_home().is_none() {
            return Ok(false);
        }
        let Ok(conn) =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            return Ok(true);
        };
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        Ok(n == 0)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// O plano, sem gravar nada.
#[tauri::command]
pub async fn migration_plan(
    state: State<'_, AppState>,
    opts: MigrationOpts,
) -> Result<ImportPlan, String> {
    let paths = Paths::with_root(state.root());
    tauri::async_runtime::spawn_blocking(move || {
        run(
            &home()?,
            &paths,
            &options(opts, true),
            &zerith_core::secrets::Configured,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Backup, aplicação e relatório em `<raiz>/migrations/<data-hora>/`.
#[tauri::command]
pub async fn migration_apply(
    state: State<'_, AppState>,
    opts: MigrationOpts,
) -> Result<ImportPlan, String> {
    let paths = Paths::with_root(state.root());
    let out = tauri::async_runtime::spawn_blocking(move || {
        run(
            &home()?,
            &paths,
            &options(opts, false),
            &zerith_core::secrets::Configured,
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    // Conversas e memória mudaram por fora da conexão guardada.
    state.reset_store();
    state.invalidate_mcp_cache();
    out
}
