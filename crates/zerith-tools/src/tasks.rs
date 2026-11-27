//! `task_plan` / `task_create` / `task_update` / `task_list`: the model's own checklist.
//!
//! The tools validate the shape and forward to `Services::tasks` (the runtime
//! owns the database); after a change they queue `HostEvent::Tasks`, which the
//! loop turns into `RunEvent::Tasks`. Outside a turn there is no list.

use serde_json::{json, Value};

use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

pub const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "deleted"];
pub const PLAN_STATUSES: [&str; 6] = [
    "pending",
    "in_progress",
    "completed",
    "done",
    "skipped",
    "failed",
];
const PLAN_MAX_STEPS: usize = 15;

/// Argument checks for a task tool; `Ok` means the call is well-formed.
pub fn check(name: &str, args: &Value) -> Result<()> {
    match name {
        "task_plan" => {
            let Some(steps) = args["steps"].as_array() else {
                return Err(ToolError::BadArguments(
                    "steps precisa ser um array com 1 a 15 etapas".into(),
                ));
            };
            if steps.is_empty() || steps.len() > PLAN_MAX_STEPS {
                return Err(ToolError::BadArguments(format!(
                    "steps precisa ter entre 1 e {PLAN_MAX_STEPS} etapas (recebido {})",
                    steps.len()
                )));
            }
            for (i, step) in steps.iter().enumerate() {
                let Some(obj) = step.as_object() else {
                    return Err(ToolError::BadArguments(format!(
                        "steps[{i}] precisa ser um objeto"
                    )));
                };
                let title = obj
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| obj.get("subject").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if title.is_none() {
                    return Err(ToolError::BadArguments(format!(
                        "steps[{i}].title precisa ser uma string não vazia"
                    )));
                }
                match obj.get("status") {
                    None | Some(Value::Null) => {}
                    Some(v) => match v.as_str() {
                        Some(s) if PLAN_STATUSES.contains(&s) => {}
                        Some(s) => {
                            return Err(ToolError::BadArguments(format!(
                                "steps[{i}].status inválido: {s}; use {}",
                                PLAN_STATUSES.join(", ")
                            )));
                        }
                        None => {
                            return Err(ToolError::BadArguments(format!(
                                "steps[{i}].status precisa ser string; use {}",
                                PLAN_STATUSES.join(", ")
                            )));
                        }
                    },
                }
            }
            Ok(())
        }
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
            "task_plan",
            "Substitui ou atualiza o checklist ordenado de etapas da sessão em uma única chamada (steps: [{title, status}]). Mantenha uma ou mais etapas ativas em in_progress e marque as finalizadas como completed.",
            json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": PLAN_MAX_STEPS,
                        "description": "lista ordenada de etapas (1 a 15 itens)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "título curto da etapa"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "skipped", "failed"],
                                    "description": "estado da etapa (padrão: pending)"
                                }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["steps"]
            }),
            "resumo do plano atualizado",
            |a, _, e| run("task_plan", a, e),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_plan_check_validates_steps_and_statuses() {
        assert!(check("task_plan", &json!({})).is_err());
        assert!(check("task_plan", &json!({"steps": []})).is_err());
        let too_many: Vec<Value> = (0..16)
            .map(|i| json!({"title": format!("s{i}"), "status": "pending"}))
            .collect();
        assert!(check("task_plan", &json!({"steps": too_many})).is_err());

        assert!(check("task_plan", &json!({"steps": [{"title": "   "}]})).is_err());
        assert!(check(
            "task_plan",
            &json!({"steps": [{"title": "ok", "status": "feito"}]})
        )
        .is_err());
        assert!(check(
            "task_plan",
            &json!({"steps": [{"title": "ok", "status": 1}]})
        )
        .is_err());

        assert!(check(
            "task_plan",
            &json!({
                "steps": [
                    {"title": "Etapa 1", "status": "done"},
                    {"subject": "Etapa 2", "status": "completed"},
                    {"title": "Etapa 3", "status": "in_progress"},
                    {"title": "Etapa 4"},
                    {"title": "Etapa 5", "status": "skipped"},
                    {"title": "Etapa 6", "status": "failed"}
                ]
            })
        )
        .is_ok());
    }
}
