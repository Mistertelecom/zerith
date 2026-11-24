//! The session checklist behind `task_create` / `task_update` / `task_list`.
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
}
