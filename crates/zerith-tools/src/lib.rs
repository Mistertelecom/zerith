//! The tool registry.
//!
//! Every tool declares the same nine things the spec lists, in one struct, so a
//! UI can show them without the tool existing twice: name, description,
//! parameters, expected result, permissions, timeout, cancellation, locality
//! and provenance.
//!
//! The policy is consulted in one place, [`Registry::authorize`], before any
//! tool touches the filesystem or spawns a process. A host that wants to ask
//! the user (`Verdict::Ask`) calls `authorize`, asks, then `run_approved`;
//! [`Registry::call`] is the non-interactive path and treats Ask as Deny.

mod agent_tool;
mod ask_user;
mod authoring;
mod browser;
mod builtin;
mod diagnostics;
mod fntool;
mod host;
mod jobs;
pub mod lsp;
mod media;
pub mod network;
mod patch;
pub mod remote;
mod skill_tool;
mod tasks;
mod terminal;
mod web;
mod workflows;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use zerith_providers::Cancel;
use zerith_security::{Action, Policy, Verdict};

pub use ask_user::{parse_questions, Question, QuestionOption};
pub use authoring::{tools as authoring_tools, Authoring};
pub use builtin::{shell_command, IMAGE_MARKER};
pub use host::{AgentRequest, EventSink, Host, HostEvent, LiveHook, Services, SkillLoad};
pub use jobs::{
    background_job_tail, background_jobs, kill_all_jobs, sessions_with_monitor_news,
    stop_background_job, take_monitor_notices, BackgroundJob,
};
pub use media::{tools as media_tools, tools_with_key as media_tools_with_key};
pub use skill_tool::check as check_skill_args;
pub use tasks::{check as check_task_args, STATUSES as TASK_STATUSES};
pub use terminal::{
    close_all_terminals, close_terminal, interrupt_terminal, list_active_terminals,
    send_terminal_input, set_terminal_change_hook, terminal_meta, terminal_program, terminal_tail,
    TerminalSnapshot,
};
pub use web::SearchTool;
pub use workflows::{set_workflow_ai_executor, set_workflow_change_hook};

/// What a caller may be asked to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Shell,
    Network,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
            Permission::Shell => "shell",
            Permission::Network => "network",
        }
    }
}

/// Everything a catalogue needs to display a tool honestly.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// What a successful call returns, in words. A schema for the result would
    /// be a lie for free-form tools; a sentence is not.
    pub returns: &'static str,
    pub permission: Permission,
    pub timeout: Duration,
    /// Whether a long call can be stopped. Reported, never assumed.
    pub cancellable: bool,
    pub local: bool,
    /// Where it came from: `built-in`, `mcp:<server>`, `plugin:<name>`.
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("no such tool: {0}")]
    NotFound(String),
    #[error("{0}")]
    BadArguments(String),
    #[error("permission denied: {0}")]
    Denied(String),
    #[error("timeout after {:?}", .0)]
    Timeout(Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ToolError>;

/// What a tool may touch. Passed per call so two windows can hold different
/// workspaces without sharing mutable state.
pub struct CallCtx<'a> {
    pub workspace: &'a Path,
    pub policy: &'a Policy,
    pub memory: Option<&'a dyn zerith_memory::MemoryBackend>,
    pub scope: Option<&'a zerith_memory::Scope>,
}

impl CallCtx<'_> {
    /// Relative paths resolve against the workspace; absolute ones are taken
    /// as given and still go through the policy, which is what bounds them.
    pub fn resolve(&self, p: &str) -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            self.workspace.join(path)
        }
    }
}

/// The running budget of one call: a deadline and the turn's cancel token.
/// Long tools poll [`Exec::check`]; the shell kills its child on either.
pub struct Exec<'a> {
    pub cancel: &'a Cancel,
    pub deadline: Instant,
    pub timeout: Duration,
    /// Session state shared by the tools of one registry.
    pub host: &'a Host,
}

impl Exec<'_> {
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ToolError::Timeout(self.timeout));
        }
        Ok(())
    }
}

/// One tool. `action` says what the call would do in policy terms (`None`
/// for a tool with no side effect worth asking about); `run` does it.
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>>;
    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String>;

    /// May this call run beside other safe calls of the same model step?
    /// Only for calls with no side effects and no shared mutable state: the
    /// loop runs consecutive safe calls on threads, and an unsafe one is a
    /// barrier. Default false, so a new tool is serial until it opts in.
    fn concurrency_safe(&self, _args: &Value) -> bool {
        false
    }

    /// Needs `CallCtx::memory`. The memory backend is not `Sync`, so the loop
    /// runs such a call on its own thread, never on a worker.
    fn uses_memory(&self) -> bool {
        false
    }
}

/// The cap on what one call hands back to the model. Bigger outputs keep the
/// head and say how much was cut, so the model knows to narrow the request.
pub const OUTPUT_CAP_CHARS: usize = 30_000;

/// `ToolSpec::source` prefix of a deferred tool: registered and runnable, but
/// left out of the tool list sent to the model (a big MCP server's schemas cost
/// tens of thousands of tokens per request). The model reaches it through
/// [`DISPATCH_TOOL`], and the runtime judges that call as the real tool.
pub const DEFERRED_SOURCE: &str = "deferred:";
/// `{tool, arguments}`: calls a deferred tool by name (see `zerith-mcp`).
pub const DISPATCH_TOOL: &str = "mcp_call";

pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
    host: Host,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// The built-in set. Small on purpose: a tool that cannot be tested is a
    /// tool that cannot be trusted with a workspace.
    pub fn new() -> Self {
        let mut tools: Vec<Arc<dyn Tool>> = builtin::all().into_iter().map(Arc::from).collect();
        tools.extend(fntool::extra().into_iter().map(Arc::from));
        tools.extend(media::all().into_iter().map(Arc::from));
        Self {
            tools,
            host: Host::default(),
        }
    }

    pub fn empty() -> Self {
        Self {
            tools: Vec::new(),
            host: Host::default(),
        }
    }

    /// Adds a tool; a later tool with the same name replaces the earlier one.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.retain(|t| t.spec().name != tool.spec().name);
        self.tools.push(Arc::from(tool));
    }

    /// Session state of this registry: bound by the runtime, read by tools.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// A registry with only the tools `keep` accepts, sharing their code but
    /// not their state (own host, own background jobs, own read history). What
    /// a subagent gets.
    pub fn subset(&self, keep: impl Fn(&str) -> bool) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .filter(|t| keep(&t.spec().name))
                .cloned()
                .collect(),
            host: Host::default(),
        }
    }

    /// Replaces `agent` with a variant whose calls never join a parallel
    /// group. For a subagent that may spawn children: it already holds a
    /// concurrency permit.
    pub fn serialize_agents(&mut self) {
        for t in agent_tool::all(false) {
            // Only swap tools the registry already offers: a child without
            // `agent` must not gain it here.
            if self.get(&t.spec().name).is_some() {
                self.register(t);
            }
        }
    }

    pub fn specs(&self) -> Vec<&ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.find(name).map(|t| t.spec())
    }

    /// Models sometimes send an array or object argument as a JSON string
    /// (`"edits": "[{...}]"`). Where the tool's schema asks for an array or an
    /// object and the string parses to one, use the parsed value; anything
    /// else is left for the tool to judge.
    pub fn coerce_args(&self, name: &str, args: &mut Value) {
        let (Some(spec), Some(obj)) = (self.get(name), args.as_object_mut()) else {
            return;
        };
        let Some(props) = spec.parameters.get("properties").and_then(Value::as_object) else {
            return;
        };
        for (key, val) in obj.iter_mut() {
            let want = props
                .get(key)
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str);
            let Some(text) = val
                .as_str()
                .filter(|_| matches!(want, Some("array" | "object")))
            else {
                continue;
            };
            match serde_json::from_str::<Value>(text.trim()) {
                Ok(v @ Value::Array(_)) if want == Some("array") => *val = v,
                Ok(v @ Value::Object(_)) if want == Some("object") => *val = v,
                _ => {}
            }
        }
    }

    fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.spec().name == name)
            .map(|t| t.as_ref())
    }

    /// The prompt catalogue: one line per tool, name and description only.
    /// Parameter schemas go to the model as tool definitions, not prose.
    pub fn catalog(&self) -> String {
        self.tools
            .iter()
            .map(|t| format!("- {}: {}", t.spec().name, t.spec().description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Policy verdict for one call, plus the action it was decided on (so a
    /// host can show the user exactly what it is approving).
    pub fn authorize(
        &self,
        name: &str,
        args: &Value,
        ctx: &CallCtx<'_>,
    ) -> Result<(Verdict, Option<Action>)> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolError::NotFound(name.into()))?;
        let action = tool.action(args, ctx)?;
        let verdict = match &action {
            Some(a) => ctx.policy.check_tool(name, a),
            None => Verdict::Allow,
        };
        Ok((verdict, action))
    }

    /// Runs a call whose verdict the caller already settled. The timeout is
    /// a deadline the tool polls, not a stopwatch read after it returns.
    pub fn run_approved(
        &self,
        name: &str,
        args: &Value,
        ctx: &CallCtx<'_>,
        cancel: &Cancel,
    ) -> Result<String> {
        Ok(truncate(
            self.run_raw(name, args, ctx, cancel)?,
            OUTPUT_CAP_CHARS,
        ))
    }

    /// Like [`Registry::run_approved`] but the output is returned whole, so
    /// the caller can spill what does not fit instead of losing it.
    pub fn run_raw(
        &self,
        name: &str,
        args: &Value,
        ctx: &CallCtx<'_>,
        cancel: &Cancel,
    ) -> Result<String> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolError::NotFound(name.into()))?;
        let timeout = tool.spec().timeout;
        let exec = Exec {
            cancel,
            deadline: Instant::now() + timeout,
            timeout,
            host: &self.host,
        };
        exec.check()?;
        // A vault value a tool typed (browser `fill_secret`) never comes back
        // in any output, whatever the page or the command echoes.
        tool.run(args, ctx, &exec)
            .map(|out| self.host.scrub_secrets(&out))
    }

    /// Whether this exact call may run beside other safe calls. Unknown tool: no.
    pub fn concurrency_safe(&self, name: &str, args: &Value) -> bool {
        self.find(name).is_some_and(|t| t.concurrency_safe(args))
    }

    /// Whether the tool reads `CallCtx::memory` (see [`Tool::uses_memory`]).
    pub fn uses_memory(&self, name: &str) -> bool {
        self.find(name).is_some_and(|t| t.uses_memory())
    }

    /// The non-interactive path: Allow runs, Ask and Deny refuse.
    pub fn call(&self, name: &str, args: &Value, ctx: &CallCtx<'_>) -> Result<String> {
        match self.authorize(name, args, ctx)?.0 {
            Verdict::Allow => self.run_approved(name, args, ctx, &Cancel::default()),
            Verdict::Ask(why) => Err(ToolError::Denied(format!("needs approval: {why}"))),
            Verdict::Deny(why) => Err(ToolError::Denied(why)),
        }
    }
}

/// Keeps the start and the end (where build and test errors land) within
/// `cap` chars, and says how much was dropped from the middle.
pub fn truncate(s: String, cap: usize) -> String {
    let total = s.chars().count();
    if total <= cap {
        return s;
    }
    let (head, tail) = head_tail(&s, cap);
    format!("{head}\n\n[saída truncada: {cap} de {total} caracteres, meio omitido; refine o pedido]\n\n{tail}")
}

/// First and last chars of `s`, two thirds of `cap` at the head.
pub fn head_tail(s: &str, cap: usize) -> (String, String) {
    let total = s.chars().count();
    let head_n = cap * 2 / 3;
    let tail_n = cap - head_n;
    let head = s.chars().take(head_n).collect();
    let tail = s.chars().skip(total.saturating_sub(tail_n)).collect();
    (head, tail)
}

/// Bytes of one `@file` attached to a prompt, and of all of them together.
const MENTION_FILE_CAP: usize = 100_000;
const MENTION_TOTAL_CAP: usize = 250_000;

/// Appends the content of every `@path` in `text` that names a file inside
/// `workspace`, as an `<arquivo>` block, so the model starts with it instead
/// of spending a step on read_file. `@server:resource` (MCP) and paths that
/// escape the workspace are left alone; a file too big is named, not inlined.
pub fn expand_file_mentions(text: &str, workspace: &Path) -> String {
    let Ok(root) = workspace.canonicalize() else {
        return text.to_string();
    };
    let mut out = text.to_string();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut total = 0usize;
    for word in text.split_whitespace() {
        let Some(rel) = word.strip_prefix('@') else {
            continue;
        };
        let rel = rel.trim_end_matches([',', '.', ';', ')', '?', '!']);
        if rel.is_empty() || rel.contains(':') && !Path::new(rel).is_absolute() {
            continue;
        }
        let Ok(canon) = root.join(rel).canonicalize() else {
            continue;
        };
        if !canon.starts_with(&root) || !canon.is_file() || seen.contains(&canon) {
            continue;
        }
        seen.push(canon.clone());
        let size = canon
            .metadata()
            .map(|m| m.len() as usize)
            .unwrap_or(usize::MAX);
        if size > MENTION_FILE_CAP || total + size > MENTION_TOTAL_CAP {
            out.push_str(&format!(
                "\n\n<arquivo caminho=\"{rel}\">[{size} bytes: grande demais para anexar; use read_file com offset/limit]</arquivo>"
            ));
            continue;
        }
        let Ok(bytes) = std::fs::read(&canon) else {
            continue;
        };
        if bytes.contains(&0) {
            continue; // binary
        }
        total += size;
        let body = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        };
        out.push_str(&format!(
            "\n\n<arquivo caminho=\"{rel}\">\n{body}\n</arquivo>"
        ));
    }
    out
}

#[cfg(test)]
mod tests;
