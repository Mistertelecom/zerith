//! Plan mode and subagents: many turns, bounded concurrency, durable state.
//!
//! The coordinator claims tasks; workers run them. Both go through the same
//! `Store` schema, so a crash mid-plan leaves rows a resume can read. The
//! concurrency limit is a hard cap, not a hint: the machine has one GPU-bound
//! provider and N parallel turns do not make it faster.
//!
//! Workers never write task rows. They report every outcome — success, error,
//! panic — over a channel, and the coordinator is the single writer. That is
//! what keeps the in-flight count honest: a worker that dies still answers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{Cancel, Client};
use zerith_sessions::plan::{NewTask, Task, TaskState};
use zerith_sessions::{SessionSource, Store};

use crate::RunError;

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("no tasks in plan {0}")]
    Empty(String),
    #[error("worker thread panicked")]
    Worker,
    #[error("plano inválido: {0}")]
    Parse(String),
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

/// Limits for one task. `None` means unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskBudget {
    /// How many model steps a task may take. Today a task is one turn, so this
    /// only bites at 0; it is here so a multi-step loop has a place to check.
    pub max_steps: Option<u32>,
    /// Wall-clock limit. Enforced by pulling the task's cancel token, so a
    /// streaming turn stops at its next network read.
    pub max_time: Option<Duration>,
}

/// How a plan runs.
#[derive(Clone)]
pub struct PlanOptions {
    pub concurrency: usize,
    /// Pulled by the host to stop the whole plan. Pending tasks are skipped,
    /// running ones see the cancel on their next check.
    pub cancel: Cancel,
    pub budget: TaskBudget,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            concurrency: 2,
            cancel: Cancel::default(),
            budget: TaskBudget::default(),
        }
    }
}

/// What a runner gets for one task: its own cancel token (plan cancel or
/// time budget pulls it) and a step counter.
pub struct TaskCtx {
    pub cancel: Cancel,
    pub budget: TaskBudget,
    started: Instant,
    steps: AtomicU32,
}

impl TaskCtx {
    fn new(cancel: Cancel, budget: TaskBudget) -> Self {
        Self {
            cancel,
            budget,
            started: Instant::now(),
            steps: AtomicU32::new(0),
        }
    }

    /// Call before each model step. Errs when cancelled or over budget.
    pub fn step(&self) -> std::result::Result<(), String> {
        if self.timed_out() {
            return Err("tempo esgotado".into());
        }
        if self.cancel.is_cancelled() {
            return Err("cancelado".into());
        }
        let n = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
        match self.budget.max_steps {
            Some(max) if n > max => Err(format!("limite de {max} passo(s) excedido")),
            _ => Ok(()),
        }
    }

    pub fn timed_out(&self) -> bool {
        self.budget
            .max_time
            .is_some_and(|t| self.started.elapsed() >= t)
    }
}

/// What a runner returns on success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    pub text: String,
    pub session_id: Option<String>,
}

/// Splits a goal into tasks, all independent (parallel). One task per line:
///
/// ```text
/// title | prompt
/// title | prompt | after: 1,3     (1-based task line numbers)
/// @revisor title | prompt          (runs as agent `revisor`)
/// ```
///
/// Invalid `after:` references are dropped; use [`parse_plan_opts`] to see
/// them as errors.
pub fn parse_plan(plan_id: &str, text: &str) -> Vec<NewTask> {
    parse_lines(plan_id, text, false).0
}

/// Same grammar. `sequential` chains every task on the previous one (the
/// `--seq` behavior). A reference must point to an earlier line, which rules
/// out cycles by construction.
pub fn parse_plan_opts(plan_id: &str, text: &str, sequential: bool) -> Result<Vec<NewTask>> {
    let (tasks, errors) = parse_lines(plan_id, text, sequential);
    if errors.is_empty() {
        Ok(tasks)
    } else {
        Err(PlanError::Parse(errors.join("; ")))
    }
}

fn parse_lines(plan_id: &str, text: &str, sequential: bool) -> (Vec<NewTask>, Vec<String>) {
    let mut errors = Vec::new();
    let id = |n: usize| format!("{plan_id}-{n:03}");
    let tasks = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .enumerate()
        .map(|(i, line)| {
            let mut parts: Vec<&str> = line.split('|').map(str::trim).collect();
            let mut refs = Vec::new();
            if parts.len() >= 3 {
                if let Some(list) = parts[parts.len() - 1].strip_prefix("after:") {
                    refs = list
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect();
                    parts.pop();
                }
            }
            // `@name` before the title picks the agent the task runs as.
            let mut agent = zerith_core::agents::DEFAULT.to_string();
            if let Some(rest) = parts[0].strip_prefix('@') {
                let (name, title) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                agent = name.to_string();
                parts[0] = title.trim();
            }
            let (title, prompt) = match parts.len() {
                1 => (parts[0].to_string(), parts[0].to_string()),
                _ => (parts[0].to_string(), parts[1..].join(" | ")),
            };
            let mut depends_on = Vec::new();
            if sequential && i > 0 {
                depends_on.push(id(i - 1));
            }
            for r in refs {
                match r.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= i => {
                        let d = id(n - 1);
                        if !depends_on.contains(&d) {
                            depends_on.push(d);
                        }
                    }
                    _ => errors.push(format!(
                        "linha {}: `after: {r}` precisa apontar para uma tarefa anterior",
                        i + 1
                    )),
                }
            }
            NewTask {
                title,
                prompt,
                agent,
                depends_on,
            }
        })
        .collect();
    (tasks, errors)
}

/// Runs a plan that is already in the store (no cancel, no budget).
pub fn run_plan(
    db: &Path,
    plan_id: &str,
    model: &str,
    concurrency: usize,
    scope: Option<Scope>,
    on_task: impl Fn(&Task, TaskState) + Send + Sync + 'static,
) -> Result<PlanReport> {
    let opts = PlanOptions {
        concurrency,
        ..PlanOptions::default()
    };
    run_plan_opts(db, plan_id, model, scope, opts, on_task)
}

/// Runs a plan with a cancel token and a per-task budget, one model turn per
/// task.
pub fn run_plan_opts(
    db: &Path,
    plan_id: &str,
    model: &str,
    scope: Option<Scope>,
    opts: PlanOptions,
    on_task: impl Fn(&Task, TaskState) + Send + Sync + 'static,
) -> Result<PlanReport> {
    let db_buf = db.to_path_buf();
    let model = model.to_string();
    let runner = move |task: &Task, ctx: &TaskCtx| {
        ctx.step()?;
        run_one(&db_buf, &model, scope.as_ref(), task, &ctx.cancel).map_err(|e| {
            if ctx.timed_out() {
                "tempo esgotado".to_string()
            } else {
                e.to_string()
            }
        })
    };
    run_plan_with(db, plan_id, opts, runner, on_task)
}

/// The scheduler, with the work itself injected. `runner` is called on a
/// worker thread; a panic inside it is caught and reported as a failure.
pub fn run_plan_with(
    db: &Path,
    plan_id: &str,
    opts: PlanOptions,
    runner: impl Fn(&Task, &TaskCtx) -> std::result::Result<TaskOutcome, String> + Send + Sync + 'static,
    on_task: impl Fn(&Task, TaskState) + Send + Sync + 'static,
) -> Result<PlanReport> {
    let concurrency = opts.concurrency.clamp(1, 8);
    let store = Store::open(db)?;
    if store.tasks(plan_id)?.is_empty() {
        return Err(PlanError::Empty(plan_id.to_string()));
    }
    // A previous process may have died holding a task. Put it back before
    // planning, or the plan stalls forever on a row nobody owns.
    store.requeue_running(plan_id)?;

    let on_task = Arc::new(on_task);
    let runner = Arc::new(runner);
    let (work_tx, work_rx) = mpsc::channel::<Task>();
    let work_rx = Arc::new(Mutex::new(work_rx));
    type Done = (Task, std::result::Result<TaskOutcome, String>);
    let (done_tx, done_rx) = mpsc::channel::<Done>();

    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let work_rx = work_rx.clone();
        let done_tx = done_tx.clone();
        let runner = runner.clone();
        let on_task = on_task.clone();
        let plan_cancel = opts.cancel.clone();
        let budget = opts.budget;
        workers.push(std::thread::spawn(move || loop {
            let task = {
                let Ok(guard) = work_rx.lock() else { return };
                match guard.recv() {
                    Ok(t) => t,
                    Err(_) => return,
                }
            };
            on_task(&task, TaskState::Running);
            let ctx = TaskCtx::new(Cancel::default(), budget);
            let watchdog = watch(plan_cancel.clone(), ctx.cancel.clone(), budget.max_time);
            let out =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner(&task, &ctx)))
                    .unwrap_or_else(|_| Err("o worker entrou em pânico".to_string()));
            drop(watchdog);
            if done_tx.send((task, out)).is_err() {
                return;
            }
        }));
    }
    drop(done_tx);

    // Coordinator: claim what is ready, skip what can never run, then block
    // until a worker answers. No polling: every dispatched task produces
    // exactly one message on `done_rx`.
    let mut running = 0usize;
    loop {
        let current = store.tasks(plan_id)?;
        let states: HashMap<&str, TaskState> =
            current.iter().map(|t| (t.id.as_str(), t.state)).collect();
        let cancelled = opts.cancel.is_cancelled();
        for t in current.iter().filter(|t| t.state == TaskState::Pending) {
            let dep_failed = t.depends_on.iter().any(|d| {
                matches!(
                    states.get(d.as_str()),
                    Some(TaskState::Failed | TaskState::Skipped)
                )
            });
            if cancelled || dep_failed {
                let why = if cancelled {
                    "plano cancelado"
                } else {
                    "dependency failed"
                };
                store.finish_task(&t.id, TaskState::Skipped, None, Some(why), None)?;
                on_task(t, TaskState::Skipped);
                continue;
            }
            let ready = t
                .depends_on
                .iter()
                .all(|d| states.get(d.as_str()) == Some(&TaskState::Done));
            if ready && store.claim_task(&t.id)? {
                running += 1;
                if work_tx.send(t.clone()).is_err() {
                    // Every worker is gone; the task cannot run.
                    running -= 1;
                    fail(&store, &*on_task, t, "nenhum worker disponível")?;
                }
            }
        }

        if running == 0 {
            break;
        }
        match done_rx.recv() {
            Ok((task, out)) => {
                running -= 1;
                match out {
                    Ok(o) => {
                        store.finish_task(
                            &task.id,
                            TaskState::Done,
                            Some(&o.text),
                            None,
                            o.session_id.as_deref(),
                        )?;
                        on_task(&task, TaskState::Done);
                    }
                    Err(e) => fail(&store, &*on_task, &task, &e)?,
                }
            }
            // All workers exited without answering for what is in flight.
            Err(_) => {
                for t in store.tasks(plan_id)? {
                    if t.state == TaskState::Running {
                        fail(&store, &*on_task, &t, "o worker terminou sem responder")?;
                    }
                }
                break;
            }
        }
    }
    drop(work_tx);
    for w in workers {
        let _ = w.join();
    }

    // Nothing in flight and still pending: the dependencies point at tasks
    // that never finish (a missing id). Close them instead of leaving a plan
    // that `unfinished_plans` reports forever.
    for t in store.tasks(plan_id)? {
        if t.state == TaskState::Pending {
            store.finish_task(
                &t.id,
                TaskState::Skipped,
                None,
                Some("dependência inexistente"),
                None,
            )?;
            on_task(&t, TaskState::Skipped);
        }
    }

    let tasks = store.tasks(plan_id)?;
    let count = |s: TaskState| tasks.iter().filter(|t| t.state == s).count();
    Ok(PlanReport {
        plan_id: plan_id.to_string(),
        done: count(TaskState::Done),
        failed: count(TaskState::Failed),
        skipped: count(TaskState::Skipped),
        tasks,
    })
}

fn fail(
    store: &Store,
    on_task: &(impl Fn(&Task, TaskState) + ?Sized),
    t: &Task,
    error: &str,
) -> Result<()> {
    store.finish_task(&t.id, TaskState::Failed, None, Some(error), None)?;
    on_task(t, TaskState::Failed);
    Ok(())
}

/// Forwards the plan cancel and the time budget into one task's token. The
/// returned sender stops the watchdog when dropped.
fn watch(plan: Cancel, task: Cancel, max_time: Option<Duration>) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let deadline = max_time.map(|d| Instant::now() + d);
    std::thread::spawn(move || loop {
        // ponytail: 50 ms slices because `Cancel` has no wakeup; add a condvar
        // to `Cancel` if this ever matters.
        let slice = Duration::from_millis(50);
        let wait = deadline.map_or(slice, |d| {
            d.saturating_duration_since(Instant::now()).min(slice)
        });
        match rx.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if plan.is_cancelled() || deadline.is_some_and(|d| Instant::now() >= d) {
                    task.cancel();
                    return;
                }
            }
            _ => return,
        }
    });
    tx
}

fn run_one(
    db: &Path,
    model: &str,
    scope: Option<&Scope>,
    task: &Task,
    cancel: &Cancel,
) -> std::result::Result<TaskOutcome, RunError> {
    // One connection per task: SQLite connections do not cross threads.
    let store = Store::open(db)?;
    let session = store.create_session(&task.title, SessionSource::Native)?;
    let client = Client::new(zerith_providers::ProviderConfig::from_env())?;
    let memory = zerith_memory::SqliteMemory::open(db).ok();
    // The task's agent: its instructions join the prompt and its preferred
    // model wins over the plan's. An unknown name runs as the default agent.
    let paths = zerith_core::Paths::resolve();
    let cwd = std::env::current_dir().ok();
    let agent = zerith_core::agents::find(&paths, cwd.as_deref(), &task.agent);
    let model = agent
        .as_ref()
        .and_then(|a| a.model.as_deref())
        .unwrap_or(model);
    let r = crate::run_turn_in(
        &store,
        &client,
        crate::TurnContext {
            memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
            scope,
            agent: agent.as_ref(),
            ..crate::TurnContext::default()
        },
        &session.id,
        model,
        &task.prompt,
        cancel,
        |_| {},
    )?;
    Ok(TaskOutcome {
        text: r.text,
        session_id: Some(r.session_id),
    })
}

/// Where a plan's database lives, so a caller does not have to know the layout.
pub fn db_path(root: &Path) -> PathBuf {
    root.join("zerith.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn plain_lines_are_parallel() {
        let tasks = parse_plan("p", "# comentário\nalfa | faz alfa\n\nbeta | faz beta\n");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "alfa");
        assert_eq!(tasks[0].prompt, "faz alfa");
        assert!(tasks[1].depends_on.is_empty());
    }

    #[test]
    fn at_name_picks_the_agent() {
        let t = parse_plan(
            "p",
            "@revisor Revisar | olhe o diff
sem agente | faz",
        );
        assert_eq!(t[0].agent, "revisor");
        assert_eq!(t[0].title, "Revisar");
        assert_eq!(t[0].prompt, "olhe o diff");
        assert_eq!(t[1].agent, zerith_core::agents::DEFAULT);
    }

    #[test]
    fn after_refs_are_one_based_and_seq_chains() {
        let text = "a | pa\nb | pb\nc | pc | after: 1, 2";
        let t = parse_plan_opts("p", text, false).unwrap();
        assert_eq!(t[2].depends_on, vec!["p-000", "p-001"]);
        assert_eq!(t[2].prompt, "pc");
        let s = parse_plan_opts("p", text, true).unwrap();
        assert_eq!(s[1].depends_on, vec!["p-000"]);
        assert_eq!(s[2].depends_on, vec!["p-001", "p-000"]);
    }

    #[test]
    fn a_forward_or_self_ref_is_an_error() {
        assert!(parse_plan_opts("p", "a | pa | after: 1", false).is_err());
        assert!(parse_plan_opts("p", "a | pa\nb | pb | after: 3", false).is_err());
        // The lenient form drops it.
        assert!(parse_plan("p", "a | pa | after: 1")[0]
            .depends_on
            .is_empty());
    }

    #[test]
    fn a_line_without_a_bar_is_its_own_prompt() {
        let tasks = parse_plan("p", "só o título");
        assert_eq!(tasks[0].title, "só o título");
        assert_eq!(tasks[0].prompt, "só o título");
    }

    fn setup(text: &str) -> (tempfile::TempDir, PathBuf) {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::tempdir_in(scratch).unwrap();
        let db = dir.path().join("zerith.db");
        let store = Store::open(&db).unwrap();
        store
            .create_plan("p", &parse_plan_opts("p", text, false).unwrap())
            .unwrap();
        (dir, db)
    }

    fn ok(t: &Task) -> std::result::Result<TaskOutcome, String> {
        Ok(TaskOutcome {
            text: format!("feito {}", t.title),
            session_id: None,
        })
    }

    #[test]
    fn independent_tasks_run_in_parallel() {
        let (_d, db) = setup("a | x\nb | y\nc | z");
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (l, p) = (live.clone(), peak.clone());
        let opts = PlanOptions {
            concurrency: 3,
            ..PlanOptions::default()
        };
        let r = run_plan_with(
            &db,
            "p",
            opts,
            move |t, _| {
                let n = l.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                l.fetch_sub(1, Ordering::SeqCst);
                ok(t)
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(r.done, 3);
        assert!(peak.load(Ordering::SeqCst) >= 2, "tasks ran one at a time");
    }

    #[test]
    fn a_panicking_worker_marks_the_task_failed_and_the_plan_ends() {
        let (_d, db) = setup("a | x\nb | y | after: 1\nc | z");
        let r = run_plan_with(
            &db,
            "p",
            PlanOptions::default(),
            |t, _| {
                if t.title == "a" {
                    panic!("boom");
                }
                ok(t)
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!((r.done, r.failed, r.skipped), (1, 1, 1));
        let a = r.tasks.iter().find(|t| t.title == "a").unwrap();
        assert!(a.error.as_deref().unwrap().contains("pânico"));
    }

    #[test]
    fn an_error_is_recorded_and_dependents_skipped() {
        let (_d, db) = setup("a | x\nb | y | after: 1");
        let r = run_plan_with(
            &db,
            "p",
            PlanOptions::default(),
            |_, _| Err("falhou".into()),
            |_, _| {},
        )
        .unwrap();
        assert_eq!((r.failed, r.skipped), (1, 1));
        assert_eq!(r.tasks[0].error.as_deref(), Some("falhou"));
    }

    #[test]
    fn a_missing_dependency_does_not_hang() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::tempdir_in(scratch).unwrap();
        let db = dir.path().join("zerith.db");
        Store::open(&db)
            .unwrap()
            .create_plan(
                "p",
                &[NewTask {
                    title: "a".into(),
                    prompt: "x".into(),
                    agent: "default".into(),
                    depends_on: vec!["nope".into()],
                }],
            )
            .unwrap();
        let r = run_plan_with(&db, "p", PlanOptions::default(), |t, _| ok(t), |_, _| {}).unwrap();
        assert_eq!(r.skipped, 1);
    }

    #[test]
    fn cancel_stops_running_tasks_and_skips_pending_ones() {
        let (_d, db) = setup("a | x\nb | y | after: 1");
        let opts = PlanOptions {
            concurrency: 1,
            ..PlanOptions::default()
        };
        let cancel = opts.cancel.clone();
        let start = Instant::now();
        let r = run_plan_with(
            &db,
            "p",
            opts,
            move |_, ctx| {
                cancel.cancel();
                // A cooperative runner: wait until the token is pulled.
                while !ctx.cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                ctx.step().map(|_| unreachable!())
            },
            |_, _| {},
        )
        .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!((r.failed, r.skipped), (1, 1));
        assert_eq!(r.tasks[0].error.as_deref(), Some("cancelado"));
    }

    #[test]
    fn time_and_step_budgets_are_enforced() {
        let (_d, db) = setup("a | x\nb | y");
        let opts = PlanOptions {
            concurrency: 2,
            budget: TaskBudget {
                max_steps: Some(1),
                max_time: Some(Duration::from_millis(100)),
            },
            ..PlanOptions::default()
        };
        let r = run_plan_with(
            &db,
            "p",
            opts,
            |t, ctx| {
                ctx.step()?;
                if t.title == "a" {
                    while !ctx.cancel.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    return ctx.step().map(|_| unreachable!());
                }
                ctx.step().map(|_| unreachable!())
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(r.failed, 2);
        let err = |title: &str| {
            r.tasks
                .iter()
                .find(|t| t.title == title)
                .unwrap()
                .error
                .clone()
                .unwrap()
        };
        assert_eq!(err("a"), "tempo esgotado");
        assert!(err("b").contains("passo"));
    }
}
