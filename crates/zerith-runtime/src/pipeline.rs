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
use std::sync::Mutex;

use serde_json::Value;
use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{Cancel, ToolCall};
use zerith_security::{Action, Policy, Verdict};
use zerith_tools::{CallCtx, Registry, OUTPUT_CAP_CHARS};

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

        let need = match (policy_ask, ask) {
            (Some(why), _) => Some((why, false)),
            (None, Some(s)) => Some((s, true)),
            (None, None) => None,
        };
        if let Some((why, forced)) = need {
            if let Err(refusal) = self.approve(&inv.name, why, forced, on_event) {
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
        why: String,
        forced: bool,
        on_event: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let Some(gate) = self.approvals else {
            return Err(format!(
                "negado: precisa de aprovação ({why}) e não há quem aprove"
            ));
        };
        if !forced && gate.granted(tool) {
            return Ok(());
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
                gate.grant(tool);
                Ok(())
            }
            Approval::Deny => Err("negado pelo usuário".into()),
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
        let all_safe = todo
            .iter()
            .all(|&i| tools.concurrency_safe(&jobs[i].inv.name, &jobs[i].inv.args));
        if max_parallel <= 1 || todo.len() < 2 || !all_safe {
            for i in todo {
                let inv = &jobs[i].inv;
                out[i] = Some(if self.relays() && inv.name == "agent" {
                    let cancel = self.cancel;
                    self.pumped(on_event, || run_plain(tools, policy, inv, cancel))
                } else {
                    self.run_one(tools, policy, inv)
                });
            }
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
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&i) = pool.get(k) else { break };
                    let r = run_plain(tools, policy, &jobs[i].inv, cancel);
                    if let Ok(mut g) = slots.lock() {
                        g[i] = Some(r);
                    }
                    finished.fetch_add(1, Ordering::SeqCst);
                });
            }
            for &i in &mine {
                let r = self.run_one(tools, policy, &jobs[i].inv);
                if let Ok(mut g) = slots.lock() {
                    g[i] = Some(r);
                }
            }
            while self.relays() && finished.load(Ordering::SeqCst) < pool.len() {
                self.pump(on_event);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        self.pump(on_event);
        out
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

    /// Runs `job` on a scoped thread while this one pumps subagent prompts.
    fn pumped<R: Send>(
        &self,
        on_event: &mut dyn FnMut(RunEvent),
        job: impl FnOnce() -> R + Send,
    ) -> R {
        std::thread::scope(|s| {
            let h = s.spawn(job);
            while !h.is_finished() {
                self.pump(on_event);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            self.pump(on_event);
            h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
        })
    }

    fn run_one(&self, tools: &Registry, policy: &Policy, inv: &ToolInvocation) -> (bool, String) {
        match tools.run_raw(&inv.name, &inv.args, &self.ctx(policy), self.cancel) {
            Ok(o) => (true, o),
            Err(e) => (false, format!("erro: {e}")),
        }
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
    match tools.run_raw(&inv.name, &inv.args, &ctx, cancel) {
        Ok(o) => (true, o),
        Err(e) => (false, format!("erro: {e}")),
    }
}
