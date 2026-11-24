//! What tools need beyond their arguments: the session they run for, files the
//! session has read, a queue of things the UI should hear about, background
//! jobs, and the one service only the runtime can provide (spawning subagents).
//!
//! One [`Host`] lives in each [`crate::Registry`] and reaches every call as
//! `Exec::host`. The tools crate cannot depend on the runtime, so the runtime
//! binds a [`Services`] implementation per turn (`Host::bind`). An unbound host
//! answers "indisponível neste contexto" and keeps read-before-edit off (a bare
//! `tools call` has no session to track).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde_json::Value;
use zerith_providers::Cancel;

use crate::{Result, ToolError};

/// One `agent` tool call, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    pub description: String,
    pub prompt: String,
    /// `.zerith/agents/<name>.md` or a built-in (`explore`, `general`, `plan`).
    pub agent: Option<String>,
    pub model: Option<String>,
    pub background: bool,
    /// Optional workspace isolation mode (`"worktree"`).
    pub isolation: Option<String>,
}

/// A skill body, ready for the model.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillLoad {
    pub body: String,
    /// `allowed-tools` from the frontmatter, `None` when it declares none.
    pub allowed_tools: Option<Vec<String>>,
}

/// Something a tool wants the host application to show. The turn loop turns
/// these into `RunEvent`s after each tool group.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    Subagent {
        id: String,
        name: String,
        /// `running`, `done`, `failed` or `cancelled`.
        state: String,
        summary: String,
        /// The model the child runs on (empty when it could not be known).
        model: String,
    },
    /// The session's task list changed; the loop reloads and publishes it.
    Tasks,
    /// Incremental stdout/stderr or terminal output while a tool runs.
    ToolProgress { tool: String, chunk: String },
}

pub type LiveHook = Arc<dyn Fn(HostEvent) + Send + Sync>;

#[derive(Default)]
struct SinkInner {
    queue: Mutex<Vec<HostEvent>>,
    live: Mutex<Option<LiveHook>>,
}

/// A cheap handle to the event queue, usable from worker threads.
#[derive(Clone, Default)]
pub struct EventSink(Arc<SinkInner>);

impl EventSink {
    pub fn set_live(&self, cb: Option<LiveHook>) {
        if let Ok(mut g) = self.0.live.lock() {
            *g = cb;
        }
    }

    pub fn push(&self, e: HostEvent) {
        if matches!(&e, HostEvent::ToolProgress { .. }) {
            // Clone outside any queue lock so callbacks never run with locks held.
            let cb = self.0.live.lock().ok().and_then(|g| g.clone());
            if let Some(cb) = cb {
                cb(e);
            }
            return;
        }
        if let Ok(mut q) = self.0.queue.lock() {
            q.push(e);
        }
    }

    pub(crate) fn take(&self) -> Vec<HostEvent> {
        self.0
            .queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

/// What the runtime provides to the tools of one session.
pub trait Services: Send + Sync {
    /// Runs a subagent of `session` to completion and returns its final text;
    /// with `req.background` it returns an id at once and `agent_result`
    /// fetches the outcome later. `deadline` is the tool call's own budget.
    fn agent(
        &self,
        session: &str,
        req: AgentRequest,
        cancel: &Cancel,
        deadline: Instant,
        sink: EventSink,
    ) -> std::result::Result<String, String>;

    /// The state or final text of a background subagent.
    fn agent_result(&self, session: &str, id: &str) -> std::result::Result<String, String>;

    /// `task_create`, `task_update` or `task_list` on the session's checklist;
    /// returns the text the model reads.
    fn tasks(&self, session: &str, op: &str, args: &Value) -> std::result::Result<String, String>;

    /// The body of an enabled skill.
    fn skill(&self, name: &str) -> std::result::Result<SkillLoad, String>;

    /// The skills library of this turn (plugin folders included), for
    /// `skill_search`. `None`: no library.
    fn skills_root(&self) -> Option<zerith_skills::Library> {
        None
    }

    /// Searches past session messages by text/FTS query.
    fn session_search(&self, _query: &str, _limit: usize) -> std::result::Result<String, String> {
        Err("busca em sessões indisponível neste contexto".into())
    }
}

struct Bound {
    session: String,
    services: Arc<dyn Services>,
    /// A child's host: the turn loop must not rebind it to the parent's.
    locked: bool,
}

type Reads = HashMap<PathBuf, SystemTime>;

/// Files each session has read, by session id. Process-wide on purpose: a
/// host that builds a fresh `Registry` per turn must not make the model re-read
/// every file it already knows.
static READS: Mutex<Option<HashMap<String, Reads>>> = Mutex::new(None);

/// Runs before a tool mutates a file; an `Err` cancels the edit.
type WriteHook = Arc<dyn Fn(&Path) -> std::result::Result<(), String> + Send + Sync>;
/// Runs with the workspace before a shell command that may mutate it.
type ShellHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Default)]
pub struct Host {
    /// Called with a path just before a tool mutates it (checkpoints).
    before_write: Mutex<Option<WriteHook>>,
    /// Called before a shell command that is not read-only (checkpoints).
    before_shell: Mutex<Option<ShellHook>>,
    bound: Mutex<Option<Bound>>,
    events: EventSink,
    skills: Mutex<HashMap<String, Option<Vec<String>>>>,
    /// Vault values a tool typed somewhere (browser `fill_secret`): scrubbed
    /// from every later output of this host, so a page that echoes the
    /// password back never hands it to the model.
    secrets: Mutex<Vec<String>>,
    jobs: crate::jobs::Jobs,
}

impl Host {
    /// Ties the host to a session. Read-before-edit is enforced from here on.
    /// A no-op on a host bound with [`Host::bind_locked`].
    pub fn bind(&self, session: &str, services: Arc<dyn Services>) {
        self.set(session, services, false);
    }

    /// Like [`Host::bind`], and later `bind` calls are ignored. For a child
    /// whose registry the turn loop would otherwise rebind to the parent.
    pub fn bind_locked(&self, session: &str, services: Arc<dyn Services>) {
        self.set(session, services, true);
    }

    fn set(&self, session: &str, services: Arc<dyn Services>, locked: bool) {
        if let Ok(mut b) = self.bound.lock() {
            if b.as_ref().is_some_and(|b| b.locked) {
                return;
            }
            *b = Some(Bound {
                session: session.to_string(),
                services,
                locked,
            });
        }
    }

    pub fn session(&self) -> Option<String> {
        self.bound.lock().ok()?.as_ref().map(|b| b.session.clone())
    }

    /// The bound session and its services, or "indisponível" when unbound.
    pub(crate) fn bound(&self) -> Result<(String, Arc<dyn Services>)> {
        self.bound
            .lock()
            .ok()
            .and_then(|b| b.as_ref().map(|b| (b.session.clone(), b.services.clone())))
            .ok_or_else(|| ToolError::Failed("indisponível neste contexto".into()))
    }

    pub fn sink(&self) -> EventSink {
        self.events.clone()
    }

    /// Events queued since the last call. Workers push, the loop drains.
    pub fn take_events(&self) -> Vec<HostEvent> {
        self.events.take()
    }

    pub fn session_search(&self, query: &str, limit: usize) -> Result<String> {
        let (_, svc) = self.bound()?;
        svc.session_search(query, limit).map_err(ToolError::Failed)
    }

    /// Records that the model loaded a skill, with the `allowed-tools` it
    /// declares (`None`: none declared).
    pub fn note_skill(&self, name: &str, allowed: Option<Vec<String>>) {
        if let Ok(mut s) = self.skills.lock() {
            s.insert(name.to_string(), allowed);
        }
    }

    /// The `allowed-tools` of a skill the model loaded in this host's life.
    /// `None` when it was not loaded or declares none. Advisory: a guard can
    /// narrow the tool set with it.
    pub fn skill_allowed_tools(&self, name: &str) -> Option<Vec<String>> {
        self.skills.lock().ok()?.get(name)?.clone()
    }

    /// Remembers a vault value this host used, to scrub it from outputs.
    pub fn note_secret(&self, value: &str) {
        if value.chars().count() < 4 {
            return;
        }
        if let Ok(mut s) = self.secrets.lock() {
            if !s.iter().any(|v| v == value) {
                s.push(value.to_string());
            }
        }
    }

    /// `text` with every vault value this host used replaced.
    pub fn scrub_secrets(&self, text: &str) -> String {
        let Ok(s) = self.secrets.lock() else {
            return text.to_string();
        };
        s.iter()
            .fold(text.to_string(), |t, v| t.replace(v.as_str(), "[redacted]"))
    }

    pub(crate) fn jobs(&self) -> &crate::jobs::Jobs {
        &self.jobs
    }

    /// New output of this host's `monitor` jobs, one notice per monitor, and
    /// their final notices. The turn loop calls it at each step boundary.
    pub fn monitor_notices(&self) -> Vec<String> {
        self.jobs.monitor_notices()
    }

    /// Stops this host's monitors without a final notice (the turn was
    /// stopped by the person).
    pub fn stop_monitors(&self) {
        self.jobs.stop_monitors();
    }

    /// Whether a `monitor` job has something new right now (no side effect).
    pub fn monitors_have_news(&self) -> bool {
        self.jobs.monitors_have_news()
    }

    /// Whether a `monitor` job of this host still has something to report.
    pub fn monitors_pending(&self) -> bool {
        self.jobs.monitors_pending()
    }

    // --- read-before-edit -------------------------------------------------

    fn key(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    fn with_reads<R>(&self, f: impl FnOnce(&mut Reads) -> R) -> Option<R> {
        let sid = self.session()?;
        let mut g = READS.lock().ok()?;
        Some(f(g
            .get_or_insert_with(HashMap::new)
            .entry(sid)
            .or_default()))
    }

    /// The session has seen `path` as it is on disk now (a read or its own
    /// write). No-op on an unbound host.
    pub fn note_read(&self, path: &Path) {
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            return;
        };
        let key = Self::key(path);
        self.with_reads(|r| r.insert(key, mtime));
    }

    /// Registers the hook run before `write_file` / `edit_file` mutate a
    /// file. An `Err` aborts the edit and is shown to the model.
    pub fn on_before_write(
        &self,
        f: impl Fn(&Path) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) {
        *self.before_write.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
    }

    /// Runs the registered pre-write hook, if any.
    pub fn before_write(&self, path: &Path) -> Result<()> {
        let hook = self
            .before_write
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match hook {
            Some(f) => f(path).map_err(|e| {
                ToolError::Failed(format!("checkpoint falhou, edição cancelada: {e}"))
            }),
            None => Ok(()),
        }
    }

    /// Registers the hook run before a shell command that is not read-only
    /// (foreground or background), with the folder it runs in. It cannot
    /// cancel the command.
    pub fn on_before_shell(&self, f: impl Fn(&Path) + Send + Sync + 'static) {
        *self.before_shell.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
    }

    /// Runs the registered pre-shell hook unless `cmd` is read-only.
    pub fn before_shell(&self, cmd: &str, workspace: &Path) {
        let hook = self
            .before_shell
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(f) = hook {
            if !zerith_core::shellparse::all_read_only(cmd) {
                f(workspace);
            }
        }
    }

    /// Err (pt-BR, for the model) when `path` exists, this session never read
    /// it, or it changed since. New files and unbound hosts pass.
    pub fn check_editable(&self, path: &Path) -> Result<()> {
        if self.session().is_none() {
            return Ok(());
        }
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            return Ok(());
        };
        let key = Self::key(path);
        match self.with_reads(|r| r.get(&key).copied()).flatten() {
            None => Err(ToolError::Failed(format!(
                "leia {} com read_file antes de alterá-lo: editar sem ver o conteúdo atual é arriscado",
                path.display()
            ))),
            Some(seen) if seen != mtime => Err(ToolError::Failed(format!(
                "{} mudou depois da última leitura; chame read_file de novo antes de alterá-lo",
                path.display()
            ))),
            Some(_) => Ok(()),
        }
    }
}
