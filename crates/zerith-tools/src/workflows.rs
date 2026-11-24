use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use zerith_workflows::{
    execute_workflow, now_ms, AiExecutor, NodeKind, NoopAiExecutor, RunOptions, Store, Workflow,
    WorkflowEdge, WorkflowNode,
};

use zerith_security::{Action, Sandbox, Verdict};

use crate::fntool::{make, make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

pub type WorkflowChangeHook = Arc<dyn Fn() + Send + Sync>;
pub type WorkflowAiHook = Arc<dyn AiExecutor>;

fn change_hook_slot() -> &'static Mutex<Option<WorkflowChangeHook>> {
    static H: OnceLock<Mutex<Option<WorkflowChangeHook>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

fn ai_hook_slot() -> &'static Mutex<Option<WorkflowAiHook>> {
    static A: OnceLock<Mutex<Option<WorkflowAiHook>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// Registers a callback fired whenever an AI tool creates, updates, toggles, or
/// deletes a workflow so desktop UIs can refresh live.
pub fn set_workflow_change_hook(cb: WorkflowChangeHook) {
    if let Ok(mut g) = change_hook_slot().lock() {
        *g = Some(cb);
    }
}

/// Registers the runtime AI executor used when `workflow_run` executes an
/// `ai_agent` node outside the desktop command layer.
pub fn set_workflow_ai_executor(ai: WorkflowAiHook) {
    if let Ok(mut g) = ai_hook_slot().lock() {
        *g = Some(ai);
    }
}

fn notify_changed() {
    let cb = change_hook_slot().lock().ok().and_then(|g| g.clone());
    if let Some(f) = cb {
        f();
    }
}

fn data_root() -> PathBuf {
    match std::env::var_os("ZERITH_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => zerith_core::Paths::resolve().root().to_path_buf(),
    }
}

fn open_store() -> Result<(PathBuf, Store)> {
    let root = data_root();
    let db = root.join("workflows.db");
    let store = Store::open(&db).map_err(|e| ToolError::Failed(e.to_string()))?;
    store
        .seed_defaults()
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok((root, store))
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make(
            "workflow_list",
            "Lista todos os workflows/automações globais salvos no Zerith, incluindo status (ativo/pausado), gatilho Cron, última execução e resumo dos blocos.",
            json!({"type": "object", "properties": {}}),
            "lista JSON/legível dos workflows globais cadastrados",
            Permission::Read,
            5,
            true,
            workflow_list,
        ),
        make(
            "channel_list",
            "Lista os canais globais de notificação configurados (WhatsApp Evolution API, WAHA, Cloud API, Telegram, Discord, Slack, Webhook, Notificação Local) com seus ids para usar em nós notify_channel.",
            json!({"type": "object", "properties": {}}),
            "lista de canais disponíveis sem expor chaves nem tokens secretos",
            Permission::Read,
            5,
            true,
            channel_list,
        ),
        make_with(
            "workflow_create_or_update",
            "Cria ou atualiza um workflow visual global no Zerith (salvo em ~/.zerith/workflows.db e sincronizado ao vivo no Canvas). Sempre prefira nós sem gasto de token (trigger_cron, shell, ssh_command, http_request, transform, condition) antes de ai_agent e notify_channel.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "id opcional para atualizar um workflow existente; omita para criar novo"},
                    "name": {"type": "string", "description": "título curto do workflow"},
                    "description": {"type": "string", "description": "resumo de 1 frase do que a automação faz"},
                    "icon": {"type": "string", "enum": ["workflow", "activity", "globe", "bot", "bell", "terminal", "file-text"], "description": "ícone no roster lateral"},
                    "bot_name": {"type": "string", "description": "nome opcional do bot especialista responsável pelo workflow"},
                    "enabled": {"type": "boolean", "description": "true para ativar o agendamento cron automaticamente; false para salvar pausado (padrão false)"},
                    "nodes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "kind": {"type": "string", "enum": ["trigger_cron", "trigger_manual", "trigger_webhook", "shell", "ssh_command", "http_request", "ai_agent", "condition", "transform", "delay", "loop", "notify_channel", "save_file"]},
                                "label": {"type": "string"},
                                "x": {"type": "number"},
                                "y": {"type": "number"},
                                "config": {"type": "object"}
                            },
                            "required": ["id", "kind", "label", "config"]
                        }
                    },
                    "edges": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "from": {"type": "string"},
                                "to": {"type": "string"},
                                "branch": {"type": ["string", "null"], "description": "null ou 'true'/'false' após nó condition"}
                            },
                            "required": ["from", "to"]
                        }
                    }
                },
                "required": ["name", "nodes", "edges"]
            }),
            "workflow criado ou atualizado com seu id e resumo do grafo",
            Permission::Write,
            10,
            false,
            |_, _| Ok(Some(Action::Write(data_root().join("workflows.db")))),
            workflow_create_or_update,
        ),
        make_with(
            "workflow_toggle",
            "Ativa ou pausa um workflow global pelo seu id.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "enabled": {"type": "boolean"}
                },
                "required": ["id", "enabled"]
            }),
            "confirmação do novo estado do workflow",
            Permission::Write,
            5,
            false,
            |_, _| Ok(Some(Action::Write(data_root().join("workflows.db")))),
            workflow_toggle,
        ),
        make_with(
            "workflow_delete",
            "Remove um workflow global pelo seu id.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
            "confirmação da exclusão",
            Permission::Write,
            5,
            false,
            |_, _| Ok(Some(Action::Write(data_root().join("workflows.db")))),
            workflow_delete,
        ),
        make_with(
            "workflow_run",
            "Executa um workflow global imediatamente para testar ou rodar sob demanda e retorna os logs de cada bloco.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
            "status final da execução e saída de cada nó",
            Permission::Shell,
            120,
            false,
            |args, _| {
                Ok(Some(Action::Shell(format!(
                    "workflow_run {}",
                    args["id"].as_str().unwrap_or("")
                ))))
            },
            workflow_run,
        ),
    ]
}

fn workflow_list(_: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (_, store) = open_store()?;
    let wfs = store
        .list_workflows()
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if wfs.is_empty() {
        return Ok("Nenhum workflow cadastrado.".into());
    }
    let lines: Vec<Value> = wfs
        .into_iter()
        .map(|w| {
            let cron = w
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::TriggerCron)
                .and_then(|n| n.config.get("cron").and_then(|v| v.as_str()))
                .unwrap_or("manual");
            json!({
                "id": w.id,
                "name": w.name,
                "enabled": w.enabled,
                "trigger": cron,
                "nodes": w.nodes.len(),
                "last_status": w.last_status,
                "description": w.description,
            })
        })
        .collect();
    serde_json::to_string_pretty(&lines).map_err(|e| ToolError::Failed(e.to_string()))
}

fn channel_list(_: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (_, store) = open_store()?;
    let channels = store
        .list_channels()
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if channels.is_empty() {
        return Ok("Nenhum canal configurado ainda em Workflows → Canais.".into());
    }
    let safe: Vec<Value> = channels
        .into_iter()
        .map(|c| {
            let has_target = c
                .config
                .get("default_target")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            json!({
                "id": c.id,
                "name": c.name,
                "kind": c.kind,
                "enabled": c.enabled,
                "has_default_target": has_target
            })
        })
        .collect();
    serde_json::to_string_pretty(&safe).map_err(|e| ToolError::Failed(e.to_string()))
}

fn workflow_create_or_update(args: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (_, store) = open_store()?;
    let name = str_arg(args, "name")?.trim().to_string();
    let existing_id = opt_str(args, "id").map(str::to_string);
    let existing = existing_id
        .as_deref()
        .and_then(|id| store.get_workflow(id).ok().flatten());

    let raw_nodes = args["nodes"]
        .as_array()
        .ok_or_else(|| ToolError::BadArguments("nodes precisa ser um array".into()))?;
    if raw_nodes.is_empty() {
        return Err(ToolError::BadArguments(
            "nodes precisa ter pelo menos 1 bloco".into(),
        ));
    }

    let mut nodes = Vec::with_capacity(raw_nodes.len());
    for (idx, rn) in raw_nodes.iter().enumerate() {
        // Crons without `tz_offset_minutes` follow the OS local offset per tick.
        let node: WorkflowNode = serde_json::from_value(json!({
            "id": rn.get("id").and_then(|v| v.as_str()).unwrap_or(&format!("n{}", idx + 1)),
            "kind": rn.get("kind").cloned().unwrap_or(Value::String("shell".into())),
            "label": rn.get("label").and_then(|v| v.as_str()).unwrap_or("Node"),
            "x": rn.get("x").and_then(|v| v.as_f64()).unwrap_or(-1.0),
            "y": rn.get("y").and_then(|v| v.as_f64()).unwrap_or(-1.0),
            "config": rn.get("config").cloned().unwrap_or_else(|| json!({}))
        }))
        .map_err(|e| ToolError::BadArguments(format!("nó inválido no índice {idx}: {e}")))?;

        nodes.push(node);
    }

    let raw_edges = args["edges"]
        .as_array()
        .ok_or_else(|| ToolError::BadArguments("edges precisa ser um array".into()))?;
    let mut edges = Vec::with_capacity(raw_edges.len());
    for (idx, re) in raw_edges.iter().enumerate() {
        let from = re
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::BadArguments(format!("edge[{idx}].from obrigatório")))?
            .to_string();
        let to = re
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::BadArguments(format!("edge[{idx}].to obrigatório")))?
            .to_string();
        let branch = re
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let id = re
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("e{}", idx + 1))
            .to_string();
        edges.push(WorkflowEdge {
            id,
            from,
            to,
            branch,
        });
    }

    if nodes.iter().any(|n| n.x < 0.0 || n.y < 0.0) {
        auto_layout_nodes(&mut nodes, &edges);
    }

    let now = now_ms();
    let wf = Workflow {
        alert: existing
            .as_ref()
            .map(|e| e.alert.clone())
            .unwrap_or_default(),
        id: existing_id.unwrap_or_else(|| format!("wf-{now}")),
        name,
        description: args["description"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(|e| e.description.clone()))
            .unwrap_or_default(),
        icon: args["icon"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| existing.as_ref().map(|e| e.icon.clone()))
            .unwrap_or_else(|| "workflow".into()),
        enabled: args["enabled"]
            .as_bool()
            .or_else(|| existing.as_ref().map(|e| e.enabled))
            .unwrap_or(false),
        nodes,
        edges,
        created_at_ms: existing.as_ref().map_or(now, |e| e.created_at_ms),
        updated_at_ms: now,
        last_run_at_ms: existing.as_ref().and_then(|e| e.last_run_at_ms),
        last_status: existing.as_ref().and_then(|e| e.last_status.clone()),
        bot_name: opt_str(args, "bot_name")
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|e| e.bot_name.clone())),
    };

    let saved = store
        .save_workflow(&wf)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    notify_changed();

    Ok(json!({
        "saved": true,
        "id": saved.id,
        "name": saved.name,
        "enabled": saved.enabled,
        "nodes": saved.nodes.len(),
        "edges": saved.edges.len(),
    })
    .to_string())
}

fn auto_layout_nodes(nodes: &mut [WorkflowNode], edges: &[WorkflowEdge]) {
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut indeg: HashMap<String, usize> = nodes.iter().map(|n| (n.id.clone(), 0usize)).collect();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for e in edges {
        if ids.contains(e.from.as_str()) && ids.contains(e.to.as_str()) {
            adj.entry(e.from.clone()).or_default().push(e.to.clone());
            if let Some(d) = indeg.get_mut(&e.to) {
                *d += 1;
            }
        }
    }
    let mut depth: HashMap<String, usize> = HashMap::new();
    let mut q: VecDeque<String> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| k.clone())
        .collect();
    for k in &q {
        depth.insert(k.clone(), 0);
    }
    while let Some(u) = q.pop_front() {
        let d = depth.get(&u).copied().unwrap_or(0);
        if let Some(vs) = adj.get(&u) {
            for v in vs {
                let entry = depth.entry(v.clone()).or_insert(0);
                if *entry < d + 1 {
                    *entry = d + 1;
                }
                if let Some(deg) = indeg.get_mut(v) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        q.push_back(v.clone());
                    }
                }
            }
        }
    }
    let mut row_counts: HashMap<usize, usize> = HashMap::new();
    for node in nodes.iter_mut() {
        let col = depth.get(&node.id).copied().unwrap_or(0);
        let row = row_counts.entry(col).or_insert(0);
        node.x = 40.0 + (col as f64) * 268.0;
        node.y = 120.0 + (*row as f64) * 145.0;
        *row += 1;
    }
}

fn workflow_toggle(args: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (_, store) = open_store()?;
    let id = str_arg(args, "id")?;
    let enabled = args["enabled"]
        .as_bool()
        .ok_or_else(|| ToolError::BadArguments("enabled precisa ser booleano".into()))?;
    store
        .set_workflow_enabled(id, enabled)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    notify_changed();
    Ok(format!(
        "Workflow '{id}' {}",
        if enabled { "ativado" } else { "pausado" }
    ))
}

fn workflow_delete(args: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (_, store) = open_store()?;
    let id = str_arg(args, "id")?;
    store
        .delete_workflow(id)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    notify_changed();
    Ok(format!("Workflow '{id}' removido"))
}

fn authorize_workflow_nodes(wf: &Workflow, ctx: &CallCtx<'_>) -> Result<()> {
    if ctx.policy.preset.sandbox == Sandbox::ReadOnly
        && wf.nodes.iter().any(|n| {
            matches!(
                n.kind,
                NodeKind::Shell
                    | NodeKind::SshCommand
                    | NodeKind::SaveFile
                    | NodeKind::HttpRequest
                    | NodeKind::Loop
            )
        })
    {
        return Err(ToolError::Denied(
            "workflow bloqueado no sandbox somente-leitura".into(),
        ));
    }
    for node in &wf.nodes {
        match node.kind {
            NodeKind::SaveFile => {
                if let Some(path) = node.config.get("path").and_then(|v| v.as_str()) {
                    if let Verdict::Deny(r) = ctx
                        .policy
                        .check_tool("write_file", &Action::Write(ctx.resolve(path)))
                    {
                        return Err(ToolError::Denied(r));
                    }
                }
            }
            NodeKind::Shell => {
                if let Some(cmd) = node.config.get("command").and_then(|v| v.as_str()) {
                    if let Verdict::Deny(r) = ctx
                        .policy
                        .check_tool("shell", &Action::Shell(cmd.to_string()))
                    {
                        return Err(ToolError::Denied(r));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn workflow_run(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let (root, store) = open_store()?;
    let id = str_arg(args, "id")?;
    let wf = store
        .get_workflow(id)
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .ok_or_else(|| ToolError::Failed(format!("workflow '{id}' não encontrado")))?;

    authorize_workflow_nodes(&wf, ctx)?;

    let registered_ai = ai_hook_slot().lock().ok().and_then(|g| g.clone());
    let fallback_ai = NoopAiExecutor;
    let ai_ref: &dyn AiExecutor = match registered_ai.as_deref() {
        Some(a) => a,
        None => &fallback_ai,
    };

    let mut noop = |_| {};
    let mut opts = RunOptions {
        store: &store,
        data_root: &root,
        ai: ai_ref,
        trigger_source: "ai_tool",
        tz_offset_minutes: zerith_workflows::local_offset_minutes(zerith_workflows::unix_secs()),
        initial_input: None,
        on_event: &mut noop,
    };
    let run = execute_workflow(&wf, &mut opts).map_err(|e| ToolError::Failed(e.to_string()))?;
    notify_changed();

    serde_json::to_string_pretty(&json!({
        "run_id": run.id,
        "status": run.status,
        "summary": run.summary,
        "node_logs": run.node_logs,
    }))
    .map_err(|e| ToolError::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use zerith_security::{preset, Mode, Policy, Rule, RuleKind};

    fn make_ctx<'a>(policy: &'a Policy) -> CallCtx<'a> {
        CallCtx {
            workspace: policy.workspace.as_path(),
            policy,
            memory: None,
            scope: None,
        }
    }

    #[test]
    fn plan_and_readonly_deny_workflow_mutations_and_execution() {
        let reg = Registry::new();
        let ws = std::path::Path::new(".");
        let create_args = json!({
            "name": "wf",
            "nodes": [{"id": "n1", "kind": "shell", "label": "sh", "config": {"command": "echo oi"}}],
            "edges": []
        });

        let plan_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Plan, ws);
        let plan_ctx = make_ctx(&plan_pol);
        for (tool, args) in [
            ("workflow_create_or_update", create_args.clone()),
            ("workflow_toggle", json!({"id": "wf-1", "enabled": true})),
            ("workflow_delete", json!({"id": "wf-1"})),
            ("workflow_run", json!({"id": "wf-1"})),
        ] {
            let (v, _) = reg.authorize(tool, &args, &plan_ctx).unwrap();
            assert!(matches!(v, Verdict::Deny(_)), "{tool} in plan: {v:?}");
            assert!(matches!(
                reg.call(tool, &args, &plan_ctx),
                Err(ToolError::Denied(_))
            ));
        }

        let ro_pol = Policy::new(preset("read-only").unwrap(), Mode::Bypass, ws);
        let ro_ctx = make_ctx(&ro_pol);
        for (tool, args) in [
            ("workflow_create_or_update", create_args),
            ("workflow_toggle", json!({"id": "wf-1", "enabled": true})),
            ("workflow_delete", json!({"id": "wf-1"})),
        ] {
            let (v, _) = reg.authorize(tool, &args, &ro_ctx).unwrap();
            assert!(matches!(v, Verdict::Deny(_)), "{tool} in readonly: {v:?}");
        }

        let shell_wf = Workflow {
            alert: Default::default(),
            id: "wf-1".into(),
            name: "sh".into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: false,
            nodes: vec![WorkflowNode {
                id: "n1".into(),
                kind: NodeKind::Shell,
                label: "sh".into(),
                x: 0.0,
                y: 0.0,
                config: json!({"command": "rm -rf /"}),
            }],
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        };
        assert!(matches!(
            authorize_workflow_nodes(&shell_wf, &ro_ctx),
            Err(ToolError::Denied(_))
        ));

        let mut deny_shell_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, ws);
        deny_shell_pol.rules = vec![Rule::parse(RuleKind::Deny, "shell(rm *)").unwrap()];
        let deny_shell_ctx = make_ctx(&deny_shell_pol);
        assert!(matches!(
            authorize_workflow_nodes(&shell_wf, &deny_shell_ctx),
            Err(ToolError::Denied(_))
        ));

        let save_wf = Workflow {
            alert: Default::default(),
            id: "wf-2".into(),
            name: "save".into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: false,
            nodes: vec![WorkflowNode {
                id: "n1".into(),
                kind: NodeKind::SaveFile,
                label: "save".into(),
                x: 0.0,
                y: 0.0,
                config: json!({"path": "out.txt"}),
            }],
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        };
        assert!(matches!(
            authorize_workflow_nodes(&save_wf, &plan_ctx),
            Err(ToolError::Denied(_))
        ));
    }
}
