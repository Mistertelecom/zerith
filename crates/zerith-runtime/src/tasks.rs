//! The session checklist behind `task_plan` / `task_create` / `task_update` / `task_list`.
//!
//! The tools (zerith-tools) validate arguments and call `run` through
//! `Services::tasks`; every call opens its own connection, because a tool may
//! run on a worker thread and a `Store` does not cross threads. The loop
//! publishes `RunEvent::Tasks` after a mutation (see `subagent::flush`).

use std::path::Path;

use serde_json::Value;
use zerith_sessions::{Store, Todo, TodoPatch, TodoStatus};

fn ids(args: &Value, key: &str) -> Vec<i64> {
    args[key]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn status_label(s: TodoStatus) -> &'static str {
    s.as_str()
}

/// One todo as the model reads it. Blockers already completed are not shown.
fn line(t: &Todo, all: &[Todo]) -> String {
    let open: Vec<String> = t
        .blocked_by
        .iter()
        .filter(|b| {
            all.iter()
                .find(|o| o.id == **b)
                .is_some_and(|o| o.status != TodoStatus::Completed)
        })
        .map(|b| format!("#{b}"))
        .collect();
    let mut s = format!("#{} [{}] {}", t.id, status_label(t.status), t.subject);
    if !open.is_empty() {
        s.push_str(&format!(" (bloqueada por {})", open.join(", ")));
    }
    s
}

/// `TaskCompleted` (Claude Code's name): before `task_update` marks an open
/// task `completed`. A blocking hook keeps it open; its reason is the
/// model's error. Any other call passes untouched.
pub fn completion_hook(
    db: &Path,
    settings: &zerith_core::Settings,
    cwd: &Path,
    session: &str,
    op: &str,
    args: &Value,
) -> Result<(), String> {
    use zerith_core::hooks::{run_hooks, HookEvent, HookPayload};
    if op != "task_update"
        || args["status"].as_str() != Some("completed")
        || settings.hooks.task_completed.is_empty()
    {
        return Ok(());
    }
    let store = Store::open(db).map_err(|e| format!("banco: {e}"))?;
    let id = args["id"].as_i64().unwrap_or(0);
    let Some(t) = store.todo(session, id).map_err(|e| e.to_string())? else {
        return Ok(()); // `run` reports the missing task
    };
    if t.status == TodoStatus::Completed {
        return Ok(());
    }
    let mut extra = serde_json::Map::new();
    extra.insert("task_id".into(), id.into());
    extra.insert("task_subject".into(), t.subject.clone().into());
    extra.insert("task_description".into(), t.description.into());
    let out = run_hooks(
        HookEvent::TaskCompleted,
        &HookPayload {
            session_id: session.to_string(),
            cwd: cwd.to_path_buf(),
            extra,
            ..Default::default()
        },
        settings,
    );
    for err in &out.errors {
        let _ = store.append_event(
            session,
            None,
            "hook/error",
            &serde_json::json!({"event": "TaskCompleted", "error": err}),
        );
    }
    if out.blocked() {
        let why = out.reasons.join("; ");
        return Err(format!(
            "tarefa #{id} continua aberta: um hook TaskCompleted recusou{}",
            if why.trim().is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        ));
    }
    Ok(())
}

/// Runs one task tool for `session`. `Err` is text for the model.
pub fn run(db: &Path, session: &str, op: &str, args: &Value) -> Result<String, String> {
    let store = Store::open(db).map_err(|e| format!("banco: {e}"))?;
    let e = |e: zerith_sessions::store::StoreError| e.to_string();
    match op {
        "task_plan" => {
            let Some(steps) = args["steps"].as_array() else {
                return Err("steps precisa ser um array com 1 a 15 etapas".into());
            };
            if steps.is_empty() || steps.len() > 15 {
                return Err(format!(
                    "steps precisa ter entre 1 e 15 etapas (recebido {})",
                    steps.len()
                ));
            }
            struct ParsedStep {
                title: String,
                status: TodoStatus,
                description: &'static str,
                raw_status: &'static str,
            }
            let mut parsed = Vec::with_capacity(steps.len());
            for (i, step) in steps.iter().enumerate() {
                let Some(obj) = step.as_object() else {
                    return Err(format!("steps[{i}] precisa ser um objeto"));
                };
                let Some(title) = obj
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| obj.get("subject").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return Err(format!("steps[{i}].title precisa ser uma string não vazia"));
                };
                let raw = match obj.get("status") {
                    None | Some(Value::Null) => "pending",
                    Some(v) => v
                        .as_str()
                        .ok_or_else(|| format!("steps[{i}].status precisa ser string"))?,
                };
                let (status, description, raw_status) = match raw {
                    "completed" | "done" => (TodoStatus::Completed, "", "completed"),
                    "in_progress" => (TodoStatus::InProgress, "", "in_progress"),
                    "pending" => (TodoStatus::Pending, "", "pending"),
                    "failed" => (TodoStatus::Completed, "[failed]", "failed"),
                    "skipped" => (TodoStatus::Completed, "[skipped]", "skipped"),
                    other => {
                        return Err(format!("steps[{i}].status inválido: {other}"));
                    }
                };
                parsed.push(ParsedStep {
                    title: title.to_string(),
                    status,
                    description,
                    raw_status,
                });
            }

            let existing = store.todo_list(session).map_err(e)?;
            for old in existing {
                let del = TodoPatch {
                    status: Some(TodoStatus::Deleted),
                    ..Default::default()
                };
                store.todo_update(session, old.id, &del).map_err(e)?;
            }

            let mut completed = 0usize;
            let mut in_progress = 0usize;
            let mut failed = 0usize;
            let mut skipped = 0usize;
            for step in &parsed {
                let created = store
                    .todo_create(session, &step.title, step.description)
                    .map_err(e)?;
                if step.status != TodoStatus::Pending {
                    let patch = TodoPatch {
                        status: Some(step.status),
                        ..Default::default()
                    };
                    store.todo_update(session, created.id, &patch).map_err(e)?;
                }
                match step.raw_status {
                    "completed" => completed += 1,
                    "in_progress" => in_progress += 1,
                    "failed" => failed += 1,
                    "skipped" => skipped += 1,
                    _ => {}
                }
            }

            let total = parsed.len();
            let mut details = vec![
                format!(
                    "{completed} concluída{}",
                    if completed == 1 { "" } else { "s" }
                ),
                format!("{in_progress} em andamento"),
            ];
            if failed > 0 {
                details.push(format!("{failed} com falha"));
            }
            if skipped > 0 {
                details.push(format!(
                    "{skipped} pulada{}",
                    if skipped == 1 { "" } else { "s" }
                ));
            }
            Ok(format!(
                "plano atualizado: {total} etapa{} ({})",
                if total == 1 { "" } else { "s" },
                details.join(", ")
            ))
        }
        "task_create" => {
            let t = store
                .todo_create(
                    session,
                    args["subject"].as_str().unwrap_or("").trim(),
                    args["description"].as_str().unwrap_or("").trim(),
                )
                .map_err(e)?;
            Ok(format!("tarefa #{} criada: {}", t.id, t.subject))
        }
        "task_update" => {
            let id = args["id"].as_i64().unwrap_or(0);
            let patch = TodoPatch {
                subject: args["subject"].as_str().map(str::to_string),
                description: args["description"].as_str().map(str::to_string),
                status: args["status"].as_str().and_then(TodoStatus::parse),
                add_blocks: ids(args, "add_blocks"),
                add_blocked_by: ids(args, "add_blocked_by"),
            };
            let t = store.todo_update(session, id, &patch).map_err(e)?;
            let all = store.todo_list(session).map_err(e)?;
            let mut out = format!("atualizada: {}", line(&t, &all));
            // One at a time is the whole point of the list: say it when it slips.
            if t.status == TodoStatus::InProgress {
                let others: Vec<String> = all
                    .iter()
                    .filter(|o| o.id != t.id && o.status == TodoStatus::InProgress)
                    .map(|o| format!("#{}", o.id))
                    .collect();
                if !others.is_empty() {
                    out.push_str(&format!(
                        "\n[aviso: também em andamento: {}; mantenha só uma in_progress]",
                        others.join(", ")
                    ));
                }
            }
            Ok(out)
        }
        "task_list" => {
            let all = store.todo_list(session).map_err(e)?;
            if all.is_empty() {
                return Ok("nenhuma tarefa".into());
            }
            Ok(all
                .iter()
                .map(|t| line(t, &all))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        other => Err(format!("operação desconhecida: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerith_sessions::SessionSource;

    #[test]
    fn create_update_list_flow_and_the_one_in_progress_warning() {
        let dir = tempfile::Builder::new()
            .tempdir_in(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/test-scratch"
            ))
            .unwrap();
        let db = dir.path().join("t.db");
        let sid = Store::open(&db)
            .unwrap()
            .create_session("t", SessionSource::Native)
            .unwrap()
            .id;
        let go = |op: &str, a: Value| run(&db, &sid, op, &a);

        assert_eq!(go("task_list", json!({})).unwrap(), "nenhuma tarefa");
        assert!(go("task_create", json!({"subject": "A"}))
            .unwrap()
            .contains("#1"));
        go("task_create", json!({"subject": "B"})).unwrap();
        go("task_update", json!({"id": 2, "add_blocked_by": [1]})).unwrap();
        let l = go("task_list", json!({})).unwrap();
        assert!(l.contains("#2 [pending] B (bloqueada por #1)"), "{l}");

        let first = go("task_update", json!({"id": 1, "status": "in_progress"})).unwrap();
        assert!(!first.contains("aviso"), "{first}");
        let second = go("task_update", json!({"id": 2, "status": "in_progress"})).unwrap();
        assert!(second.contains("também em andamento: #1"), "{second}");

        go("task_update", json!({"id": 1, "status": "completed"})).unwrap();
        let l = go("task_list", json!({})).unwrap();
        assert!(
            l.contains("#1 [completed] A") && !l.contains("bloqueada"),
            "{l}"
        );

        go("task_update", json!({"id": 1, "status": "deleted"})).unwrap();
        assert!(!go("task_list", json!({})).unwrap().contains("#1"));
        assert!(go("task_update", json!({"id": 99, "status": "completed"})).is_err());
    }

    #[test]
    fn task_plan_creates_replaces_and_maps_all_statuses() {
        let dir = tempfile::Builder::new()
            .tempdir_in(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/test-scratch"
            ))
            .unwrap();
        let db = dir.path().join("plan.db");
        let sid = Store::open(&db)
            .unwrap()
            .create_session("p", SessionSource::Native)
            .unwrap()
            .id;
        let go = |op: &str, a: Value| run(&db, &sid, op, &a);

        // Seed an older checklist item to ensure task_plan replaces it cleanly.
        go("task_create", json!({"subject": "antiga"})).unwrap();

        let summary = go(
            "task_plan",
            json!({
                "steps": [
                    {"title": "Ler código", "status": "done"},
                    {"subject": "Implementar ferramenta", "status": "completed"},
                    {"title": "Rodar testes", "status": "in_progress"},
                    {"title": "Revisar diff"},
                    {"title": "Deploy opcional", "status": "skipped"},
                    {"title": "Checagem falhou", "status": "failed"}
                ]
            }),
        )
        .unwrap();
        assert_eq!(
            summary,
            "plano atualizado: 6 etapas (2 concluídas, 1 em andamento, 1 com falha, 1 pulada)"
        );

        let todos = Store::open(&db).unwrap().todo_list(&sid).unwrap();
        assert_eq!(todos.len(), 6);
        assert_eq!(
            todos
                .iter()
                .map(|t| (t.subject.as_str(), t.status, t.description.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Ler código", TodoStatus::Completed, ""),
                ("Implementar ferramenta", TodoStatus::Completed, ""),
                ("Rodar testes", TodoStatus::InProgress, ""),
                ("Revisar diff", TodoStatus::Pending, ""),
                ("Deploy opcional", TodoStatus::Completed, "[skipped]"),
                ("Checagem falhou", TodoStatus::Completed, "[failed]"),
            ]
        );

        // Invalid call must fail without wiping the existing checklist.
        assert!(go(
            "task_plan",
            json!({"steps": [{"title": "x", "status": "bad"}]})
        )
        .is_err());
        assert_eq!(Store::open(&db).unwrap().todo_list(&sid).unwrap().len(), 6);

        // Replacing with a shorter list soft-deletes previous items cleanly.
        let summary2 = go(
            "task_plan",
            json!({
                "steps": [
                    {"title": "Passo A", "status": "completed"},
                    {"title": "Passo B", "status": "in_progress"}
                ]
            }),
        )
        .unwrap();
        assert_eq!(
            summary2,
            "plano atualizado: 2 etapas (1 concluída, 1 em andamento)"
        );
        let todos2 = Store::open(&db).unwrap().todo_list(&sid).unwrap();
        assert_eq!(
            todos2
                .iter()
                .map(|t| (t.subject.as_str(), t.status))
                .collect::<Vec<_>>(),
            vec![
                ("Passo A", TodoStatus::Completed),
                ("Passo B", TodoStatus::InProgress),
            ]
        );
    }
}
