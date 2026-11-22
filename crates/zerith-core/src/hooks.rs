//! User hooks: shell commands run at fixed points of a session.
//!
//! Contract, close in spirit to Claude Code's:
//! - the command gets one JSON object on stdin:
//!   `{event, session_id, tool_name?, tool_input?, tool_output?, prompt?, cwd}`;
//! - exit 0: stdout may hold `{decision?, reason?, updated_input?,
//!   additional_context?, continue?}` (empty or non-JSON stdout is fine);
//! - exit 2: block, stderr is the reason;
//! - any other exit, spawn failure or timeout: non-blocking error, recorded in
//!   `HookOutcome::errors` for the caller to log.
//!
//! Hooks run in the user's shell with the user's privileges. They are config
//! the user wrote, not a sandbox.

use crate::settings::{glob_match, Decision, HookSpec, Settings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    /// A tool call that failed or was refused (Claude Code: `PostToolUseFailure`).
    PostToolUseFailure,
    /// About to ask the user for approval; a hook may answer for them.
    PermissionRequest,
    Stop,
    PreCompact,
    SessionEnd,
}

impl HookEvent {
    /// Events whose hooks carry a `matcher` over the tool.
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            HookEvent::PreToolUse
                | HookEvent::PostToolUse
                | HookEvent::PostToolUseFailure
                | HookEvent::PermissionRequest
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::PermissionRequest => "PermissionRequest",
            HookEvent::Stop => "Stop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::SessionEnd => "SessionEnd",
        }
    }

    fn specs(self, s: &Settings) -> &[HookSpec] {
        let h = &s.hooks;
        match self {
            HookEvent::SessionStart => &h.session_start,
            HookEvent::UserPromptSubmit => &h.user_prompt_submit,
            HookEvent::PreToolUse => &h.pre_tool_use,
            HookEvent::PostToolUse => &h.post_tool_use,
            HookEvent::PostToolUseFailure => &h.post_tool_use_failure,
            HookEvent::PermissionRequest => &h.permission_request,
            HookEvent::Stop => &h.stop,
            HookEvent::PreCompact => &h.pre_compact,
            HookEvent::SessionEnd => &h.session_end,
        }
    }
}

/// What the host knows at the hook point. `event` is filled by the runner.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HookPayload {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub cwd: PathBuf,
    /// What a `Tool(specifier)` matcher is tested against: the command for
    /// `shell`, the path for file tools, the URL for network tools. Not sent
    /// to the hook (it is already inside `tool_input`).
    #[serde(skip)]
    pub subject: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct HookReply {
    decision: Option<Decision>,
    reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
    #[serde(rename = "continue")]
    continue_: Option<bool>,
}

/// The combined result of every hook that ran for one event.
#[derive(Debug, Clone, PartialEq)]
pub struct HookOutcome {
    /// Strongest decision any hook gave: deny > ask > allow. `None`: no hook
    /// expressed one; the normal permission path decides.
    pub decision: Option<Decision>,
    /// Reasons from hooks that denied or asked, in run order.
    pub reasons: Vec<String>,
    /// Replacement tool input; the last hook that set one wins.
    pub updated_input: Option<Value>,
    /// Text to add to the model's context.
    pub additional_context: Vec<String>,
    /// `false` when a hook asked the session to stop.
    pub continue_: bool,
    /// Non-blocking failures (bad exit, timeout, spawn error).
    pub errors: Vec<String>,
}

impl Default for HookOutcome {
    fn default() -> Self {
        Self {
            decision: None,
            reasons: Vec::new(),
            updated_input: None,
            additional_context: Vec::new(),
            continue_: true,
            errors: Vec::new(),
        }
    }
}

impl HookOutcome {
    /// The action must not happen.
    pub fn blocked(&self) -> bool {
        self.decision == Some(Decision::Deny) || !self.continue_
    }

    fn decide(&mut self, d: Decision, reason: Option<String>) {
        let rank = |d: Option<Decision>| match d {
            None => 0,
            Some(Decision::Allow) => 1,
            Some(Decision::Ask) => 2,
            Some(Decision::Deny) => 3,
        };
        if rank(Some(d)) > rank(self.decision) {
            self.decision = Some(d);
        }
        if d != Decision::Allow {
            if let Some(r) = reason.filter(|r| !r.trim().is_empty()) {
                self.reasons.push(r);
            }
        }
    }
}

/// Runs every hook configured for `event`, in order, and folds the results.
///
/// Call sites (the runtime and the UI own them):
/// - `SessionStart` / `SessionEnd`: when a session opens / closes;
/// - `UserPromptSubmit`: before the prompt is stored, `payload.prompt` set; a
///   block drops the prompt, `additional_context` is added to the turn;
/// - `PreToolUse` / `PostToolUse` / `PostToolUseFailure` /
///   `PermissionRequest`: per tool call (the guards middleware does this);
/// - `Stop`: when the model ends a turn; `continue_ == false` or a deny asks
///   the host to keep the agent going with the reason as the next prompt;
/// - `PreCompact`: before the context is compacted.
///
/// Fail-open: a hook that crashes, times out or exits with anything but 0/2 is
/// recorded in `errors` and changes nothing. Only exit 2 or an explicit
/// `deny` / `continue: false` blocks.
pub fn run_hooks(event: HookEvent, payload: &HookPayload, settings: &Settings) -> HookOutcome {
    let mut out = HookOutcome::default();
    let tool = payload.tool_name.as_deref().unwrap_or("");
    let subject = payload.subject.as_deref();
    for spec in event.specs(settings) {
        if event.is_tool_event() && !matcher_hits(spec.matcher.as_deref(), tool, subject) {
            continue;
        }
        run_one(spec, event, payload, &mut out);
        if out.blocked() {
            break;
        }
    }
    out
}

/// `matcher`: `|`-separated entries, each `tool` (`*` glob) or
/// `tool(specifier)`. A `|` inside parentheses belongs to the specifier.
/// A specifier entry needs a subject; without one it does not match.
pub fn matcher_hits(matcher: Option<&str>, tool: &str, subject: Option<&str>) -> bool {
    let Some(m) = matcher.map(str::trim).filter(|m| !m.is_empty()) else {
        return true;
    };
    split_alternatives(m).iter().any(|entry| {
        let entry = entry.trim();
        match entry.split_once('(') {
            Some((name, rest)) => {
                let spec = rest.strip_suffix(')').unwrap_or(rest);
                glob_match(name.trim(), tool)
                    && subject.is_some_and(|s| subject_hits(tool, spec, s))
            }
            None => glob_match(entry, tool),
        }
    })
}

fn split_alternatives(m: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in m.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth <= 0 => {
                out.push(&m[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&m[start..]);
    out
}

fn subject_hits(tool: &str, spec: &str, subject: &str) -> bool {
    let spec = spec.trim().to_lowercase();
    let spec = match spec.strip_suffix(":*") {
        Some(p) => format!("{p} *"),
        None => spec,
    };
    let one = |text: &str| {
        glob_match(&spec, text)
            || spec.strip_suffix(" *").is_some_and(|p| p == text)
            // `*.env` against a path also matches the file name.
            || (!spec.contains('/')
                && glob_match(&spec, text.rsplit('/').next().unwrap_or(text)))
    };
    let lower = subject.to_lowercase();
    if tool == "shell" {
        let a = crate::shellparse::analyze(&lower);
        one(&lower) || a.raw.iter().chain(&a.inner).any(|p| one(p))
    } else {
        one(&lower.replace('\\', "/"))
    }
}

fn run_one(spec: &HookSpec, event: HookEvent, payload: &HookPayload, out: &mut HookOutcome) {
    let mut input = serde_json::to_value(payload).unwrap_or_default();
    input["event"] = Value::String(event.as_str().into());
    let label = format!("hook {} `{}`", event.as_str(), spec.command);
    match exec(
        &spec.command,
        &input.to_string(),
        &payload.cwd,
        spec.timeout_ms,
    ) {
        Err(e) => out.errors.push(format!("{label}: {e}")),
        Ok((Some(0), stdout, _)) => {
            let text = stdout.trim();
            if text.is_empty() {
                return;
            }
            let Ok(reply) = serde_json::from_str::<HookReply>(text) else {
                // Plain stdout is context, as in Claude Code.
                out.additional_context.push(text.to_string());
                return;
            };
            if let Some(d) = reply.decision {
                out.decide(d, reply.reason.clone());
            }
            if reply.updated_input.is_some() {
                out.updated_input = reply.updated_input;
            }
            if let Some(c) = reply.additional_context {
                out.additional_context.push(c);
            }
            if reply.continue_ == Some(false) {
                out.continue_ = false;
                if let Some(r) = reply.reason {
                    out.reasons.push(r);
                }
            }
        }
        Ok((Some(2), _, stderr)) => {
            let reason = stderr.trim();
            let reason = if reason.is_empty() {
                format!("{label} bloqueou sem motivo")
            } else {
                reason.to_string()
            };
            out.decide(Decision::Deny, Some(reason));
        }
        Ok((code, _, stderr)) => out.errors.push(format!(
            "{label}: saiu com {}: {}",
            code.map_or("sinal".to_string(), |c| c.to_string()),
            stderr.trim()
        )),
    }
}

/// Spawns `command` in the platform shell, feeds stdin, enforces the timeout.
/// Returns `(exit code, stdout, stderr)`.
fn exec(
    command: &str,
    stdin: &str,
    cwd: &std::path::Path,
    timeout_ms: u64,
) -> Result<(Option<i32>, String, String), String> {
    let mut cmd = shell(command);
    if cwd.is_dir() {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("não iniciou: {e}"))?;

    // Every pipe on its own thread: a hook that ignores stdin or floods
    // stdout must not deadlock the host.
    if let Some(mut w) = child.stdin.take() {
        let data = stdin.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(data.as_bytes());
        });
    }
    let (otx, orx) = mpsc::channel();
    let (etx, erx) = mpsc::channel();
    if let Some(r) = child.stdout.take() {
        std::thread::spawn(move || otx.send(read_all(r)));
    }
    if let Some(r) = child.stderr.take() {
        std::thread::spawn(move || etx.send(read_all(r)));
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(&mut child);
                return Err(format!("excedeu {timeout_ms} ms e foi encerrado"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    };
    // A grandchild may still hold the pipes open; do not wait past the deadline.
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(100));
    let stdout = orx.recv_timeout(wait).unwrap_or_default();
    let stderr = erx
        .recv_timeout(Duration::from_millis(100))
        .unwrap_or_default();
    Ok((status.code(), stdout, stderr))
}

fn read_all(mut r: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    // raw_arg: std's quoting is for C runtimes, and cmd parses quotes its own way.
    c.raw_arg(format!("/S /C \"{command}\""));
    c
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(command);
    c
}

#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    // `cmd /C` runs the real work in a grandchild; killing cmd alone leaves it.
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(windows))]
fn kill_tree(child: &mut std::process::Child) {
    // ponytail: kills `sh` only; a hook that backgrounds work keeps it running.
    // Upgrade: spawn in a new process group and kill the group.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(event: &str, specs: &[(&str, &str, u64)]) -> Settings {
        let mut text = String::new();
        for (matcher, command, timeout) in specs {
            text.push_str(&format!(
                "[[hooks.{event}]]\nmatcher = {matcher:?}\ncommand = {command:?}\ntimeout_ms = {timeout}\n"
            ));
        }
        toml::from_str(&text).unwrap()
    }

    fn payload(tool: &str) -> HookPayload {
        HookPayload {
            session_id: "s1".into(),
            tool_name: Some(tool.into()),
            tool_input: Some(serde_json::json!({"path": "a.txt"})),
            cwd: std::env::current_dir().unwrap(),
            ..Default::default()
        }
    }

    #[cfg(windows)]
    const DENY_JSON: &str = r#"echo {"decision":"deny","reason":"nao"}"#;
    #[cfg(not(windows))]
    const DENY_JSON: &str = r#"echo '{"decision":"deny","reason":"nao"}'"#;
    #[cfg(windows)]
    const EXIT2: &str = "echo proibido 1>&2 & exit /b 2";
    #[cfg(not(windows))]
    const EXIT2: &str = "echo proibido 1>&2; exit 2";
    #[cfg(windows)]
    const EXIT1: &str = "exit /b 1";
    #[cfg(not(windows))]
    const EXIT1: &str = "exit 1";
    #[cfg(windows)]
    const SLOW: &str = "ping -n 6 127.0.0.1 >nul";
    #[cfg(not(windows))]
    const SLOW: &str = "sleep 5";
    const CONTEXT: &str = "echo lembrete";
    // Prints only when stdin holds the event and the prompt.
    #[cfg(windows)]
    const CAT: &str = r#"findstr /C:"UserPromptSubmit" >nul && echo recebido"#;
    #[cfg(not(windows))]
    const CAT: &str = "grep -q '\"prompt\":\"oi\"' && echo recebido";

    #[test]
    fn json_deny_blocks_and_carries_the_reason() {
        let s = settings("PreToolUse", &[("shell|write_*", DENY_JSON, 10_000)]);
        let o = run_hooks(HookEvent::PreToolUse, &payload("write_file"), &s);
        assert!(o.blocked(), "{o:?}");
        assert_eq!(o.reasons, vec!["nao"]);
    }

    #[test]
    fn matcher_supports_tool_specifiers() {
        let m = |m: &str, t: &str, s: Option<&str>| matcher_hits(Some(m), t, s);
        assert!(m(
            "shell(git push *)",
            "shell",
            Some("git push origin main")
        ));
        assert!(m(
            "shell(git push *)",
            "shell",
            Some("timeout 5 git push origin")
        ));
        assert!(m("shell(git push *)", "shell", Some("ls && git push x")));
        assert!(!m("shell(git push *)", "shell", Some("git status")));
        assert!(!m("shell(git push *)", "shell", None));
        assert!(m(
            "write_file(*.env)|read_file",
            "write_file",
            Some("a/b/.env")
        ));
        assert!(m("write_file(*.env)|read_file", "read_file", None));
        assert!(m("", "x", None));
        assert!(m("mcp__*", "mcp__gh__search", None));
    }

    #[test]
    fn specifier_matcher_gates_the_hook() {
        let s = settings("PreToolUse", &[("shell(rm *)", DENY_JSON, 10_000)]);
        let mut p = payload("shell");
        p.subject = Some("rm -rf x".into());
        assert!(run_hooks(HookEvent::PreToolUse, &p, &s).blocked());
        p.subject = Some("ls".into());
        assert!(!run_hooks(HookEvent::PreToolUse, &p, &s).blocked());
    }

    #[test]
    fn new_events_read_their_own_tables() {
        let s = settings("PostToolUseFailure", &[("", CONTEXT, 10_000)]);
        let o = run_hooks(HookEvent::PostToolUseFailure, &payload("shell"), &s);
        assert_eq!(o.additional_context, vec!["lembrete"]);
        let none = run_hooks(HookEvent::PostToolUse, &payload("shell"), &s);
        assert!(none.additional_context.is_empty());
        let s = settings("PermissionRequest", &[("", DENY_JSON, 10_000)]);
        assert!(run_hooks(HookEvent::PermissionRequest, &payload("shell"), &s).blocked());
    }

    #[test]
    fn matcher_skips_other_tools() {
        let s = settings("PreToolUse", &[("shell", DENY_JSON, 10_000)]);
        let o = run_hooks(HookEvent::PreToolUse, &payload("read_file"), &s);
        assert_eq!(o, HookOutcome::default());
    }

    #[test]
    fn exit_2_blocks_with_stderr_and_other_codes_do_not() {
        let s = settings("PreToolUse", &[("", EXIT2, 10_000)]);
        let o = run_hooks(HookEvent::PreToolUse, &payload("shell"), &s);
        assert!(o.blocked());
        assert_eq!(o.reasons, vec!["proibido"]);

        let s = settings("PreToolUse", &[("", EXIT1, 10_000)]);
        let o = run_hooks(HookEvent::PreToolUse, &payload("shell"), &s);
        assert!(!o.blocked());
        assert_eq!(o.errors.len(), 1);
    }

    #[test]
    fn a_slow_hook_is_killed_at_the_timeout() {
        let s = settings("Stop", &[("", SLOW, 300)]);
        let t = Instant::now();
        let o = run_hooks(HookEvent::Stop, &payload("x"), &s);
        assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
        assert!(o.errors[0].contains("300 ms"), "{o:?}");
        assert!(!o.blocked());
    }

    #[test]
    fn plain_stdout_is_context_and_stdin_carries_the_payload() {
        let s = settings(
            "UserPromptSubmit",
            &[("", CONTEXT, 10_000), ("", CAT, 10_000)],
        );
        let mut p = payload("x");
        p.prompt = Some("oi".into());
        let o = run_hooks(HookEvent::UserPromptSubmit, &p, &s);
        assert_eq!(o.additional_context, vec!["lembrete", "recebido"], "{o:?}");
    }
}
