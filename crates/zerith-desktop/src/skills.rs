//! Installed skills and the catalogue that suggests them.
//!
//! Nothing here executes a skill. Installing copies files, scans them, and
//! records a manifest row; enabling only flips that row.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;
use zerith_skills::catalog::Catalogue;
use zerith_skills::remote::{self, RemoteRepo};
use zerith_skills::{scan, Installed, Library, Skill, Status};

use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingView {
    severity: String,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    installed: Installed,
    /// Findings the scanner recorded; empty when the directory no longer scans.
    findings: Vec<FindingView>,
    scripts: Vec<String>,
    inventory: Vec<(String, usize)>,
    body: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    trusted: bool,
    enabled: bool,
}

fn library(state: &AppState) -> PathBuf {
    state.root().join("skills")
}

fn open_library(root: &Path) -> Result<Library, String> {
    Library::open(root).map_err(|e| e.to_string())
}

fn view(root: &Path, installed: Installed) -> SkillView {
    let scanned: Option<Skill> = scan(&root.join(&installed.dir)).ok();
    let body =
        std::fs::read_to_string(root.join(&installed.dir).join("SKILL.md")).unwrap_or_default();
    SkillView {
        findings: scanned
            .as_ref()
            .map(|s| {
                s.findings
                    .iter()
                    .map(|f| FindingView {
                        severity: f.severity.as_str().to_string(),
                        message: f.message.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        scripts: scanned
            .as_ref()
            .map(|s| s.scripts.clone())
            .unwrap_or_default(),
        inventory: scanned
            .as_ref()
            .map(|s| s.inventory.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .unwrap_or_default(),
        body,
        installed,
    }
}

fn all(lib: &Library, root: &Path) -> Result<Vec<SkillView>, String> {
    let manifest = lib.manifest().map_err(|e| e.to_string())?;
    Ok(manifest.skills.into_iter().map(|i| view(root, i)).collect())
}

#[tauri::command]
pub async fn skills_list(state: State<'_, AppState>) -> Result<Vec<SkillView>, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        all(&lib, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reads the suggested repositories. The cached answer is used until it goes
/// stale; force asks the network again.
#[tauri::command]
pub async fn skills_catalog(state: State<'_, AppState>, force: bool) -> Result<Catalogue, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let token = std::env::var("GITHUB_TOKEN").ok();
        zerith_skills::catalog::load(&root, token.as_deref(), force).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skills_plugins(state: State<'_, AppState>) -> Result<Vec<PluginView>, String> {
    let root = state.root().join("plugins");
    tauri::async_runtime::spawn_blocking(move || {
        zerith_plugins::Library::open(&root)
            .and_then(|l| l.plugins())
            .map(|list| {
                list.into_iter()
                    .map(|p| PluginView {
                        name: p.manifest.name.clone(),
                        version: p.manifest.version.clone(),
                        description: p.manifest.description.clone(),
                        capabilities: p.manifest.capabilities.clone(),
                        trusted: p.trusted,
                        enabled: p.status == zerith_plugins::Status::Enabled,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skill_install(
    state: State<'_, AppState>,
    from: String,
) -> Result<Vec<SkillView>, String> {
    let from = from.trim().to_string();
    if from.is_empty() {
        return Err("informe a pasta da skill".into());
    }
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        lib.install(Path::new(&from), &from)
            .map_err(|e| e.to_string())?;
        all(&lib, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The skills inside one suggested repository, cached per repository.
#[tauri::command]
pub async fn skills_remote(
    state: State<'_, AppState>,
    repo: String,
    force: bool,
) -> Result<RemoteRepo, String> {
    let repo = repo.trim().to_string();
    if !zerith_skills::catalog::SOURCES.contains(&repo.as_str()) && !remote::valid_repo(&repo) {
        return Err(format!("repositório inválido: {repo}"));
    }
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let token = std::env::var("GITHUB_TOKEN").ok();
        remote::load(&root, &repo, token.as_deref(), force).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Downloads one skill folder from GitHub and installs it; no clone.
#[tauri::command]
pub async fn skill_install_remote(
    state: State<'_, AppState>,
    repo: String,
    branch: String,
    path: String,
) -> Result<Vec<SkillView>, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        let token = std::env::var("GITHUB_TOKEN").ok();
        remote::install_remote(
            &lib,
            repo.trim(),
            branch.trim(),
            path.trim(),
            token.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        all(&lib, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skill_toggle(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
) -> Result<Vec<SkillView>, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        let status = if enabled {
            Status::Enabled
        } else {
            Status::Disabled
        };
        lib.set_status(&name, status).map_err(|e| e.to_string())?;
        all(&lib, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn skill_remove(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<SkillView>, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        lib.remove(&name).map_err(|e| e.to_string())?;
        all(&lib, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesSkillView {
    name: String,
    category: String,
    description: String,
    installed: bool,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesSkillsImport {
    imported: Vec<String>,
    existed: Vec<String>,
    failed: Vec<String>,
    skills: Vec<SkillView>,
}

fn hermes_home() -> Result<PathBuf, String> {
    zerith_importers::hermes_setup::hermes_home()
        .ok_or_else(|| "Hermes não encontrado neste computador".to_string())
}

/// Every skill in the Hermes home, with whether it is already here.
#[tauri::command]
pub async fn hermes_skills(state: State<'_, AppState>) -> Result<Vec<HermesSkillView>, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        Ok(
            zerith_importers::hermes_setup::hermes_skills(&hermes_home()?, &lib)
                .into_iter()
                .map(|s| HermesSkillView {
                    name: s.name,
                    category: s.category,
                    description: s.description,
                    installed: s.installed,
                    error: s.error,
                })
                .collect(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copies the chosen Hermes skills in, enabled, through the normal install.
#[tauri::command]
pub async fn hermes_skills_import(
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<HermesSkillsImport, String> {
    let root = library(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let lib = open_library(&root)?;
        let r = zerith_importers::hermes_setup::import_hermes_skills(&hermes_home()?, &lib, &names);
        Ok(HermesSkillsImport {
            imported: r.imported,
            existed: r.existed,
            failed: r.failed,
            skills: all(&lib, &root)?,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
