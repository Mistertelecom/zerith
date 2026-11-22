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
    pub icon: Option<String>,
    pub quick_prompts: Vec<String>,
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
            icon: a.icon.clone(),
            quick_prompts: a.quick_prompts.clone(),
        }
    }
}

/// The form behind `Novo agente` / `Novo Bot`. Every field is plain text;
/// the file is only built when the person presses Save.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentForm {
    pub name: String,
    pub description: String,
    pub instructions: String,
    /// Comma-separated allowlist; empty inherits every tool.
    pub tools: String,
    pub model: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub quick_prompts: Vec<String>,
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
    let icon = one_line(&f.icon);
    if !icon.is_empty() && !icon.contains("---") {
        head.push_str(&format!("icon: {icon}\n"));
    }
    let qps: Vec<String> = f
        .quick_prompts
        .iter()
        .map(|q| one_line(q))
        .filter(|q| !q.is_empty() && !q.contains("---"))
        .collect();
    if !qps.is_empty() {
        head.push_str("quickPrompts:\n");
        for q in qps {
            head.push_str(&format!("  - \"{}\"\n", q.replace('"', "'")));
        }
    }
    let body = f.instructions.trim();
    if head.is_empty() {
        format!("{body}\n")
    } else {
        format!("---\n{head}---\n{body}\n")
    }
}

/// Writes `<dir>/<name>.md`, optionally overwriting an existing definition.
pub fn save_agent(dir: &Path, f: &AgentForm, overwrite: bool) -> Result<PathBuf, String> {
    if let Some(why) = name_problem(&f.name) {
        return Err(why.into());
    }
    if f.instructions.trim().is_empty() {
        return Err("Escreva as instruções: é o que o agente segue.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Não consegui criar a pasta: {e}"))?;
    let path = dir.join(format!("{}.md", f.name.trim()));
    let content = render_file(f);
    if overwrite {
        std::fs::write(&path, content.as_bytes())
            .map(|_| path)
            .map_err(|e| format!("Não consegui gravar o arquivo: {e}"))
    } else {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file
                .write_all(content.as_bytes())
                .map(|_| path)
                .map_err(|e| format!("Não consegui gravar o arquivo: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
                "Já existe um agente chamado “{}”. Escolha outro nome; nada foi sobrescrito.",
                f.name.trim()
            )),
            Err(e) => Err(format!("Não consegui criar o arquivo: {e}")),
        }
    }
}

/// Writes `<dir>/<name>.md`, refusing to overwrite.
pub fn write_agent(dir: &Path, f: &AgentForm) -> Result<PathBuf, String> {
    save_agent(dir, f, false)
}

/// Seeds ready-to-use specialist starter bots once per Zerith home directory.
/// Existing user files with the same name are never overwritten.
pub fn seed_starter_bots(root: &Path) {
    let dir = Paths::with_root(root).agents();
    let marker = dir.join(".seeded_v1");
    if marker.exists() {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let starters: [(&str, &str); 4] = [
        (
            "noc-engineer.md",
            r#"---
description: "NOC, redes, Mikrotik, OLT/FTTH, Linux & diagnósticos SSH"
icon: activity
quickPrompts:
  - "Diagnosticar latência, perda de pacotes e status de links via SSH"
  - "Revisar regras de firewall, BGP/OSPF ou PPPoE em roteador Mikrotik"
  - "Criar um workflow cron para monitorar servidores e avisar no WhatsApp"
---
Você é o @noc-engineer, especialista em operações de rede (NOC/ISP), Linux, Proxmox, Mikrotik RouterOS, BGP/OSPF e FTTH/OLT.
Seja direto, técnico e prático: proponha comandos diagnósticos seguros (somente leitura primeiro), analise métricas com precisão e sugira automações no Zerith Workflows quando fizer sentido.
"#,
        ),
        (
            "finance-analyst.md",
            r#"---
description: "Análise de mercado, B3/US/Crypto, indicadores e resumo macro"
icon: trending-up
quickPrompts:
  - "Resumir cenário macro do dia (Dólar, Juros, IBOV, S&P 500 e BTC)"
  - "Analisar fundamentos e riscos de um ativo ou carteira"
  - "Montar relatório executivo em Markdown sobre oportunidades da semana"
---
Você é o @finance-analyst, analista financeiro e quantitativo do Zerith.
Apresente cenários com rigor técnico (bull/base/bear), contexto macroeconômico, gestão de risco clara e indicadores objetivos, evitando recomendações cegas e destacando incertezas.
"#,
        ),
        (
            "workflow-architect.md",
            r#"---
description: "Criação, depuração e otimização de workflows e automações"
icon: bot
quickPrompts:
  - "Criar um workflow completo a partir da minha descrição"
  - "Otimizar meus workflows ativos para economizar tokens usando Transform/Condition"
  - "Configurar webhook externo (Zabbix/Grafana/GitHub) ligado a notificações"
---
Você é o @workflow-architect, arquiteto de automações e DAGs do Zerith Workflows Studio.
Sempre priorize nós determinísticos de custo zero (`trigger_cron`, `trigger_webhook`, `shell`, `ssh_command`, `http_request`, `loop`, `transform`, `condition`, `delay`) antes de acionar `ai_agent` ou `notify_channel`. Use os tools nativos `workflow_*` e `channel_list` quando aplicável.
"#,
        ),
        (
            "code-reviewer.md",
            r#"---
description: "Revisão rigorosa de diffs, segurança, performance e testes"
icon: shield
quickPrompts:
  - "Revisar as últimas alterações do repositório (git diff) em busca de bugs"
  - "Fazer auditoria rápida de segurança e tratamento de erros"
  - "Identificar lacunas de cobertura de testes nos arquivos modificados"
---
Você é o @code-reviewer, revisor sênior de engenharia de software.
Analise diffs, arquitetura, condições de corrida, segurança, edge cases e legibilidade. Aponte arquivo e linha exatos com correções objetivas.
"#,
        ),
    ];
    for (filename, body) in starters {
        let target = dir.join(filename);
        if !target.exists() {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .and_then(|mut f| std::io::Write::write_all(&mut f, body.as_bytes()));
        }
    }
    let _ = std::fs::write(&marker, b"v1\n");
}

/// Every definition the window should show, sorted by name: the visible ones
/// plus the disabled ones, which only appear here — a turn never sees them.
pub fn all(root: &Path) -> Vec<AgentView> {
    seed_starter_bots(root);
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
pub async fn agent_save(
    state: State<'_, AppState>,
    form: AgentForm,
) -> Result<Vec<AgentView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let dir = Paths::with_root(&root).agents();
        save_agent(&dir, &form, true)?;
        Ok::<_, String>(all(&root))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_delete(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<AgentView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(why) = name_problem(&name) {
            return Err(why.to_string());
        }
        let path = Paths::with_root(&root)
            .agents()
            .join(format!("{}.md", name.trim()));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Não consegui excluir o bot: {e}"))?;
        }
        crate::workflows::disable_bot_workflows(&root, &name)?;
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
        if disabled {
            crate::workflows::disable_bot_workflows(&root, &name)?;
        }
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
