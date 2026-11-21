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
            "Delega uma tarefa a um subagente com contexto próprio; só o texto final dele volta para você. Use para explorar o código ou pesquisar sem encher seu contexto, e chame vários de uma vez para trabalho independente. Agentes: explore (somente leitura), plan (somente leitura, devolve um plano), general (todas as ferramentas) ou um definido em .zerith/agents/<nome>.md. O prompt precisa bastar sozinho: o subagente não vê esta conversa.",
            json!({"type": "object", "properties": {
                "description": {"type": "string", "description": "3 a 5 palavras"},
                "prompt": {"type": "string", "description": "a tarefa completa e o formato esperado da resposta"},
                "agent": {"type": "string", "description": "explore, plan, general ou nome de um agente definido (padrão: general)"},
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
    ]
}

fn result(args: &Value, exec: &Exec<'_>) -> Result<String> {
    let (session, svc) = exec.host.bound()?;
    svc.agent_result(&session, str_arg(args, "id")?)
        .map_err(ToolError::Failed)
}
