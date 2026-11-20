//! Background shell: `shell{run_in_background:true}` starts a job and returns
//! its id; `shell_output` reads what it printed since the last look;
//! `shell_kill` stops it. Jobs belong to the registry's [`crate::Host`] and
//! die with it, so a closed session leaves no stray process. The window lists
//! and stops them through [`background_jobs`] / [`stop_background_job`].
//!
//! ponytail: output is kept in memory (capped at 1 MB, oldest dropped); a job
//! that prints gigabytes loses its head, not the process. Spill to a file if
//! that ever matters.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Instant;

use serde_json::{json, Value};
use zerith_security::jail::{Jail, Jailed};
use zerith_security::Policy;

use crate::builtin::shell_command;
use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

/// Jobs alive at once, per registry.
pub const MAX_JOBS: usize = 4;
/// Bytes of output kept per job.
const MAX_OUTPUT: usize = 1 << 20;

static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Output {
    text: String,
    /// Bytes of `text` already handed to the model.
    seen: usize,
    truncated: bool,
}

struct Job {
    jailed: Jailed,
    out: Arc<Mutex<Output>>,
    readers: Vec<JoinHandle<()>>,
    command: String,
    exit: Option<String>,
    /// The session whose turn started it.
    session: Option<String>,
    started: Instant,
}

type Table = Mutex<Vec<(String, Job)>>;

/// Every job table alive in this process, so the window can list and stop the
/// jobs of any turn. Weak: a table still dies with its host.
static TABLES: Mutex<Vec<Weak<Table>>> = Mutex::new(Vec::new());

/// Jobs outlive the turn that started them: the table is kept alive here, so a
/// GUI script (`python janela.py`) does not lose its window when the answer
/// lands. `kill_all` stops them when the app closes.
static KEEP: Mutex<Vec<Arc<Table>>> = Mutex::new(Vec::new());

pub struct Jobs {
    jobs: Arc<Table>,
}

impl Default for Jobs {
    fn default() -> Self {
        let jobs = Arc::new(Table::default());
        if let Ok(mut all) = TABLES.lock() {
            all.retain(|t| t.strong_count() > 0);
            all.push(Arc::downgrade(&jobs));
        }
        Self { jobs }
    }
}

/// A running background job, as the window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJob {
    pub id: String,
    pub command: String,
    /// The session whose turn started it (a subagent's own session for a child).
    pub session: Option<String>,
    /// Seconds since it started.
    pub secs: u64,
}

fn tables() -> Vec<Arc<Table>> {
    TABLES
        .lock()
        .map(|all| all.iter().filter_map(Weak::upgrade).collect())
        .unwrap_or_default()
}

/// Jobs still running, from every live registry of this process.
pub fn background_jobs() -> Vec<BackgroundJob> {
    let mut out = Vec::new();
    for table in tables() {
        let mut to_join = Vec::new();
        if let Ok(mut jobs) = table.lock() {
            for (id, j) in jobs.iter_mut() {
                to_join.extend(refresh(j));
                if j.exit.is_none() {
                    out.push(BackgroundJob {
                        id: id.clone(),
                        command: j.command.clone(),
                        session: j.session.clone(),
                        secs: j.started.elapsed().as_secs(),
                    });
                }
            }
        }
        for r in to_join {
            let _ = r.join();
        }
    }
    out
}

/// Stops job `id` wherever it runs (ids are unique per process): the window's
/// Stop button. The model sees it as ended at its next `shell_output`.
/// Stops every job of the process: the window is closing.
pub fn kill_all_jobs() {
    for table in tables() {
        let ids: Vec<String> = table
            .lock()
            .map(|jobs| jobs.iter().map(|(i, _)| i.clone()).collect())
            .unwrap_or_default();
        for id in ids {
            let _ = kill_in(&table, &id);
        }
    }
    if let Ok(mut keep) = KEEP.lock() {
        keep.clear();
    }
}

pub fn stop_background_job(id: &str) -> Result<String> {
    let table = tables()
        .into_iter()
        .find(|t| t.lock().is_ok_and(|jobs| jobs.iter().any(|(i, _)| i == id)))
        .ok_or_else(|| ToolError::Failed(format!("job desconhecido: {id}")))?;
    kill_in(&table, id)
}

fn poisoned() -> ToolError {
    ToolError::Failed("registro de jobs corrompido".into())
}

impl Jobs {
    pub fn start(
        &self,
        command: &str,
        cwd: &Path,
        policy: &Policy,
        session: Option<String>,
    ) -> Result<String> {
        let mut to_join = Vec::new();
        let id = {
            let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
            for (_, j) in jobs.iter_mut() {
                to_join.extend(refresh(j));
            }
            let alive = jobs.iter().filter(|(_, j)| j.exit.is_none()).count();
            if alive >= MAX_JOBS {
                return Err(ToolError::Failed(format!(
                    "já há {MAX_JOBS} jobs em segundo plano; encerre um com shell_kill"
                )));
            }
            // Finished jobs whose output was read can go; keep the table small.
            jobs.retain(|(_, j)| {
                j.exit.is_none() || j.out.lock().is_ok_and(|o| o.seen < o.text.len())
            });
            let jail = Jail::for_policy(policy);
            let mut cmd = shell_command(command);
            cmd.current_dir(cwd);
            let mut cmd = jail.wrap(cmd);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut jailed = jail.spawn_wrapped(cmd)?;
            let out = Arc::new(Mutex::new(Output::default()));
            let readers = vec![
                pump(jailed.child.stdout.take().map(|s| Box::new(s) as _), &out),
                pump(jailed.child.stderr.take().map(|s| Box::new(s) as _), &out),
            ];
            let id = format!("job-{}", NEXT.fetch_add(1, Ordering::Relaxed));
            jobs.push((
                id.clone(),
                Job {
                    jailed,
                    out,
                    readers,
                    command: command.to_string(),
                    exit: None,
                    session,
                    started: Instant::now(),
                },
            ));
            id
        };
        if let Ok(mut keep) = KEEP.lock() {
            if !keep.iter().any(|t| Arc::ptr_eq(t, &self.jobs)) {
                keep.push(self.jobs.clone());
            }
        }
        for r in to_join {
            let _ = r.join();
        }
        Ok(id)
    }

    /// New output since the last call, plus the state.
    pub fn read(&self, id: &str) -> Result<String> {
        // Reap and release the lock before joining reader threads so a
        // grandchild holding a pipe never deadlocks other job operations.
        let to_join = {
            let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
            let j = find(&mut jobs, id)?;
            refresh(j)
        };
        for r in to_join {
            let _ = r.join();
        }
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let j = find(&mut jobs, id)?;
        let mut o = j.out.lock().map_err(|_| poisoned())?;
        let mut cut = o.seen.min(o.text.len());
        while !o.text.is_char_boundary(cut) {
            cut += 1;
        }
        let fresh = o.text[cut..].to_string();
        o.seen = o.text.len();
        let mut text = match &j.exit {
            Some(code) => format!("{id}: terminou ({code})\n"),
            None => format!("{id}: em execução\n"),
        };
        if fresh.is_empty() {
            text.push_str("[sem saída nova]\n");
        } else {
            text.push_str(&fresh);
            if !fresh.ends_with('\n') {
                text.push('\n');
            }
        }
        if o.truncated {
            text.push_str("[saída antiga descartada: limite de 1 MB]\n");
        }
        Ok(text)
    }

    pub fn kill(&self, id: &str) -> Result<String> {
        kill_in(&self.jobs, id)
    }
}

fn kill_in(table: &Table, id: &str) -> Result<String> {
    let (msg, to_join) = {
        let mut jobs = table.lock().map_err(|_| poisoned())?;
        let j = find(&mut jobs, id)?;
        if j.exit.is_some() {
            return Ok(format!("{id} já tinha terminado"));
        }
        j.jailed.kill_tree();
        let readers = std::mem::take(&mut j.readers);
        j.exit = Some("encerrado".into());
        (format!("{id} encerrado: {}", j.command), readers)
    };
    for r in to_join {
        let _ = r.join();
    }
    Ok(msg)
}

fn find<'a>(jobs: &'a mut [(String, Job)], id: &str) -> Result<&'a mut Job> {
    jobs.iter_mut()
        .find(|(i, _)| i == id)
        .map(|(_, j)| j)
        .ok_or_else(|| ToolError::Failed(format!("job desconhecido: {id}")))
}

impl Drop for Jobs {
    fn drop(&mut self) {
        // A background job is adopted by `KEEP`: it belongs to the app, not to
        // the turn that started it, and stops with `shell_kill` (or when the
        // app closes). Everything else dies with its host, as before.
        if KEEP
            .lock()
            .is_ok_and(|keep| keep.iter().any(|t| Arc::ptr_eq(t, &self.jobs)))
        {
            return;
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            for (_, j) in jobs.iter_mut() {
                if j.exit.is_none() {
                    j.jailed.kill_tree();
                }
            }
        }
    }
}

/// Notes the exit once and returns the reader handles for the caller to join
/// outside the `Jobs` mutex.
fn refresh(j: &mut Job) -> Vec<JoinHandle<()>> {
    if j.exit.is_some() {
        return Vec::new();
    }
    if let Ok(Some(status)) = j.jailed.child.try_wait() {
        j.exit = Some(match status.code() {
            Some(c) => format!("exit {c}"),
            None => "sinal".into(),
        });
        return std::mem::take(&mut j.readers);
    }
    Vec::new()
}

fn pump(r: Option<Box<dyn Read + Send>>, out: &Arc<Mutex<Output>>) -> JoinHandle<()> {
    let out = out.clone();
    std::thread::spawn(move || {
        let Some(mut r) = r else { return };
        // Fixed-size chunks: a process printing gigabytes without newlines
        // cannot grow a single line buffer unboundedly in RAM.
        let mut buf = [0u8; 8192];
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let s = String::from_utf8_lossy(&buf[..n]);
            let Ok(mut o) = out.lock() else { return };
            o.text.push_str(&s);
            if o.text.len() > MAX_OUTPUT {
                let mut cut = o.text.len() - MAX_OUTPUT;
                while !o.text.is_char_boundary(cut) {
                    cut += 1;
                }
                o.text.drain(..cut);
                o.seen = o.seen.saturating_sub(cut);
                o.truncated = true;
            }
        }
    })
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make(
            "shell_output",
            "Lê a saída nova de um job iniciado com shell run_in_background e diz se ele terminou.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "estado do job e as linhas novas desde a última leitura",
            Permission::Read,
            10,
            false,
            |a, _, e| e.host.jobs().read(str_arg(a, "id")?),
        ),
        make(
            "shell_kill",
            "Encerra um job em segundo plano e os processos que ele iniciou.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "confirmação",
            Permission::Shell,
            10,
            false,
            |a, _, e| e.host.jobs().kill(str_arg(a, "id")?),
        ),
    ]
}

/// Called by `shell` itself for `run_in_background`.
pub(crate) fn start(
    args: &Value,
    cwd: &Path,
    policy: &Policy,
    host: &crate::Host,
) -> Result<String> {
    let id = host
        .jobs()
        .start(str_arg(args, "command")?, cwd, policy, host.session())?;
    Ok(format!(
        "{id} iniciado em segundo plano; use shell_output para ler a saída e shell_kill para encerrar"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_security::{preset, Mode};

    #[test]
    fn the_window_lists_and_stops_the_jobs_of_any_registry() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(root).unwrap();
        let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, dir.path());
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul"
        } else {
            "sleep 30"
        };
        let jobs = Jobs::default();
        let a = jobs
            .start(long, dir.path(), &policy, Some("s-ui".into()))
            .unwrap();
        let b = jobs.start(long, dir.path(), &policy, None).unwrap();
        let listed = |id: &str| background_jobs().into_iter().find(|j| j.id == id);

        let seen = listed(&a).expect("a running job is listed");
        assert_eq!(seen.command, long);
        assert_eq!(seen.session.as_deref(), Some("s-ui"));
        assert!(stop_background_job(&a).unwrap().contains("encerrado"));
        assert!(listed(&a).is_none(), "a stopped job leaves the list");
        assert!(jobs.read(&a).unwrap().contains("terminou (encerrado)"));
        assert!(stop_background_job("job-0").is_err());

        assert!(listed(&b).is_some());
        drop(jobs);
        assert!(
            listed(&b).is_some(),
            "a background job outlives the turn that started it"
        );
        kill_all_jobs();
        assert!(listed(&b).is_none(), "closing the app stops the jobs");
    }
}
