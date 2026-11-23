//! The tool pipeline, in phases:
//!
//! ```text
//! parse args -> before-middleware -> policy -> approval        (main thread, call order)
//!            -> execute (timeout, cancel; parallel for safe calls)
//!            -> after-middleware -> finalize (spill/truncate)  (main thread, call order)
//!            -> persist                                        (the loop)
//! ```
//!
//! Middleware is how a host plugs hooks and permission rules in without the
//! runtime knowing about their crates. It can only make a call stricter
//! (`Deny`, `Ask`) or replace its result; it cannot allow what the policy
//! denies, and rewritten arguments go through the policy again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{Cancel, ToolCall};
use zerith_security::{Action, Policy, Verdict};
use zerith_tools::{CallCtx, HostEvent, Registry, OUTPUT_CAP_CHARS};

use crate::agent::{Approval, ApprovalGate};
use crate::{spill, RunEvent};

/// One tool call as middleware sees it. `args` may be rewritten by `before`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub session_id: String,
    pub cwd: PathBuf,
}

/// What `before` decides.
#[derive(Debug, Clone, PartialEq)]
pub enum Flow {
    Continue,
    /// Refuse the call; the model reads the reason.
    Deny(String),
    /// Make the user confirm, even if the policy would run it. The summary is
    /// what the approval prompt shows.
    Ask(String),
    /// Skip execution and use this as the tool's output (a cached or
    /// synthesized result).
    Replace(String),
}

/// The result `after` may rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
    /// Extra text for the model, added as a user message after the step's
    /// tool results (a hook saying "tests are failing", say).
    pub context: Vec<String>,
}

impl ToolOutcome {
    pub fn new(ok: bool, output: impl Into<String>) -> Self {
        Self {
            ok,
            output: output.into(),
            context: Vec::new(),
        }
    }
}

/// A step in the pipeline. Both methods default to doing nothing, so an
/// implementation overrides only the side it needs. `before` runs in the
/// order of `AgentOptions::middleware`, stopping at the first `Deny` or
/// `Replace` (an `Ask` is remembered and the chain goes on, so a later `Deny`
/// still wins); `after` runs in the same order over every outcome, denied and
/// failed ones included.
pub trait Middleware: Send + Sync {
    fn before(&self, _call: &mut ToolInvocation) -> Flow {
        Flow::Continue
    }
    fn after(&self, _call: &ToolInvocation, _result: &mut ToolOutcome) {}
}

/// Does nothing. The default for hosts without hooks.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMiddleware;

impl Middleware for NoopMiddleware {}

static NEXT_APPROVAL: AtomicU64 = AtomicU64::new(1);

const PANICKED: &str = "erro interno na ferramenta (panic)";

/// A call that went through the first phases. `settled` is `Some` when it
/// already has its outcome (refused, replaced) and must not run.
pub(crate) struct Prepared {
    pub inv: ToolInvocation,
    pub settled: Option<ToolOutcome>,
}

pub(crate) struct Pipeline<'a> {
    pub core: Option<(&'a Registry, &'a Policy)>,
    pub memory: Option<&'a dyn MemoryBackend>,
    pub scope: Option<&'a Scope>,
    pub middleware: &'a [Box<dyn Middleware + Send + Sync>],
    pub approvals: Option<&'a ApprovalGate>,
    /// The root turn emits the approval prompts its subagents queue on the
    /// shared gate while an `agent` call runs. Children only queue.
    pub relay: bool,
    pub spill_dir: Option<&'a Path>,
    pub session_id: &'a str,
    pub cancel: &'a Cancel,
    pub orchestrator_root: bool,
}

impl Pipeline<'_> {
    fn ctx<'c>(&'c self, policy: &'c Policy) -> CallCtx<'c> {
        CallCtx {
            workspace: &policy.workspace,
            policy,
            memory: self.memory,
            scope: self.scope,
        }
    }

    /// May this raw call join a parallel group? Judged on the model's own
    /// arguments; `run_group` checks again after middleware.
    pub fn is_safe(&self, call: &ToolCall) -> bool {
        let Some((tools, _)) = self.core else {
            return false;
        };
        parse_args(&call.arguments).is_ok_and(|a| tools.concurrency_safe(&call.name, &a))
    }

    /// Phases 1-4: arguments, before-middleware, policy, approval.
    pub fn prepare(&self, call: &ToolCall, on_event: &mut dyn FnMut(RunEvent)) -> Prepared {
        let mut inv = ToolInvocation {
            id: call.id.clone(),
            name: call.name.clone(),
            args: Value::Null,
            session_id: self.session_id.to_string(),
            cwd: self
                .core
                .map(|(_, p)| p.workspace.clone())
                .unwrap_or_default(),
        };
        let settle = |inv: ToolInvocation, ok: bool, text: String| Prepared {
            inv,
            settled: Some(ToolOutcome::new(ok, text)),
        };
        let Some((tools, policy)) = self.core else {
            return settle(
                inv,
                false,
                "erro: ferramentas desativadas neste turno".into(),
            );
        };
        if self.orchestrator_root && !crate::agent::is_orchestrator_root_tool(&inv.name) {
            return settle(
                inv,
                false,
                "modo orquestrador: ferramenta exclusiva dos subagentes; delegue usando `agent`"
                    .into(),
            );
        }
        inv.args = match parse_args(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return settle(
                    inv,
                    false,
                    format!("erro: argumentos não são JSON válido: {e}"),
                )
            }
        };
        tools.coerce_args(&inv.name, &mut inv.args);

        // `mcp_call` naming a deferred tool IS that tool from here on: its
        // rules, hooks, approval card and "allow for the session" are the
        // real tool's, never a blanket grant on the meta-tool. Any other name
        // stays `mcp_call`, which refuses it.
        if inv.name == zerith_tools::DISPATCH_TOOL {
            let target = inv.args.get("tool").and_then(Value::as_str).filter(|n| {
                tools
                    .get(n)
                    .is_some_and(|s| s.source.starts_with(zerith_tools::DEFERRED_SOURCE))
            });
            if let Some(real) = target.map(str::to_string) {
                inv.args = match inv.args["arguments"].take() {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                    a => a,
                };
                inv.name = real;
            }
        }

        let mut ask: Option<String> = None;
        for m in self.middleware {
            match m.before(&mut inv) {
                Flow::Continue => {}
                Flow::Deny(why) => return settle(inv, false, format!("negado: {why}")),
                Flow::Replace(out) => return settle(inv, true, out),
                Flow::Ask(s) => {
                    ask.get_or_insert(s);
                }
            }
        }

        // The policy sees the arguments middleware left, not the model's.
        let cctx = self.ctx(policy);
        let (verdict, action) = match tools.authorize(&inv.name, &inv.args, &cctx) {
            Ok(v) => v,
            Err(e) => return settle(inv, false, format!("erro: {e}")),
        };
        let mut policy_ask = match verdict {
            Verdict::Allow => None,
            Verdict::Deny(why) => return settle(inv, false, format!("negado: {why}")),
            Verdict::Ask(why) => Some(why),
        };
        // The model is told to read its own spill files; asking the user
        // before every such read would defeat the point. Only reads, only
        // inside this session's spill directory.
        if let (Some(Action::Read(p)), Some(root)) = (&action, self.spill_dir) {
            if policy_ask.is_some() && spill::covers(&spill::session_dir(root, self.session_id), p)
            {
                policy_ask = None;
            }
        }

        let (mut need, net_grant) = match (policy_ask, ask) {
            // If middleware explicitly asked, keep `forced = true` even when
            // policy also asked, so a prior AllowSession cannot waive a hook.
            (Some(why), Some(_)) => (Some((why, true)), None),
            // Same for an `ask` rule the user wrote (`shell(ssh *)`: every
            // command on a production router is shown), and for any remote
            // access, whose blast radius is another machine.
            // ponytail: the rule is recognized by its message prefix; a
            // Verdict::AskRule variant if a second caller needs it.
            (Some(why), None) => {
                if let Some((risk, critical, grant_override)) =
                    classify_terminal_call(&inv.name, &inv.args)
                {
                    use zerith_tools::network::NetCmdRisk;
                    match risk {
                        NetCmdRisk::Dangerous => (
                            Some((format!("⚠️ COMANDO DESTRUTIVO DE REDE: {why}"), true)),
                            Some(grant_override),
                        ),
                        NetCmdRisk::Mutation => {
                            let label = if critical {
                                format!("[equipamento crítico] {why}")
                            } else {
                                why
                            };
                            (Some((label, true)), Some(grant_override))
                        }
                        NetCmdRisk::ReadOnly => {
                            let forced = why.starts_with("regra ");
                            (Some((why, forced)), Some(grant_override))
                        }
                    }
                } else {
                    let sensitive_boundary = match &action {
                        Some(Action::Write(path) | Action::Read(path)) => {
                            !policy.inside_workspace(path) || policy.is_own_control(path)
                        }
                        _ => false,
                    } || (inv.name == "web_fetch"
                        && inv
                            .args
                            .get("allow_private")
                            .and_then(Value::as_bool)
                            .unwrap_or(false));
                    let forced = why.starts_with("regra ")
                        || remote_access(&inv.name, &inv.args)
                        || sensitive_boundary;
                    let grant_override = sensitive_boundary.then(|| {
                        format!(
                            "{}:boundary",
                            crate::agent::approval_grant_key(&inv.name, &inv.args)
                        )
                    });
                    (Some((why, forced)), grant_override)
                }
            }
            (None, Some(s)) => (Some((s, true)), None),
            (None, None) => (None, None),
        };
        // Even in Bypass mode, dangerous network commands require explicit forced approval.
        if need.is_none() {
            if let Some((zerith_tools::network::NetCmdRisk::Dangerous, _, grant_override)) =
                classify_terminal_call(&inv.name, &inv.args)
            {
                let summary = inv
                    .args
                    .get("input")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        inv.args.get("commands").and_then(Value::as_array).map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                    })
                    .unwrap_or_else(|| inv.name.clone());
                need = Some((format!("⚠️ COMANDO DESTRUTIVO DE REDE: {summary}"), true));
                if let Some((why, forced)) = need {
                    if let Err(refusal) =
                        self.approve(&inv.name, &grant_override, why, forced, on_event)
                    {
                        return settle(inv, false, refusal);
                    }
                    return Prepared { inv, settled: None };
                }
            }
        }
        if let Some((why, forced)) = need {
            let grant_key =
                net_grant.unwrap_or_else(|| crate::agent::approval_grant_key(&inv.name, &inv.args));
            if let Err(refusal) = self.approve(&inv.name, &grant_key, why, forced, on_event) {
                return settle(inv, false, refusal);
            }
        }
        Prepared { inv, settled: None }
    }

    /// Blocks on the user. `forced` (a middleware ask) is not waived by an
    /// earlier "allow for the session": the hook asked on purpose.
    fn approve(
        &self,
        tool: &str,
        grant_key: &str,
        why: String,
        forced: bool,
        on_event: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let Some(gate) = self.approvals else {
            return Err(format!(
                "negado: precisa de aprovação ({why}) e não há quem aprove{}",
                denial_hint(tool, &why)
            ));
        };
        if !forced && gate.granted(grant_key) {
            return Ok(());
        }
        if self.cancel.is_cancelled() {
            return Err("cancelado".into());
        }
        // The root pumps while it waits: the slot may be held by a background
        // child whose prompt sits in `relayed`, which only the root emits.
        let relay = self.relay;
        let slot = gate.acquire_prompt_slot_pumping(self.cancel, &mut || {
            if relay {
                for e in gate.take_relayed() {
                    on_event(e);
                }
            }
        });
        let Some(_slot) = slot else {
            return Err("cancelado".into());
        };
        if !forced && gate.granted(grant_key) {
            return Ok(());
        }
        if self.cancel.is_cancelled() {
            return Err("cancelado".into());
        }
        let id = NEXT_APPROVAL.fetch_add(1, Ordering::Relaxed);
        if self.relay {
            on_event(RunEvent::ApprovalNeeded {
                id,
                tool: tool.to_string(),
                summary: why,
            });
        } else {
            // A subagent: the root turn emits it into the person's chat.
            gate.relay(RunEvent::ApprovalNeeded {
                id,
                tool: tool.to_string(),
                summary: format!("[subagente] {why}"),
            });
        }
        match gate.wait(id, self.cancel) {
            Approval::AllowOnce => Ok(()),
            Approval::AllowSession => {
                gate.grant(grant_key);
                Ok(())
            }
            Approval::Deny => {
                if self.cancel.is_cancelled() {
                    Err("cancelado".into())
                } else {
                    Err("negado pelo usuário".into())
                }
            }
        }
    }

    /// Phase 5: runs every call that is not settled. Safe calls run on up to
    /// `max_parallel` threads, but only when *all* of them are still safe
    /// after middleware; any doubt means serial, in order. Results come back
    /// indexed like `jobs`, so the caller commits them in the model's order.
    pub fn run_group(
        &self,
        jobs: &[Prepared],
        max_parallel: usize,
        on_event: &mut dyn FnMut(RunEvent),
    ) -> Vec<Option<(bool, String)>> {
        let mut out: Vec<Option<(bool, String)>> = vec![None; jobs.len()];
        let Some((tools, policy)) = self.core else {
            return out;
        };
        let todo: Vec<usize> = (0..jobs.len())
            .filter(|&i| jobs[i].settled.is_none())
            .collect();
        if todo.is_empty() {
            return out;
        }
        struct ClearLive(zerith_tools::EventSink);
        impl Drop for ClearLive {
            fn drop(&mut self) {
                self.0.set_live(None);
            }
        }
        let (live_tx, live_rx) = mpsc::sync_channel::<HostEvent>(256);
        let sink = tools.host().sink();
        sink.set_live(Some(Arc::new(move |ev| {
            let _ = live_tx.try_send(ev);
        })));
        let _guard = ClearLive(sink);

        let all_safe = todo
            .iter()
            .all(|&i| tools.concurrency_safe(&jobs[i].inv.name, &jobs[i].inv.args));
        if max_parallel <= 1 || todo.len() < 2 || !all_safe {
            let cancel = self.cancel;
            for i in todo {
                if cancel.is_cancelled() {
                    break;
                }
                let inv = &jobs[i].inv;
                out[i] = Some(if tools.uses_memory(&inv.name) {
                    self.run_one(tools, policy, inv)
                } else {
                    self.pumped_live(on_event, &live_rx, || run_plain(tools, policy, inv, cancel))
                });
            }
            Self::drain_live(&live_rx, on_event);
            return out;
        }

        // A memory backend is not `Sync`: its calls stay on this thread.
        let (mine, pool): (Vec<usize>, Vec<usize>) = todo
            .into_iter()
            .partition(|&i| tools.uses_memory(&jobs[i].inv.name));
        let slots: Mutex<&mut Vec<Option<(bool, String)>>> = Mutex::new(&mut out);
        let next = AtomicUsize::new(0);
        let finished = AtomicUsize::new(0);
        let cancel = self.cancel;
        let workers = max_parallel.min(pool.len());
        struct FinishOnDrop<'a>(&'a AtomicUsize);
        impl Drop for FinishOnDrop<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&i) = pool.get(k) else { break };
                    let _done = FinishOnDrop(&finished);
                    if cancel.is_cancelled() {
                        continue;
                    }
                    let r = run_plain(tools, policy, &jobs[i].inv, cancel);
                    if let Ok(mut g) = slots.lock() {
                        g[i] = Some(r);
                    }
                });
            }
            for &i in &mine {
                if cancel.is_cancelled() {
                    break;
                }
                let r = self.run_one(tools, policy, &jobs[i].inv);
                if let Ok(mut g) = slots.lock() {
                    g[i] = Some(r);
                }
            }
            while finished.load(Ordering::SeqCst) < pool.len() {
                if self.relays() {
                    self.pump(on_event);
                }
                Self::drain_live(&live_rx, on_event);
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        if self.relays() {
            self.pump(on_event);
        }
        Self::drain_live(&live_rx, on_event);
        out
    }

    fn drain_live(live_rx: &mpsc::Receiver<HostEvent>, on_event: &mut dyn FnMut(RunEvent)) {
        while let Ok(ev) = live_rx.try_recv() {
            if let HostEvent::ToolProgress { tool, chunk } = ev {
                on_event(RunEvent::ToolProgress { tool, chunk });
            }
        }
    }

    fn relays(&self) -> bool {
        self.relay && self.approvals.is_some()
    }

    /// Emits what subagents queued on the shared gate (their approval
    /// prompts): the person answers them in the parent's chat.
    fn pump(&self, on_event: &mut dyn FnMut(RunEvent)) {
        if let (true, Some(g)) = (self.relay, self.approvals) {
            for e in g.take_relayed() {
                on_event(e);
            }
        }
    }

    /// Runs `job` on a scoped thread while pumping subagent prompts and live tool progress.
    fn pumped_live<R: Send>(
        &self,
        on_event: &mut dyn FnMut(RunEvent),
        live_rx: &mpsc::Receiver<HostEvent>,
        job: impl FnOnce() -> R + Send,
    ) -> R {
        std::thread::scope(|s| {
            let h = s.spawn(job);
            for _ in 0..4 {
                if h.is_finished() {
                    break;
                }
                std::thread::yield_now();
            }
            while !h.is_finished() {
                if self.relays() {
                    self.pump(on_event);
                }
                Self::drain_live(live_rx, on_event);
                if let Ok(HostEvent::ToolProgress { tool, chunk }) =
                    live_rx.recv_timeout(Duration::from_millis(15))
                {
                    on_event(RunEvent::ToolProgress { tool, chunk });
                }
            }
            if self.relays() {
                self.pump(on_event);
            }
            Self::drain_live(live_rx, on_event);
            h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
        })
    }

    fn run_one(&self, tools: &Registry, policy: &Policy, inv: &ToolInvocation) -> (bool, String) {
        run_caught(|| tools.run_raw(&inv.name, &inv.args, &self.ctx(policy), self.cancel))
    }

    /// Phases 6-7: after-middleware, then spill/truncate. Returns the outcome
    /// and the spill file, if one was written.
    pub fn finish(
        &self,
        p: Prepared,
        ran: Option<(bool, String)>,
    ) -> (ToolOutcome, Option<PathBuf>) {
        let mut outcome = p.settled.unwrap_or_else(|| {
            let (ok, text) = ran.unwrap_or((false, "erro: a chamada não foi executada".into()));
            ToolOutcome::new(ok, text)
        });
        for m in self.middleware {
            m.after(&p.inv, &mut outcome);
        }
        // A `show run`, an `/export` or a `.env` read carries passwords,
        // communities and keys: they never reach the model, the history or
        // the spill file. After the hooks, so a hook still sees the raw text.
        // Idea from Hermes Agent's terminal-output redaction (MIT); the rules
        // are Zerith's own `redact`.
        if redacts_output(&p.inv.name) {
            outcome.output = zerith_security::redact(&outcome.output);
        }
        let spilled = self.finalize(&p.inv.id, &mut outcome.output);
        (outcome, spilled)
    }

    /// An output over the cap goes to a file when there is a place for it, and
    /// is cut otherwise; either way the model is told how to get the rest.
    fn finalize(&self, call_id: &str, output: &mut String) -> Option<PathBuf> {
        if output.len() <= OUTPUT_CAP_CHARS || output.chars().count() <= OUTPUT_CAP_CHARS {
            return None;
        }
        let full = std::mem::take(output);
        if let Some(root) = self.spill_dir {
            if let Ok((path, shown)) = spill::spill(root, self.session_id, call_id, &full) {
                *output = shown;
                return Some(path);
            }
        }
        *output = zerith_tools::truncate(full, OUTPUT_CAP_CHARS);
        None
    }
}

/// Tools whose output can carry device or service secrets. Edits are left
/// out on purpose: the model must see what it wrote to edit it again, and it
/// wrote that text itself.
fn redacts_output(tool: &str) -> bool {
    !matches!(
        tool,
        "write_file" | "edit_file" | "multi_edit" | "task_create" | "task_update" | "task_list"
    )
}

/// Classifies commands sent to an active remote/network terminal session.
/// Returns `Some((worst_risk, is_critical_device, grant_key))` when the call
/// targets a remote or vendor-tagged terminal session.
fn classify_terminal_call(
    tool: &str,
    args: &Value,
) -> Option<(zerith_tools::network::NetCmdRisk, bool, String)> {
    use zerith_tools::network::{classify_network_command, DeviceRole, NetCmdRisk, Vendor};
    if !matches!(tool, "terminal_send" | "terminal_batch") {
        return None;
    }
    let id = args.get("id").and_then(Value::as_str).unwrap_or("");
    let meta = zerith_tools::terminal_meta(id);
    let has_vendor = meta.as_ref().and_then(|m| m.0.as_ref()).is_some();
    let is_remote = has_vendor || remote_access(tool, args);
    if !is_remote {
        return None;
    }
    if tool == "terminal_send" && args.get("secret").and_then(Value::as_str).is_some() {
        return Some((
            NetCmdRisk::Mutation,
            true,
            format!("terminal_net:secret:{id}"),
        ));
    }
    let cmds: Vec<&str> = match tool {
        "terminal_send" => args
            .get("input")
            .and_then(Value::as_str)
            .map(|s| vec![s])
            .unwrap_or_default(),
        "terminal_batch" => args
            .get("commands")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    if cmds.is_empty() {
        return None;
    }
    let vendor = meta
        .as_ref()
        .and_then(|m| m.0.as_deref())
        .and_then(Vendor::parse)
        .unwrap_or(Vendor::Generic);
    let critical = meta
        .as_ref()
        .and_then(|m| m.2.as_deref())
        .and_then(DeviceRole::parse)
        .is_some_and(DeviceRole::is_critical);
    let mut worst = NetCmdRisk::ReadOnly;
    for cmd in cmds {
        let r = classify_network_command(vendor, cmd);
        if r > worst {
            worst = r;
        }
    }
    let grant_key = match worst {
        NetCmdRisk::ReadOnly => format!("terminal_net:readonly:{id}"),
        NetCmdRisk::Mutation => format!("terminal_net:mutation:{id}"),
        NetCmdRisk::Dangerous => format!("terminal_net:dangerous:{id}"),
    };
    Some((worst, critical, grant_key))
}

/// A shell command that reaches another machine's prompt (SSH, Telnet, serial
/// tools). Each one is approved on its own, never by a session grant.
fn remote_access(tool: &str, args: &Value) -> bool {
    if tool == "net_connect" {
        return true;
    }
    const REMOTE: &[&str] = &[
        "ssh", "plink", "telnet", "sshpass", "scp", "sftp", "putty", "kitty",
    ];
    let owned;
    let cmd = match tool {
        "shell" | "monitor" | "terminal_open" => args["command"].as_str().unwrap_or(""),
        // Every line typed into an ssh/telnet session reaches the router.
        "terminal_send" | "terminal_batch" => {
            owned = zerith_tools::terminal_program(args["id"].as_str().unwrap_or(""))
                .unwrap_or_default();
            &owned
        }
        _ => return false,
    };
    let a = zerith_core::shellparse::analyze(cmd);
    a.raw.iter().chain(&a.inner).any(|piece| {
        let prog = piece.split_whitespace().next().unwrap_or("");
        let name = prog
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(prog)
            .to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        REMOTE.contains(&name)
    })
}

fn parse_args(raw: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw })
}

/// A call without memory, for worker threads.
fn run_plain(
    tools: &Registry,
    policy: &Policy,
    inv: &ToolInvocation,
    cancel: &Cancel,
) -> (bool, String) {
    let ctx = CallCtx {
        workspace: &policy.workspace,
        policy,
        memory: None,
        scope: None,
    };
    run_caught(|| tools.run_raw(&inv.name, &inv.args, &ctx, cancel))
}

/// A tool that panics fails its call, not the turn: an unwinding turn would
/// skip abandoning its subagents and writing `turn/end`.
fn run_caught(run: impl FnOnce() -> zerith_tools::Result<String>) -> (bool, String) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(o)) => (true, o),
        Ok(Err(e)) => (false, format!("erro: {e}")),
        Err(_) => (false, PANICKED.into()),
    }
}

/// What to do instead of a refused shell line that only writes, reads or
/// deletes a file. In the benchmarks the model kept retrying `cat > f <<EOF`
/// and `cat -n f` through the shell, each one another denied step, while
/// `write_file`/`read_file` were already allowed.
fn denial_hint(tool: &str, why: &str) -> &'static str {
    if tool != "shell" {
        return "";
    }
    let cmd = why.strip_prefix("shell: ").unwrap_or(why);
    let writes =
        cmd.contains("<<") || cmd.contains("> ") || cmd.contains(">>") || cmd.contains("tee ");
    // `for f in a b; do cat -n $f; done` reads several files: the loop is what
    // got it refused, and read_file can be called once per file instead.
    let reads = ["cat ", "cat -n", "head ", "tail ", "type ", "Get-Content"]
        .iter()
        .any(|p| {
            cmd.trim_start().starts_with(p)
                || cmd.contains(&format!("; {p}"))
                || cmd.contains(&format!("do {p}"))
                || cmd.contains(&format!("&& {p}"))
        });
    let deletes = ["rm ", "del ", "Remove-Item"]
        .iter()
        .any(|p| cmd.contains(p));
    if writes {
        ". Para criar ou mudar arquivo, use write_file, edit_file ou multi_edit em vez do shell."
    } else if deletes {
        ". Não apague pelo shell: deixe o arquivo ou diga ao usuário para apagar."
    } else if reads {
        ". Para ler arquivo, use read_file, glob ou grep em vez do shell."
    } else {
        ". Não tente outra forma do mesmo comando: use as ferramentas liberadas ou diga ao usuário o que falta."
    }
}

#[cfg(test)]
mod denial_hint_tests {
    use super::denial_hint;

    #[test]
    fn a_refused_file_command_points_at_the_file_tool() {
        assert!(denial_hint("shell", "shell: cat > a.py <<'EOF'\nx\nEOF").contains("write_file"));
        assert!(denial_hint("shell", "shell: cat -n billing/money.py").contains("read_file"));
        // a loop over files is still just reading them
        assert!(denial_hint(
            "shell",
            "shell: for f in README.md pve1/etc/pve/corosync.conf; do cat -n $f; done"
        )
        .contains("read_file"));
        assert!(denial_hint("shell", "shell: rm _scratch.py; ls").contains("Não apague"));
        assert!(denial_hint("shell", "shell: curl http://x").contains("Não tente outra forma"));
        assert_eq!(denial_hint("write_file", "escrever /x"), "");
    }
}

#[cfg(test)]
mod remote_tests {
    use super::remote_access;
    use serde_json::json;

    #[test]
    fn every_way_of_reaching_a_router_is_remote_and_local_commands_are_not() {
        let sh = |c: &str| remote_access("shell", &json!({ "command": c }));
        assert!(sh("ssh admin@10.0.0.1 /system resource print"));
        assert!(sh("C:/Windows/System32/OpenSSH/ssh.exe -V"));
        assert!(sh("cd x && plink -batch -pw y host < cmds.txt"));
        assert!(sh("telnet 10.248.20.2"));
        assert!(!sh("cargo test -q"));
        assert!(!sh("git status"));
        assert!(!remote_access("read_file", &json!({ "path": "ssh" })));
        assert!(remote_access("net_connect", &json!({ "device": "core-1" })));
        // A monitor is a shell command too: `ssh` there is just as remote.
        assert!(remote_access(
            "monitor",
            &json!({ "command": "ssh admin@10.0.0.1 ping 8.8.8.8" })
        ));
        assert!(!remote_access(
            "monitor",
            &json!({ "command": "tail -f app.log" })
        ));
    }
}

#[cfg(test)]
mod parallel_panic_tests {
    use super::*;
    use zerith_security::{preset, Mode};
    use zerith_tools::{Exec, Permission, Tool, ToolSpec};

    struct MockTool {
        spec: ToolSpec,
        panic: bool,
    }

    impl Tool for MockTool {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        fn action(
            &self,
            _args: &Value,
            _ctx: &CallCtx<'_>,
        ) -> zerith_tools::Result<Option<Action>> {
            Ok(None)
        }

        fn run(
            &self,
            _args: &Value,
            _ctx: &CallCtx<'_>,
            _exec: &Exec<'_>,
        ) -> zerith_tools::Result<String> {
            if self.panic {
                panic!("boom in parallel tool");
            }
            Ok("ok".into())
        }

        fn concurrency_safe(&self, _args: &Value) -> bool {
            true
        }
    }

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: "mock".into(),
            parameters: serde_json::json!({"type": "object"}),
            returns: "text",
            permission: Permission::Read,
            timeout: Duration::from_secs(5),
            cancellable: false,
            local: true,
            source: "built-in".into(),
        }
    }

    #[test]
    fn parallel_tool_panic_does_not_deadlock_run_group() {
        let mut reg = Registry::empty();
        reg.register(Box::new(MockTool {
            spec: spec("panicker"),
            panic: true,
        }));
        reg.register(Box::new(MockTool {
            spec: spec("healthy"),
            panic: false,
        }));
        let policy = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Bypass,
            Path::new("."),
        );
        let cancel = Cancel::default();
        let pipe = Pipeline {
            core: Some((&reg, &policy)),
            memory: None,
            scope: None,
            middleware: &[],
            approvals: None,
            relay: false,
            spill_dir: None,
            session_id: "s1",
            cancel: &cancel,
            orchestrator_root: false,
        };
        let calls = [
            ToolCall {
                id: "1".into(),
                name: "panicker".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "2".into(),
                name: "healthy".into(),
                arguments: "{}".into(),
            },
        ];
        let prepared: Vec<Prepared> = calls.iter().map(|c| pipe.prepare(c, &mut |_| {})).collect();
        let expected = vec![
            Some((false, "erro interno na ferramenta (panic)".into())),
            Some((true, "ok".into())),
        ];
        assert_eq!(pipe.run_group(&prepared, 2, &mut |_| {}), expected);
        // Serial path too: a panic fails the call, it never unwinds the turn.
        assert_eq!(pipe.run_group(&prepared, 1, &mut |_| {}), expected);
    }

    #[test]
    fn allow_session_never_bypasses_sensitive_boundaries_or_private_web_fetch() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let ws = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let outside = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        std::fs::create_dir_all(ws.path().join(".zerith")).unwrap();

        let reg = Registry::new();
        let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Default, ws.path());
        let cancel = Cancel::default();

        // Scenario 1: AllowSession granted on an in-workspace write must NOT waive
        // outside-workspace writes or control files (`.zerith/`, `.git/hooks/`).
        let (tx, gate) = crate::agent::approval_channel();
        let pipe = Pipeline {
            core: Some((&reg, &policy)),
            memory: None,
            scope: None,
            middleware: &[],
            approvals: Some(&gate),
            relay: true,
            spill_dir: None,
            session_id: "s-bound",
            cancel: &cancel,
            orchestrator_root: false,
        };
        let in_ws_call = ToolCall {
            id: "w1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "ok.txt", "content": "hi"}).to_string(),
        };
        let prep1 = pipe.prepare(&in_ws_call, &mut |ev| {
            if let RunEvent::ApprovalNeeded { id, .. } = ev {
                assert!(tx.answer(id, Approval::AllowSession));
            }
        });
        assert!(prep1.settled.is_none());

        // Another in-workspace write uses the AllowSession grant without prompting.
        let in_ws_call2 = ToolCall {
            id: "w2".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "ok2.txt", "content": "hi"}).to_string(),
        };
        assert!(pipe.prepare(&in_ws_call2, &mut |_| {}).settled.is_none());

        // Outside-workspace and control-file writes STILL prompt even with AllowSession on write_file.
        for sensitive_path in [
            outside
                .path()
                .join("pwn.txt")
                .to_string_lossy()
                .into_owned(),
            ".zerith/settings.toml".into(),
            ".git/hooks/pre-commit".into(),
        ] {
            let (tx_d, gate_d) = crate::agent::approval_channel();
            gate_d.grant("write_file");
            let pipe_d = Pipeline {
                approvals: Some(&gate_d),
                ..pipe
            };
            let call = ToolCall {
                id: "wsens".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": sensitive_path, "content": "x"}).to_string(),
            };
            let mut prompted = false;
            let prep = pipe_d.prepare(&call, &mut |ev| {
                if let RunEvent::ApprovalNeeded { id, .. } = ev {
                    prompted = true;
                    assert!(tx_d.answer(id, Approval::Deny));
                }
            });
            assert!(
                prompted,
                "sensitive path `{sensitive_path}` must prompt despite AllowSession on write_file"
            );
            assert!(
                prep.settled.is_some_and(|s| !s.ok),
                "denying prompt must settle `{sensitive_path}` as refused"
            );
        }

        // Scenario 2: AllowSession on public web_fetch must NOT auto-approve allow_private: true.
        let (tx_w, gate_w) = crate::agent::approval_channel();
        gate_w.grant(&crate::agent::approval_grant_key(
            "web_fetch",
            &serde_json::json!({"url": "https://example.com"}),
        ));
        let pipe_w = Pipeline {
            approvals: Some(&gate_w),
            ..pipe
        };
        let pub_call = ToolCall {
            id: "wf1".into(),
            name: "web_fetch".into(),
            arguments: serde_json::json!({"url": "https://example.org"}).to_string(),
        };
        assert!(pipe_w.prepare(&pub_call, &mut |_| {}).settled.is_none());

        let priv_call = ToolCall {
            id: "wf2".into(),
            name: "web_fetch".into(),
            arguments: serde_json::json!({"url": "http://127.0.0.1:8080", "allow_private": true})
                .to_string(),
        };
        let mut prompted_priv = false;
        let prep_priv = pipe_w.prepare(&priv_call, &mut |ev| {
            if let RunEvent::ApprovalNeeded { id, .. } = ev {
                prompted_priv = true;
                assert!(tx_w.answer(id, Approval::Deny));
            }
        });
        assert!(prompted_priv);
        assert!(prep_priv.settled.is_some_and(|s| !s.ok));
    }

    #[test]
    fn concurrent_approvals_serialize_and_share_allow_session_grant() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let ws = tempfile::Builder::new().tempdir_in(&scratch).unwrap();

        let reg = Registry::new();
        let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Default, ws.path());
        let cancel = Cancel::default();
        let (tx, gate) = crate::agent::approval_channel();
        let prompt_count = Arc::new(AtomicUsize::new(0));
        let (first_seen_tx, first_seen_rx) = mpsc::channel::<u64>();
        let (second_entered_tx, second_entered_rx) = mpsc::channel::<()>();

        std::thread::scope(|s| {
            let reg_ref = &reg;
            let pol_ref = &policy;
            let cancel_ref = &cancel;
            let g1 = gate.clone();
            let cnt1 = Arc::clone(&prompt_count);
            let h1 = s.spawn(move || {
                let p1 = Pipeline {
                    core: Some((reg_ref, pol_ref)),
                    memory: None,
                    scope: None,
                    middleware: &[],
                    approvals: Some(&g1),
                    relay: true,
                    spill_dir: None,
                    session_id: "s-serial",
                    cancel: cancel_ref,
                    orchestrator_root: false,
                };
                let c1 = ToolCall {
                    id: "w1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "a.txt", "content": "1"}).to_string(),
                };
                p1.prepare(&c1, &mut |ev| {
                    if let RunEvent::ApprovalNeeded { id, .. } = ev {
                        cnt1.fetch_add(1, Ordering::SeqCst);
                        let _ = first_seen_tx.send(id);
                    }
                })
            });

            let first_id = first_seen_rx.recv().unwrap();

            let g2 = gate.clone();
            let cnt2 = Arc::clone(&prompt_count);
            let h2 = s.spawn(move || {
                let p2 = Pipeline {
                    core: Some((reg_ref, pol_ref)),
                    memory: None,
                    scope: None,
                    middleware: &[],
                    approvals: Some(&g2),
                    relay: false,
                    spill_dir: None,
                    session_id: "s-serial",
                    cancel: cancel_ref,
                    orchestrator_root: false,
                };
                let _ = second_entered_tx.send(());
                let c2 = ToolCall {
                    id: "w2".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "b.txt", "content": "2"}).to_string(),
                };
                p2.prepare(&c2, &mut |ev| {
                    if let RunEvent::ApprovalNeeded { .. } = ev {
                        cnt2.fetch_add(1, Ordering::SeqCst);
                    }
                })
            });

            second_entered_rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(80));
            assert_eq!(
                prompt_count.load(Ordering::SeqCst),
                1,
                "second approval must block on prompt slot before emitting"
            );
            assert!(gate.take_relayed().is_empty());

            // Grant AllowSession for #1 -> #2 must acquire slot, see grant, and return without prompting.
            assert!(tx.answer(first_id, Approval::AllowSession));
            assert!(h1.join().unwrap().settled.is_none());
            assert!(h2.join().unwrap().settled.is_none());
            assert_eq!(prompt_count.load(Ordering::SeqCst), 1);
            assert!(gate.take_relayed().is_empty());
        });

        // Forced approvals still prompt even when the tool has an AllowSession grant.
        let p_forced = Pipeline {
            core: Some((&reg, &policy)),
            memory: None,
            scope: None,
            middleware: &[],
            approvals: Some(&gate),
            relay: true,
            spill_dir: None,
            session_id: "s-serial",
            cancel: &cancel,
            orchestrator_root: false,
        };
        let mut forced_prompted = false;
        let res = p_forced.approve(
            "write_file",
            "write_file",
            "regra explícita".into(),
            true,
            &mut |ev| {
                if let RunEvent::ApprovalNeeded { id, .. } = ev {
                    forced_prompted = true;
                    assert!(tx.answer(id, Approval::AllowOnce));
                }
            },
        );
        assert!(forced_prompted);
        assert!(res.is_ok());

        // A background child holds the slot and its prompt sits in `relayed`:
        // the root waiting for the slot must emit it, or nobody ever answers.
        {
            let held = gate.acquire_prompt_slot(&Cancel::default()).unwrap();
            gate.relay(RunEvent::ApprovalNeeded {
                id: 999_999,
                tool: "shell".into(),
                summary: "[subagente] x".into(),
            });
            let held = std::sync::Mutex::new(Some(held));
            let mut seen = false;
            let got = p_forced.approve("write_file", "k-pump", "w".into(), true, &mut |ev| {
                if let RunEvent::ApprovalNeeded { id, .. } = ev {
                    if id == 999_999 {
                        seen = true;
                        held.lock().unwrap().take(); // child answered: frees the slot
                    } else {
                        assert!(tx.answer(id, Approval::AllowOnce));
                    }
                }
            });
            assert!(seen, "relayed child prompt must be pumped while waiting");
            assert!(got.is_ok());
        }

        // Cancel unblocks a waiting prompt slot immediately with None.
        let c_wait = Cancel::default();
        let _held = gate.acquire_prompt_slot(&Cancel::default()).unwrap();
        let c_wait2 = c_wait.clone();
        std::thread::scope(|s| {
            let g_wait = gate.clone();
            let h = s.spawn(move || g_wait.acquire_prompt_slot(&c_wait2).is_none());
            std::thread::sleep(Duration::from_millis(40));
            c_wait.cancel();
            assert!(h.join().unwrap());
        });
    }

    #[test]
    fn orchestrator_root_denies_execution_tools_and_allows_brain_tools() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let ws = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let reg = Registry::new();
        let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, ws.path());
        let cancel = Cancel::default();
        let pipe = Pipeline {
            core: Some((&reg, &policy)),
            memory: None,
            scope: None,
            middleware: &[],
            approvals: None,
            relay: true,
            spill_dir: None,
            session_id: "s-orch",
            cancel: &cancel,
            orchestrator_root: true,
        };

        for blocked in ["shell", "write_file", "edit_file"] {
            let prep = pipe.prepare(
                &ToolCall {
                    id: format!("id-{blocked}"),
                    name: blocked.into(),
                    arguments: "{}".into(),
                },
                &mut |_| {},
            );
            let settled = prep.settled.expect("execution tool must be denied");
            assert!(!settled.ok);
            assert_eq!(
                settled.output,
                "modo orquestrador: ferramenta exclusiva dos subagentes; delegue usando `agent`"
            );
        }

        let allowed = pipe.prepare(
            &ToolCall {
                id: "id-read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "Cargo.toml"}).to_string(),
            },
            &mut |_| {},
        );
        assert!(allowed.settled.is_none(), "read_file must stay allowed");
    }
}
