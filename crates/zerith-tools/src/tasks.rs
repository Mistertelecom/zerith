//! `task_create` / `task_update` / `task_list`: the model's own checklist.
//!
//! The tools validate the shape and forward to `Services::tasks` (the runtime
//! owns the database); after a change they queue `HostEvent::Tasks`, which the
//! loop turns into `RunEvent::Tasks`. Outside a turn there is no list.

use serde_json::{json, Value};

use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

pub const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "deleted"];

/// Argument checks for a task tool; `Ok` means the call is well-formed.
pub fn check(name: &str, args: &Value) -> Result<()> {
    match name {
        "task_create" => str_arg(args, "subject").map(|_| ()),
        "task_update" => {
            if args["id"].as_i64().is_none() {
                return Err(ToolError::BadArguments("id precisa ser um inteiro".into()));
            }
            match args["status"].as_str() {
                Some(s) if !STATUSES.contains(&s) => Err(ToolError::BadArguments(format!(
                    "status inválido: {s}; use {}",
                    STATUSES.join(", ")
                ))),
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

fn run(name: &str, args: &Value, exec: &crate::Exec<'_>) -> Result<String> {
    check(name, args)?;
    let (session, svc) = exec.host.bound()?;
    let out = svc.tasks(&session, name, args).map_err(ToolError::Failed)?;
    if name != "task_list" {
        exec.host.sink().push(crate::HostEvent::Tasks);
    }
    Ok(out)
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    let t = |name, description, parameters, returns, run| {
        make(
            name,
            description,
            parameters,
            returns,
            Permission::Read,
            10,
            false,
            run,
        )
    };
    vec![
        t(
            "task_create",
            "Cria uma tarefa na lista da sessão. Use para trabalho com vários passos; mantenha exatamente uma tarefa in_progress por vez.",
            json!({"type": "object", "properties": {
                "subject": {"type": "string", "description": "título curto, no imperativo"},
                "description": {"type": "string", "description": "o que fazer e como saber que acabou"}
            }, "required": ["subject"]}),
            "o id da tarefa criada",
            |a, _, e| run("task_create", a, e),
        ),
        t(
            "task_update",
            "Atualiza uma tarefa: status (pending, in_progress, completed, deleted), título, descrição ou dependências.",
            json!({"type": "object", "properties": {
                "id": {"type": "integer"},
                "status": {"type": "string", "enum": STATUSES},
                "subject": {"type": "string"},
                "description": {"type": "string"},
                "add_blocks": {"type": "array", "items": {"type": "integer"}, "description": "tarefas que esta bloqueia"},
                "add_blocked_by": {"type": "array", "items": {"type": "integer"}, "description": "tarefas que precisam acabar antes"}
            }, "required": ["id"]}),
            "a tarefa atualizada",
            |a, _, e| run("task_update", a, e),
        ),
        t(
            "task_list",
            "Lista as tarefas da sessão com status e dependências.",
            json!({"type": "object", "properties": {}}),
            "uma tarefa por linha",
            |a, _, e| run("task_list", a, e),
        ),
    ]
}
