//! Durable task rows for plan mode and subagents.
//!
//! A task is a row, not a value in a closure: a plan that only lives in memory
//! disappears with the crash it exists to survive. States are the only thing
//! that moves, and every move is a compare-and-set so two workers cannot both
//! claim the same task.

use crate::migrations::now_ms;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::store::{Result, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
    /// A dependency failed or was cancelled; this task will never run.
    Skipped,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => TaskState::Pending,
            "running" => TaskState::Running,
            "done" => TaskState::Done,
            "failed" => TaskState::Failed,
            "skipped" => TaskState::Skipped,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Skipped
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub plan_id: String,
    pub title: String,
    pub prompt: String,
    pub agent: String,
    /// Ids of tasks in the same plan that must finish first.
    pub depends_on: Vec<String>,
    pub session_id: Option<String>,
    pub state: TaskState,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What a caller supplies to schedule one task.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub title: String,
    pub prompt: String,
    pub agent: String,
    pub depends_on: Vec<String>,
}

const COLUMNS: &str = "id, plan_id, title, prompt, agent, depends_on, session_id, state, result, error, created_at, updated_at";

impl Store {
    /// Inserts the whole plan in one transaction. Either every task exists or
    /// none does; a half-written plan would run twice on resume.
    pub fn create_plan(&self, plan_id: &str, tasks: &[NewTask]) -> Result<Vec<Task>> {
        let ts = now_ms();
        let tx = self.write_tx()?;
        let mut out = Vec::with_capacity(tasks.len());
        for (i, t) in tasks.iter().enumerate() {
            let id = format!("{plan_id}-{i:03}");
            tx.execute(
                "INSERT INTO agent_tasks(id, plan_id, title, prompt, agent, depends_on, state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    id,
                    plan_id,
                    t.title,
                    t.prompt,
                    t.agent,
                    encode_deps(&t.depends_on),
                    TaskState::Pending.as_str(),
                    ts
                ],
            )?;
            out.push(Task {
                id,
                plan_id: plan_id.to_string(),
                title: t.title.clone(),
                prompt: t.prompt.clone(),
                agent: t.agent.clone(),
                depends_on: t.depends_on.clone(),
                session_id: None,
                state: TaskState::Pending,
                result: None,
                error: None,
                created_at: ts,
                updated_at: ts,
            });
        }
        tx.commit()?;
        Ok(out)
    }

    pub fn tasks(&self, plan_id: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM agent_tasks WHERE plan_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![plan_id], row_to_task)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Claims a pending task for one worker. Returns false when someone else
    /// got there first, which is what makes concurrency safe without a lock.
    pub fn claim_task(&self, id: &str) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE agent_tasks SET state = 'running', updated_at = ?2 WHERE id = ?1 AND state = 'pending'",
            params![id, now_ms()],
        )?;
        Ok(n == 1)
    }

    pub fn finish_task(
        &self,
        id: &str,
        state: TaskState,
        result: Option<&str>,
        error: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE agent_tasks SET state = ?2, result = ?3, error = ?4, session_id = COALESCE(?5, session_id), updated_at = ?6 WHERE id = ?1",
            params![id, state.as_str(), result, error, session_id, now_ms()],
        )?;
        Ok(())
    }

    /// Plans that still have work, so a resumed process knows what to pick up.
    pub fn unfinished_plans(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT plan_id FROM agent_tasks WHERE state IN ('pending', 'running') ORDER BY plan_id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// After a crash, a task that was running has no worker left. It goes back
    /// to pending so the next run retries it, and the attempt is not hidden.
    pub fn requeue_running(&self, plan_id: &str) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE agent_tasks SET state = 'pending', updated_at = ?2 WHERE plan_id = ?1 AND state = 'running'",
            params![plan_id, now_ms()],
        )?)
    }
}

fn encode_deps(deps: &[String]) -> Option<String> {
    if deps.is_empty() {
        None
    } else {
        serde_json::to_string(deps).ok()
    }
}

fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    let deps: Option<String> = r.get(5)?;
    let state: String = r.get(7)?;
    Ok(Task {
        id: r.get(0)?,
        plan_id: r.get(1)?,
        title: r.get(2)?,
        prompt: r.get(3)?,
        agent: r.get(4)?,
        depends_on: deps
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        session_id: r.get(6)?,
        state: TaskState::parse(&state).unwrap_or(TaskState::Pending),
        result: r.get(8)?,
        error: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// A plan id that sorts by creation and needs no uuid dependency.
pub fn task_id_seed() -> String {
    format!("{:x}-{:x}", now_ms(), std::process::id())
}

/// Reads one row directly (tests and the panel that shows what a plan did).
pub fn task(store: &Store, id: &str) -> Result<Option<Task>> {
    Ok(store
        .conn()
        .query_row(
            &format!("SELECT {COLUMNS} FROM agent_tasks WHERE id = ?1"),
            params![id],
            row_to_task,
        )
        .optional()?)
}
