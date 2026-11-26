//! Background shell: `shell{run_in_background:true}` starts a job and returns
//! its id; `process` reads what it printed since the last look (`output`),
//! stops it (`kill`) or lists the jobs (`list`), and when the job ends on its
//! own the turn gets one notice with the exit and the last lines, through the
//! same channel as `monitor`. `shell_output` / `shell_kill` stay as unlisted
//! aliases (workflows, agent `tools:` and rules may name them). Jobs belong to the registry's [`crate::Host`] and
//! die with it, so a closed session leaves no stray process. The window lists
//! and stops them through [`background_jobs`] / [`stop_background_job`].
//!
//! ponytail: output is kept in memory (capped at 1 MB, oldest dropped); a job
//! that prints gigabytes loses its head, not the process. Spill to a file if
//! that ever matters.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_security::jail::{Jail, Jailed};
use zerith_security::Policy;

use crate::builtin::shell_command;
use crate::fntool::str_arg;
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

struct Reader {
    done: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct DoneOnDrop(Arc<AtomicBool>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn join_readers_with_grace(readers: Vec<Reader>, grace: Duration) {
    if readers.is_empty() {
        return;
    }
    let deadline = Instant::now() + grace;
    for r in readers {
        while !r.done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if r.done.load(Ordering::Acquire) {
            let _ = r.handle.join();
        }
    }
}

struct Job {
    jailed: Jailed,
    out: Arc<Mutex<Output>>,
    readers: Vec<Reader>,
    command: String,
    exit: Option<String>,
    /// The session whose turn started it.
    session: Option<String>,
    started: Instant,
    /// Set for a `monitor` job: its new lines are pushed to the turn.
    watch: Option<Watch>,
    /// A plain job's end needs no notice: already told, read through
    /// `process output`, or stopped on purpose.
    notified: bool,
}

/// What a `monitor` job adds to a background job: who is told, until when,
/// and how much it has said (flood guard).
struct Watch {
    description: String,
    deadline: Instant,
    /// Bytes of output already turned into notices.
    sent: usize,
    /// Consecutive collections cut at the per-notice cap.
    strikes: u8,
    /// The final notice (exit, deadline, stop or flood) was delivered.
    closed: bool,
}

/// Lines of output one monitor notice carries at most.
pub const MONITOR_LINES: usize = 20;
/// Characters one monitor notice carries at most.
pub const MONITOR_CHARS: usize = 2000;
/// Consecutive collections that overflow before the monitor is stopped.
const MONITOR_STRIKES: u8 = 3;
/// Longest a monitor may run.
pub const MONITOR_MAX_SECS: u64 = 1800;
/// Lines of output the end-of-job notice carries.
pub const DONE_LINES: usize = 20;

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
    let mut to_join = Vec::new();
    for table in tables() {
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
    }
    join_readers_with_grace(to_join, Duration::from_millis(150));
    out
}

/// The last `lines` lines job `id` printed, for the window. Read-only: what
/// the model has not seen yet through `process output` stays unseen.
pub fn background_job_tail(id: &str, lines: usize) -> Option<String> {
    for table in tables() {
        let jobs = table.lock().ok()?;
        if let Some((_, j)) = jobs.iter().find(|(i, _)| i == id) {
            let out = j.out.lock().ok()?;
            let all: Vec<&str> = out.text.lines().collect();
            return Some(all[all.len().saturating_sub(lines)..].join("\n"));
        }
    }
    None
}

/// Stops job `id` wherever it runs (ids are unique per process): the window's
/// Stop button. The model sees it as ended at its next `process output`.
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

/// Sessions with a monitor that still has something to say, from every live
/// registry: the host wakes those conversations when their turn has ended.
pub fn sessions_with_monitor_news() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for table in tables() {
        let Ok(mut jobs) = table.lock() else { continue };
        for (_, j) in jobs.iter_mut() {
            if has_news(j) {
                if let Some(s) = &j.session {
                    if !out.contains(s) {
                        out.push(s.clone());
                    }
                }
            }
        }
    }
    out
}

/// The monitor notices of `session` across every live registry (the
/// registry of the turn that started them may be gone): what the host hands
/// to the new turn it starts for them.
pub fn take_monitor_notices(session: &str) -> Vec<String> {
    let mut out = Vec::new();
    for table in tables() {
        let owns = table.lock().is_ok_and(|jobs| {
            jobs.iter()
                .any(|(_, j)| j.session.as_deref() == Some(session))
        });
        if owns {
            out.extend(Jobs { jobs: table }.monitor_notices());
        }
    }
    out
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
        self.spawn(command, cwd, policy, session, None)
    }

    /// Starts a `monitor` job: a background job whose new output lines the
    /// turn receives on its own ([`Jobs::monitor_notices`]) until the command
    /// exits, `secs` pass (capped at [`MONITOR_MAX_SECS`]) or it is stopped.
    pub fn start_monitor(
        &self,
        command: &str,
        description: &str,
        secs: u64,
        cwd: &Path,
        policy: &Policy,
        session: Option<String>,
    ) -> Result<String> {
        let watch = Watch {
            description: description.to_string(),
            deadline: Instant::now() + Duration::from_secs(secs.clamp(1, MONITOR_MAX_SECS)),
            sent: 0,
            strikes: 0,
            closed: false,
        };
        self.spawn(command, cwd, policy, session, Some(watch))
    }

    fn spawn(
        &self,
        command: &str,
        cwd: &Path,
        policy: &Policy,
        session: Option<String>,
        watch: Option<Watch>,
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
                    "já há {MAX_JOBS} jobs em segundo plano; encerre um com process action=kill"
                )));
            }
            // Finished jobs whose output was read (or, for a monitor, whose
            // final notice went out) can go; keep the table small.
            jobs.retain(|(_, j)| {
                j.exit.is_none()
                    || match &j.watch {
                        Some(w) => !w.closed,
                        None => !j.notified || j.out.lock().is_ok_and(|o| o.seen < o.text.len()),
                    }
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
                    watch,
                    notified: false,
                },
            ));
            id
        };
        if let Ok(mut keep) = KEEP.lock() {
            if !keep.iter().any(|t| Arc::ptr_eq(t, &self.jobs)) {
                keep.push(self.jobs.clone());
            }
        }
        join_readers_with_grace(to_join, Duration::from_millis(150));
        Ok(id)
    }

    /// New output since the last call, plus the state.
    pub fn read(&self, id: &str) -> Result<String> {
        // Reap and release the lock before waiting briefly on reader threads so a
        // grandchild holding a pipe never deadlocks or hangs other job operations.
        let to_join = {
            let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
            let j = find(&mut jobs, id)?;
            refresh(j)
        };
        join_readers_with_grace(to_join, Duration::from_millis(150));
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let j = find(&mut jobs, id)?;
        // Read after it ended: the model knows, no notice follows.
        j.notified |= j.exit.is_some();
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

    /// This registry's jobs, running or not yet collected, one per line.
    pub fn list(&self) -> Result<String> {
        let mut to_join = Vec::new();
        let text = {
            let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
            let mut lines = Vec::new();
            for (id, j) in jobs.iter_mut() {
                to_join.extend(refresh(j));
                let state = match &j.exit {
                    Some(code) => format!("terminou ({code})"),
                    None => format!("em execução há {}s", j.started.elapsed().as_secs()),
                };
                let kind = if j.watch.is_some() { " [monitor]" } else { "" };
                lines.push(format!("{id}: {state}{kind} — {}", j.command));
            }
            if lines.is_empty() {
                "nenhum job em segundo plano".to_string()
            } else {
                lines.join("\n")
            }
        };
        join_readers_with_grace(to_join, Duration::from_millis(150));
        Ok(text)
    }

    /// Whether a monitor printed a whole new line, exited, or hit its
    /// deadline since its last notice, or a plain job ended unannounced,
    /// without consuming anything.
    pub fn monitors_have_news(&self) -> bool {
        self.jobs
            .lock()
            .is_ok_and(|mut jobs| jobs.iter_mut().any(|(_, j)| has_news(j)))
    }

    /// Stops every monitor of this registry and drops what it had to say:
    /// the person stopped the turn, and a notice must not revive it (Hermes
    /// `kill_started_since(..., consume_output=True)`, same reason).
    pub fn stop_monitors(&self) {
        let ids: Vec<String> = self
            .jobs
            .lock()
            .map(|jobs| {
                jobs.iter()
                    .filter(|(_, j)| j.watch.is_some())
                    .map(|(i, _)| i.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            let _ = kill_in(&self.jobs, &id);
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            for (_, j) in jobs.iter_mut() {
                if let Some(w) = j.watch.as_mut() {
                    w.closed = true;
                }
                // A plain job keeps running (a window, a server), but its
                // end must not start a turn in a conversation just stopped.
                j.notified = true;
            }
        }
    }

    /// Whether a monitor of this registry is still to report something.
    pub fn monitors_pending(&self) -> bool {
        self.jobs.lock().is_ok_and(|jobs| {
            jobs.iter()
                .any(|(_, j)| j.watch.as_ref().is_some_and(|w| !w.closed))
        })
    }

    /// One notice per monitor with something new: the complete lines it
    /// printed since the last call (at most [`MONITOR_LINES`] /
    /// [`MONITOR_CHARS`], the rest counted), and a final notice when it
    /// exits, hits its deadline, is stopped, or keeps flooding.
    pub fn monitor_notices(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut to_join = Vec::new();
        let Ok(mut jobs) = self.jobs.lock() else {
            return out;
        };
        for (id, j) in jobs.iter_mut() {
            if j.watch.is_none() {
                if !j.notified {
                    to_join.extend(refresh(j));
                    if j.exit.is_some() {
                        j.notified = true;
                        out.extend(done_notice(id, j));
                    }
                }
                continue;
            }
            if j.watch.as_ref().is_some_and(|w| w.closed) {
                continue;
            }
            to_join.extend(refresh(j));
            let expired = j.exit.is_none()
                && j.watch
                    .as_ref()
                    .is_some_and(|w| Instant::now() >= w.deadline);
            if expired {
                j.jailed.kill_tree();
                to_join.extend(std::mem::take(&mut j.readers));
                j.exit = Some("prazo do monitor esgotado".into());
            }
            let finished = j.exit.clone();
            let Ok(o) = j.out.lock() else { continue };
            let w = j.watch.as_mut().expect("checked above");
            // Only whole lines while it runs; the tail too once it ended.
            let start = w.sent.min(o.text.len());
            let mut end = if finished.is_some() {
                o.text.len()
            } else {
                o.text[start..].rfind('\n').map_or(start, |i| start + i + 1)
            };
            while !o.text.is_char_boundary(end) {
                end += 1;
            }
            let fresh = &o.text[start..end];
            w.sent = end;
            let lines: Vec<&str> = fresh.lines().filter(|l| !l.trim().is_empty()).collect();
            let mut body = String::new();
            let mut shown = 0;
            for l in &lines {
                if shown == MONITOR_LINES || body.len() + l.len() > MONITOR_CHARS {
                    break;
                }
                body.push_str(l);
                body.push('\n');
                shown += 1;
            }
            let cut = lines.len() - shown;
            if cut > 0 {
                body.push_str(&format!("[+{cut} linhas omitidas]\n"));
                w.strikes += 1;
            } else if !lines.is_empty() {
                w.strikes = 0;
            }
            let flooding = finished.is_none() && w.strikes >= MONITOR_STRIKES;
            let head = format!("[Monitor {id}: {}]", w.description);
            if !body.is_empty() {
                out.push(format!("{head}\n{body}").trim_end().to_string());
            }
            drop(o);
            if flooding {
                j.jailed.kill_tree();
                to_join.extend(std::mem::take(&mut j.readers));
                j.exit = Some("parado: saída demais".into());
                out.push(format!(
                    "{head} parado: imprimiu mais de {MONITOR_LINES} linhas em {MONITOR_STRIKES} leituras seguidas. Filtre a saída (grep --line-buffered) e inicie de novo."
                ));
            } else if let Some(why) = finished {
                out.push(format!("{head} terminou ({why})."));
            }
            if let Some(w) = j.watch.as_mut() {
                w.closed = j.exit.is_some();
            }
        }
        drop(jobs);
        join_readers_with_grace(to_join, Duration::from_millis(150));
        out
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
        // Stopped on purpose (the model, the window, the app closing).
        j.notified = true;
        (format!("{id} encerrado: {}", j.command), readers)
    };
    join_readers_with_grace(to_join, Duration::from_secs(1));
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
        // the turn that started it, and stops with `process kill` (or when the
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

/// Whether `j` has something to tell the turn: a monitor with a new whole
/// line, an exit or a passed deadline; a plain job that ended unannounced.
fn has_news(j: &mut Job) -> bool {
    let exited = j.exit.is_some() || matches!(j.jailed.child.try_wait(), Ok(Some(_)));
    match j.watch.as_ref() {
        None => !j.notified && exited,
        Some(w) if w.closed => false,
        Some(w) => {
            let (sent, deadline) = (w.sent, w.deadline);
            let line = j.out.lock().is_ok_and(|o| {
                o.text.len() > sent
                    && o.text.is_char_boundary(sent)
                    && o.text[sent..].contains('\n')
            });
            exited || line || Instant::now() >= deadline
        }
    }
}

/// The one notice of a plain background job that ended on its own: exit and
/// the last [`DONE_LINES`] lines (at most [`MONITOR_CHARS`]). Marks the
/// output as seen. After Hermes `terminal(background, notify)` (MIT,
/// tools/terminal_tool.py); no code copied.
fn done_notice(id: &str, j: &Job) -> Option<String> {
    let mut o = j.out.lock().ok()?;
    let lines: Vec<&str> = o.text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut tail: Vec<&str> = Vec::new();
    let mut size = 0;
    for l in lines.iter().rev().take(DONE_LINES) {
        if size + l.len() > MONITOR_CHARS {
            break;
        }
        size += l.len() + 1;
        tail.push(l);
    }
    tail.reverse();
    let mut cmd: String = j.command.chars().take(80).collect();
    if cmd.len() < j.command.len() {
        cmd.push('…');
    }
    let code = j.exit.as_deref().unwrap_or("?");
    let mut text = format!("[Job {id}: {cmd}] terminou ({code}).");
    if tail.is_empty() {
        text.push_str(" Sem saída.");
    } else {
        let omitted = lines.len() - tail.len();
        text.push_str(" Últimas linhas:");
        if omitted > 0 {
            text.push_str(&format!(" [+{omitted} linhas antes]"));
        }
        text.push('\n');
        text.push_str(&tail.join("\n"));
    }
    let end = o.text.len();
    o.seen = end;
    Some(text)
}

/// Notes the exit once and returns the reader handles for the caller to wait on
/// outside the `Jobs` mutex.
fn refresh(j: &mut Job) -> Vec<Reader> {
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

fn pump(r: Option<Box<dyn Read + Send>>, out: &Arc<Mutex<Output>>) -> Reader {
    let out = out.clone();
    let done = Arc::new(AtomicBool::new(false));
    let done_flag = done.clone();
    let handle = std::thread::spawn(move || {
        let _done = DoneOnDrop(done_flag);
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
    });
    Reader { done, handle }
}

struct ProcessTool {
    spec: crate::ToolSpec,
}

impl Tool for ProcessTool {
    fn spec(&self) -> &crate::ToolSpec {
        &self.spec
    }

    fn action(
        &self,
        _args: &Value,
        _ctx: &crate::CallCtx<'_>,
    ) -> Result<Option<zerith_security::Action>> {
        Ok(None)
    }

    fn run(&self, a: &Value, _ctx: &crate::CallCtx<'_>, e: &crate::Exec<'_>) -> Result<String> {
        match a["action"].as_str().unwrap_or("") {
            "output" => e.host.jobs().read(str_arg(a, "id")?),
            "kill" => e.host.jobs().kill(str_arg(a, "id")?),
            "list" => e.host.jobs().list(),
            other => Err(ToolError::BadArguments(format!(
                "action precisa ser output, kill ou list, não {other:?}"
            ))),
        }
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        matches!(args["action"].as_str(), Some("list" | "output"))
    }
}

/// `process` and its old names. Concept after Hermes `process` (MIT,
/// tools/process_tool.py) and Claude Code's background-shell tools.
pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ProcessTool {
            spec: crate::ToolSpec {
                name: "process".into(),
                description: "Jobs em segundo plano (shell run_in_background e monitor). action \"output\": a saída nova desde a última leitura e se terminou; \"kill\": encerra o job e o que ele iniciou; \"list\": os jobs desta conversa. Um job que termina sozinho já te avisa (código de saída e últimas linhas): não fique consultando.".into(),
                parameters: json!({"type": "object", "properties": {
                    "action": {"type": "string", "enum": ["output", "kill", "list"]},
                    "id": {"type": "string", "description": "id do job (job-N); não precisa para list"}
                }, "required": ["action"]}),
                returns: "estado e saída nova do job, confirmação, ou a lista de jobs",
                permission: Permission::Shell,
                timeout: Duration::from_secs(10),
                cancellable: true,
                local: true,
                source: "built-in".into(),
            },
        }),
        // The old names still run (workflows, agent `tools:`, saved rules and
        // conversations name them) but are not offered to the model.
        alias(
            "shell_output",
            Permission::Read,
            true,
            |a, _, e| e.host.jobs().read(str_arg(a, "id")?),
        ),
        alias(
            "shell_kill",
            Permission::Shell,
            false,
            |a, _, e| e.host.jobs().kill(str_arg(a, "id")?),
        ),
        monitor_tool(),
    ]
}

/// An unlisted old name of `process` (source [`crate::ALIAS_SOURCE`]).
fn alias(
    name: &str,
    permission: Permission,
    safe: bool,
    run: crate::fntool::RunFn,
) -> Box<dyn Tool> {
    Box::new(crate::fntool::FnTool {
        spec: crate::ToolSpec {
            name: name.into(),
            description: "Apelido antigo de process.".into(),
            parameters: json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            returns: "o mesmo que process",
            permission,
            timeout: Duration::from_secs(10),
            cancellable: true,
            local: true,
            source: format!("{}process", crate::ALIAS_SOURCE),
        },
        action: |_, _| Ok(None),
        run,
        safe,
    })
}

/// The `monitor` tool. Concept from Claude Code's Monitor (each output line
/// reaches the model on its own); batching and flood guard after Hermes
/// Agent's watch notifications (MIT) and Goose's shell streaming (Apache-2.0).
/// No code copied. Same permission as `shell`: the command goes through the
/// same `Action::Shell` check, so a rule that blocks `shell(x)` blocks this.
pub(crate) fn monitor_tool() -> Box<dyn Tool> {
    crate::fntool::make_with(
        "monitor",
        "Roda um comando em segundo plano e te avisa sozinho, a cada passo, das linhas novas que ele imprime, até ele terminar, o prazo acabar ou você parar com process action=kill. Use para acompanhar algo demorado (build, deploy, log, ping contínuo) sem ficar perguntando: continue trabalhando e reaja quando o aviso chegar. Cada linha de stdout é um evento: filtre só o que importa, incluindo as falhas (ex.: `tail -f app.log | grep --line-buffered -E \"ERROR|ready|Traceback\"`); um monitor que imprime demais é parado. Para esperar uma condição única, prefira um comando que termina quando ela acontece (`until curl -s x; do sleep 2; done`).",
        json!({"type": "object", "properties": {
            "command": {"type": "string", "description": "comando de shell; cada linha que ele imprime vira um aviso"},
            "description": {"type": "string", "description": "o que está sendo acompanhado, curto (aparece em cada aviso)"},
            "timeout_secs": {"type": "integer", "description": "prazo máximo; padrão 300, máximo 1800"}
        }, "required": ["command", "description"]}),
        "o id do monitor; as linhas chegam depois, sozinhas, como avisos [Monitor ...]",
        Permission::Shell,
        10,
        false,
        |a, _| Ok(Some(zerith_security::Action::Shell(str_arg(a, "command")?.to_string()))),
        |a, ctx, e| {
            let command = str_arg(a, "command")?;
            let description = str_arg(a, "description")?;
            let secs = a["timeout_secs"].as_u64().unwrap_or(300);
            e.host.before_shell(command, ctx.workspace);
            let id = e.host.jobs().start_monitor(
                command,
                description,
                secs,
                ctx.workspace,
                ctx.policy,
                e.host.session(),
            )?;
            Ok(format!(
                "{id} monitorando \"{description}\" (prazo {}s). As linhas novas chegam sozinhas como avisos [Monitor {id}: ...]; não fique consultando. Para parar: process action=kill id={id}.",
                secs.clamp(1, MONITOR_MAX_SECS)
            ))
        },
    )
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
        "{id} iniciado em segundo plano. Quando terminar chega sozinho um aviso [Job {id}: ...] com o código de saída e as últimas linhas: continue trabalhando. Ler antes: process action=output id={id}; encerrar: process action=kill id={id}."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_security::{preset, Mode};

    #[test]
    fn the_window_lists_and_stops_the_jobs_of_any_registry() {
        let _serial = serial();
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
        assert!(background_job_tail(&a, 5).is_some());
        assert!(background_job_tail("job-0", 5).is_none());
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

    /// Jobs are process-global (`kill_all_jobs` stops every table): the
    /// tests that start them run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn scratch_policy() -> (tempfile::TempDir, Policy) {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(root).unwrap();
        let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, dir.path());
        (dir, policy)
    }

    /// Collects notices until `done` holds or ~10 s pass.
    fn collect(jobs: &Jobs, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let mut all = Vec::new();
        for _ in 0..200 {
            all.extend(jobs.monitor_notices());
            if done(&all) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        all
    }

    #[test]
    fn a_monitor_reports_each_new_line_then_its_exit_once() {
        let _serial = serial();
        let (dir, policy) = scratch_policy();
        let jobs = Jobs::default();
        let id = jobs
            .start_monitor(
                "echo um && echo dois",
                "teste",
                60,
                dir.path(),
                &policy,
                None,
            )
            .unwrap();
        assert!(jobs.monitors_pending());
        let all = collect(&jobs, |n| n.iter().any(|s| s.contains("terminou")));
        let text = all.join("\n");
        assert!(text.contains(&format!("[Monitor {id}: teste]")), "{text}");
        assert!(text.contains("um") && text.contains("dois"), "{text}");
        assert!(text.contains("terminou (exit 0)"), "{text}");
        assert_eq!(text.matches("terminou").count(), 1, "{text}");
        assert!(
            !jobs.monitors_pending(),
            "a closed monitor has nothing left"
        );
        assert!(
            jobs.monitor_notices().is_empty(),
            "nothing is reported twice"
        );
    }

    /// The turn that started a monitor is gone (its registry dropped); the
    /// host still finds which conversation has news and takes the notices.
    #[test]
    fn a_monitor_outlives_its_turn_and_the_host_can_wake_the_conversation() {
        let _serial = serial();
        let (dir, policy) = scratch_policy();
        let session = "s-wake-1".to_string();
        {
            let jobs = Jobs::default();
            jobs.start_monitor(
                "echo pronto",
                "deploy",
                30,
                dir.path(),
                &policy,
                Some(session.clone()),
            )
            .unwrap();
        } // the turn's registry is dropped here; KEEP holds the table
        let mut woke = false;
        for _ in 0..100 {
            if sessions_with_monitor_news().contains(&session) {
                woke = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(woke, "the conversation has news");
        let mut text = String::new();
        for _ in 0..40 {
            text.push_str(&take_monitor_notices(&session).join("\n"));
            if text.contains("terminou") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            text.contains("pronto") && text.contains("terminou"),
            "{text}"
        );
        assert!(
            !sessions_with_monitor_news().contains(&session),
            "nothing twice"
        );
        assert!(take_monitor_notices(&session).is_empty());
    }

    #[test]
    fn a_monitor_stops_at_its_deadline() {
        let _serial = serial();
        let (dir, policy) = scratch_policy();
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul"
        } else {
            "sleep 30"
        };
        let jobs = Jobs::default();
        let id = jobs
            .start_monitor(long, "longo", 1, dir.path(), &policy, None)
            .unwrap();
        let all = collect(&jobs, |n| n.iter().any(|s| s.contains("prazo")));
        assert!(
            all.join("\n").contains("prazo do monitor esgotado"),
            "{all:?}"
        );
        assert!(
            background_jobs().iter().all(|j| j.id != id),
            "the process is gone"
        );
    }

    #[test]
    fn a_flooding_monitor_is_cut_then_stopped() {
        let _serial = serial();
        let (dir, policy) = scratch_policy();
        // 60 lines per second, forever.
        let flood = "while true; do for i in $(seq 1 60); do echo linha $i; done; sleep 1; done";
        let jobs = Jobs::default();
        let id = jobs
            .start_monitor(flood, "barulho", 60, dir.path(), &policy, None)
            .unwrap();
        let mut all = Vec::new();
        for _ in 0..40 {
            all.extend(jobs.monitor_notices());
            if all.iter().any(|s| s.contains("parado")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1100));
        }
        let text = all.join("\n");
        assert!(text.contains("linhas omitidas"), "{text}");
        assert!(
            text.contains("saída demais") || text.contains("imprimiu mais de"),
            "{text}"
        );
        for n in &all {
            assert!(
                n.lines().count() <= MONITOR_LINES + 2,
                "a notice stays small: {n}"
            );
        }
        assert!(background_jobs().iter().all(|j| j.id != id));
    }

    #[test]
    fn join_readers_with_grace_does_not_hang_on_open_pipe() {
        struct BlockingReader(std::sync::mpsc::Receiver<()>);
        impl Read for BlockingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Ok(0)
            }
        }

        let out = Arc::new(Mutex::new(Output::default()));
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = pump(Some(Box::new(BlockingReader(rx))), &out);
        let done_flag = reader.done.clone();

        let started = Instant::now();
        join_readers_with_grace(vec![reader], Duration::from_millis(60));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!done_flag.load(Ordering::Acquire));

        drop(tx);
        for _ in 0..40 {
            if done_flag.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done_flag.load(Ordering::Acquire));
    }
}
