//! Agent definitions: the ones the user wrote, the ones a project ships and
//! the ones the model created (disabled until a person reads the file).
//!
//! Creating one writes `<root>/agents/<name>.md` with `create_new`, so two
//! clicks cannot race into a clobber and an existing agent is never
//! overwritten silently.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;
use zerith_core::agents::AgentDef;
use zerith_core::Paths;

use crate::state::AppState;

/// One agent as the window sees it: no `PathBuf`, nothing to serialize by hand.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub instructions: String,
    pub path: String,
    /// `plugin`, `user` or `project`.
    pub layer: String,
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<String>,
    pub max_turns: Option<u32>,
    pub skills: Vec<String>,
    pub isolation: String,
    pub background: bool,
    pub created_by_ai: bool,
    pub disabled: bool,
}

impl AgentView {
    fn of(a: &AgentDef) -> Self {
        Self {
            name: a.name.clone(),
            description: a.description.clone(),
            model: a.model.clone(),
            instructions: a.instructions.clone(),
            path: a.path.display().to_string(),
            layer: a.layer.to_string(),
            tools: a.tools.clone(),
            disallowed_tools: a.disallowed_tools.clone(),
            permission_mode: a.permission_mode.clone(),
            max_turns: a.max_turns,
            skills: a.skills.clone(),
            isolation: match a.isolation {
                zerith_core::agents::Isolation::Worktree => "worktree",
                zerith_core::agents::Isolation::None => "none",
            }
            .into(),
            background: a.background,
            created_by_ai: a.created_by_ai,
            disabled: a.disabled,
        }
    }
}

/// The form behind `Novo agente`. Every field is plain text; the file is only
/// built when the person presses the button.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentForm {
    pub name: String,
    pub description: String,
    pub instructions: String,
    /// Comma-separated allowlist; empty inherits every tool.
    pub tools: String,
    pub model: String,
}

/// Why `name` cannot be an agent file name. The same rule the reader applies,
/// plus `default`, which is reserved.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        Some("Dê um nome ao agente.")
    } else if n.len() > 64 {
        Some("O nome pode ter até 64 caracteres.")
    } else if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Some("Use só letras sem acento, números, “-” e “_” no nome.")
    } else if n == zerith_core::agents::DEFAULT {
        Some("“default” é reservado para o agente padrão.")
    } else {
        None
    }
}

/// The `.md` file for the form: frontmatter only for what was filled in.
pub fn render_file(f: &AgentForm) -> String {
    let mut head = String::new();
    let one_line = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let desc = one_line(&f.description);
    if !desc.is_empty() {
        head.push_str(&format!("description: \"{}\"\n", desc.replace('"', "'")));
    }
    let tools: Vec<String> = f
        .tools
        .split(',')
        .map(one_line)
        .filter(|t| !t.is_empty() && !t.contains("---"))
        .collect();
    if !tools.is_empty() {
        head.push_str(&format!("tools: {}\n", tools.join(", ")));
    }
    let model = one_line(&f.model);
    if !model.is_empty() && !model.contains("---") {
        head.push_str(&format!("model: {model}\n"));
    }
    let body = f.instructions.trim();
    if head.is_empty() {
        format!("{body}\n")
    } else {
        format!("---\n{head}---\n{body}\n")
    }
}

/// Writes `<dir>/<name>.md`, refusing to overwrite.
pub fn write_agent(dir: &Path, f: &AgentForm) -> Result<PathBuf, String> {
    if let Some(why) = name_problem(&f.name) {
        return Err(why.into());
    }
    if f.instructions.trim().is_empty() {
        return Err("Escreva as instruções: é o que o agente segue.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Não consegui criar a pasta: {e}"))?;
    let path = dir.join(format!("{}.md", f.name.trim()));
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file
            .write_all(render_file(f).as_bytes())
            .map(|_| path)
            .map_err(|e| format!("Não consegui gravar o arquivo: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "Já existe um agente chamado “{}”. Escolha outro nome; nada foi sobrescrito.",
            f.name.trim()
        )),
        Err(e) => Err(format!("Não consegui criar o arquivo: {e}")),
    }
}

/// Every definition the window should show, sorted by name: the visible ones
/// plus the disabled ones, which only appear here — a turn never sees them.
pub fn all(root: &Path) -> Vec<AgentView> {
    let cwd = Some(crate::turn::last_workspace(&Paths::with_root(root).db()));
    let extra = zerith_plugins::Library::open(&root.join("plugins"))
        .and_then(|l| l.contributions())
        .map(|c| c.agents)
        .unwrap_or_default();
    let paths = Paths::with_root(root);
    let mut all = zerith_core::agents::list_with_extra(&paths, cwd.as_deref(), &extra);
    for d in zerith_core::agents::list_all(&paths, cwd.as_deref()) {
        if d.disabled && !all.iter().any(|a| a.name == d.name) {
            all.push(d);
        }
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.iter().map(AgentView::of).collect()
}

#[tauri::command]
pub async fn agents_list(state: State<'_, AppState>) -> Result<Vec<AgentView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || all(&root))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_create(
    state: State<'_, AppState>,
    form: AgentForm,
) -> Result<Vec<AgentView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = Paths::with_root(&root).agents();
        write_agent(&dir, &form)?;
        Ok::<_, String>(all(&root))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_toggle(
    state: State<'_, AppState>,
    name: String,
    disabled: bool,
) -> Result<Vec<AgentView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        zerith_core::agents_write::set_disabled(&Paths::with_root(&root), &name, disabled)?;
        Ok::<_, String>(all(&root))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_reveal(state: State<'_, AppState>) -> Result<(), String> {
    let dir = Paths::with_root(state.root()).agents();
    let _ = std::fs::create_dir_all(&dir);
    crate::wire::reveal(&dir)
}
