//! Plan mode and subagents: many turns, bounded concurrency, durable state.
//!
//! The coordinator claims tasks; workers run them. Both go through the same
//! \`Store\` schema, so a crash mid-plan leaves rows a resume can read. The
//! concurrency limit is a hard cap, not a hint: the machine has one GPU-bound
//! provider and N parallel turns do not make it faster.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;

use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::Client;
use zerith_sessions::plan::{NewTask, Task, TaskState};
use zerith_sessions::{SessionSource, Store};

use crate::{run_turn_with_memory, RunError, TurnReport};

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("no tasks in plan {0}")]
    Empty(String),
    #[error("worker thread panicked")]
    Worker,
}

pub type Result<T> = std::result::Result<T, PlanError>;

/// What one plan did, per task. A UI shows this instead of guessing.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanReport {
    pub plan_id: String,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub tasks: Vec<Task>,
}

/// Splits a goal into tasks. One task per line, \`title | prompt\`, so the same
/// text a user types in plan mode is what gets scheduled — no hidden model
/// call deciding what the work is.
pub fn parse_plan(plan_id: &str, text: &str) -> Vec<NewTask> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .enumerate()
        .map(|(i, line)| {
            let (title, prompt) = match line.split_once('|') {
                Some((t, p)) => (t.trim(), p.trim()),
                None => (line, line),
            };
            let depends_on = if i == 0 {
                Vec::new()
            } else {
                vec![format!("{plan_id}-{:03}", i - 1)]
            };
            NewTask {
                title: title.to_string(),
                prompt: prompt.to_string(),
                agent: "default".to_string(),
                depends_on,
            }
        })
        .collect()
}

/// Runs a plan that is already in the store.
///
/// Each worker opens its own connection: SQLite wants one writer per process
/// per handle, and sharing a \`Connection\` across threads is not allowed.
#[allow(clippy::too_many_arguments)]
pub fn run_plan(
    db: &Path,
    plan_id: &str,
    model: &str,
    concurrency: usize,
    scope: Option<Scope>,
    on_task: impl Fn(&Task, TaskState) + Send + Sync + 'static,
) -> Result<PlanReport> {
    let concurrency = concurrency.clamp(1, 8);
    let store = Store::open(db)?;
    let tasks = store.tasks(plan_id)?;
    if tasks.is_empty() {
        return Err(PlanError::Empty(plan_id.to_string()));
    }
    // A previous process may have died holding a task. Put it back before
    // planning, or the plan stalls forever on a row nobody owns.
    store.requeue_running(plan_id)?;

    let on_task = std::sync::Arc::new(on_task);
    let (tx, rx) = mpsc::channel::<Task>();
    let rx = std::sync::Arc::new(Mutex::new(rx));
    let db_buf = db.to_path_buf();
    let model_buf = model.to_string();

    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let rx = rx.clone();
        let db = db_buf.clone();
        let model = model_buf.clone();
        let scope = scope.clone();
        let on_task = on_task.clone();
        workers.push(std::thread::spawn(move || {
            let store = match Store::open(&db) {
                Ok(s) => s,
                Err(_) => return,
            };
            loop {
                let task = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    match guard.recv() {
                        Ok(t) => t,
                        Err(_) => return,
                    }
                };
                on_task(&task, TaskState::Running);
                let report = run_one(&store, &model, scope.as_ref(), &task);
                match report {
                    Ok(r) => {
                        store
                            .finish_task(
                                &task.id,
                                TaskState::Done,
                                Some(&r.text),
                                None,
                                Some(&r.session_id),
                            )
                            .ok();
                        on_task(&task, TaskState::Done);
                    }
                    Err(e) => {
                        store
                            .finish_task(
                                &task.id,
                                TaskState::Failed,
                                None,
                                Some(&e.to_string()),
                                None,
                            )
                            .ok();
                        on_task(&task, TaskState::Failed);
                    }
                }
            }
        }));
    }

    // Coordinator: claim what is ready, skip what can never run.
    let mut running = 0usize;
    loop {
        let current = store.tasks(plan_id)?;
        let terminal: HashSet<&str> = current
            .iter()
            .filter(|t| t.state.is_terminal())
            .map(|t| t.id.as_str())
            .collect();
        let failed: HashSet<&str> = current
            .iter()
            .filter(|t| matches!(t.state, TaskState::Failed | TaskState::Skipped))
            .map(|t| t.id.as_str())
            .collect();

        let mut progressed = false;
        for t in current.iter().filter(|t| t.state == TaskState::Pending) {
            if t.depends_on.iter().any(|d| failed.contains(d.as_str())) {
                store
                    .finish_task(
                        &t.id,
                        TaskState::Skipped,
                        None,
                        Some("dependency failed"),
                        None,
                    )
                    .ok();
                on_task(t, TaskState::Skipped);
                progressed = true;
                continue;
            }
            if !t.depends_on.iter().all(|d| terminal.contains(d.as_str())) {
                continue;
            }
            if store.claim_task(&t.id)? {
                running += 1;
                progressed = true;
                let _ = tx.send(t.clone());
            }
        }

        let current = store.tasks(plan_id)?;
        let remaining = current.iter().filter(|t| !t.state.is_terminal()).count();
        if remaining == 0 {
            break;
        }
        if !progressed && running == 0 {
            // Nothing ready and nothing in flight: the plan depends on tasks
            // that do not exist. Say so instead of hanging.
            break;
        }
        if !progressed {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    drop(tx);
    for w in workers {
        let _ = w.join();
    }

    let tasks = store.tasks(plan_id)?;
    let done = tasks.iter().filter(|t| t.state == TaskState::Done).count();
    let failed = tasks
        .iter()
        .filter(|t| t.state == TaskState::Failed)
        .count();
    let skipped = tasks
        .iter()
        .filter(|t| t.state == TaskState::Skipped)
        .count();
    Ok(PlanReport {
        plan_id: plan_id.to_string(),
        done,
        failed,
        skipped,
        tasks,
    })
}

fn run_one(
    store: &Store,
    model: &str,
    scope: Option<&Scope>,
    task: &Task,
) -> std::result::Result<TurnReport, RunError> {
    let session = store.create_session(&task.title, SessionSource::Native)?;
    let client = Client::new(zerith_providers::ProviderConfig::from_env())?;
    // Memory is opened per worker for the same reason the store is: one
    // connection per thread.
    let memory = store
        .conn()
        .path()
        .and_then(|p| zerith_memory::SqliteMemory::open(p).ok());
    run_turn_with_memory(
        store,
        &client,
        memory.as_ref().map(|m| m as &dyn MemoryBackend),
        scope,
        &session.id,
        model,
        &task.prompt,
        |_| {},
    )
}

/// Where a plan's database lives, so a caller does not have to know the layout.
pub fn db_path(root: &Path) -> PathBuf {
    root.join("zerith.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_one_task_per_line_and_is_a_chain() {
        let tasks = parse_plan("p", "# comentário\nalfa | faz alfa\n\nbeta | faz beta\n");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "alfa");
        assert_eq!(tasks[0].prompt, "faz alfa");
        assert!(tasks[0].depends_on.is_empty());
        assert_eq!(tasks[1].depends_on, vec!["p-000".to_string()]);
    }

    #[test]
    fn a_line_without_a_bar_is_its_own_prompt() {
        let tasks = parse_plan("p", "só o título");
        assert_eq!(tasks[0].title, "só o título");
        assert_eq!(tasks[0].prompt, "só o título");
    }
}
