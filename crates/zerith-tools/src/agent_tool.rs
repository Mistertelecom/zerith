//! `agent` (subagents) and `agent_result` (collect a background one).
//!
//! The tool only parses and forwards to `Services::agent`; running a child
//! session is the runtime's job. Marked concurrency-safe: several `agent`
//! calls in one assistant message run in parallel, bounded by the runtime's
//! process-wide subagent limit. A child that may itself spawn gets the serial
//! variant (`all(false)`): its children run under its own permit, one at a time.

use serde_json::{json, Value};

use crate::fntool::{make, opt_str, str_arg};
use crate::host::AgentRequest;
use crate::{Exec, Permission, Result, Tool, ToolError};

pub(crate) fn all(parallel: bool) -> Vec<Box<dyn Tool>> {
    vec![
        make(
            "agent",
            "Delega uma tarefa a um subagente com contexto próprio; só o texto final dele volta para você. Use para executar tarefas operacionais completas (SSH/Telnet/Serial, Proxmox, VPN, shell, edições de código), explorar o código ou pesquisar sem encher seu contexto, e chame vários de uma vez para trabalho independente. Agentes built-in: executor (execução operacional completa com todas as ferramentas), explore (somente leitura), plan (somente leitura, devolve um plano), general (todas as ferramentas) ou agentes especialistas dos plugins (engenheiro-infra, workflow-architect). O prompt precisa bastar sozinho: o subagente não vê esta conversa.",
            json!({"type": "object", "properties": {
                "description": {"type": "string", "description": "3 a 5 palavras"},
                "prompt": {"type": "string", "description": "a tarefa completa e o formato esperado da resposta"},
                "agent": {"type": "string", "description": "executor, explore, plan, general, engenheiro-infra, workflow-architect ou outro agente definido (padrão: general)"},
                "model": {"type": "string"},
                "isolation": {"type": "string", "enum": ["worktree"], "description": "use \"worktree\" para isolar alterações em um git worktree temporário"},
                "run_in_background": {"type": "boolean", "description": "devolve um id na hora; busque o resultado com agent_result"}
            }, "required": ["description", "prompt"]}),
            "o texto final do subagente",
            Permission::Read,
            900,
            parallel,
            |a, _, exec| {
                let req = AgentRequest {
                    description: str_arg(a, "description")?.to_string(),
                    prompt: str_arg(a, "prompt")?.to_string(),
                    agent: opt_str(a, "agent").map(str::to_string),
                    model: opt_str(a, "model").map(str::to_string),
                    background: a["run_in_background"].as_bool().unwrap_or(false),
                    isolation: opt_str(a, "isolation").map(str::to_string),
                };
                let (session, svc) = exec.host.bound()?;
                svc.agent(&session, req, exec.cancel, exec.deadline, exec.host.sink())
                    .map_err(ToolError::Failed)
            },
        ),
        make(
            "agent_result",
            "Estado ou resultado final de um subagente iniciado com run_in_background.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "running, ou o texto final do subagente",
            Permission::Read,
            10,
            true,
            |a, _, exec| result(a, exec),
        ),
        // Talking to children that run while this turn works (background, or
        // in parallel): concept after Codex multi-agent v2 (openai/codex,
        // Apache-2.0).
        make(
            "agent_list",
            "Lista os subagentes desta conversa que ainda estão rodando: id, descrição, passos feitos e estado.",
            json!({"type": "object", "properties": {}}),
            "uma linha por subagente ativo",
            Permission::Read,
            10,
            true,
            |a, _, exec| control("list", a, exec),
        ),
        make(
            "agent_send",
            "Manda uma mensagem a um subagente em execução (iniciado com run_in_background); ele a lê no próximo passo, como nota no contexto dele. Use para corrigir o rumo ou acrescentar uma informação sem pará-lo.",
            json!({"type": "object", "properties": {
                "id": {"type": "string", "description": "id do subagente (sub-N)"},
                "message": {"type": "string"}
            }, "required": ["id", "message"]}),
            "confirmação de entrega",
            Permission::Read,
            10,
            true,
            |a, _, exec| control("send", a, exec),
        ),
        make(
            "agent_stop",
            "Para um subagente em execução e os subagentes dele. Devolve o que ele já produziu; o custo dele conta para esta conversa.",
            json!({"type": "object", "properties": {
                "id": {"type": "string", "description": "id do subagente (sub-N)"}
            }, "required": ["id"]}),
            "o que o subagente já tinha produzido",
            Permission::Read,
            60,
            true,
            |a, _, exec| control("stop", a, exec),
        ),
    ]
}

fn control(op: &str, args: &Value, exec: &Exec<'_>) -> Result<String> {
    let (session, svc) = exec.host.bound()?;
    svc.agent_control(&session, op, args)
        .map_err(ToolError::Failed)
}

fn result(args: &Value, exec: &Exec<'_>) -> Result<String> {
    let (session, svc) = exec.host.bound()?;
    svc.agent_result(&session, str_arg(args, "id")?)
        .map_err(ToolError::Failed)
}
