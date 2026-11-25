//! Permission rules, hooks and the plan gate, as pipeline middleware.
//!
//! Thin adapter: the logic lives in `zerith_security` (policy, rules, modes)
//! and `zerith_core::hooks`. This module only translates between them and
//! [`Middleware`].
//!
//! ```text
//! let mut policy = Policy::from_settings(&settings, &workspace)?;
//! let guards = Guards::new(settings, &mut policy, session_id);
//! registry.register(guards.exit_plan_mode_tool());   // + enter_plan_mode_tool()
//! options.middleware.push(Box::new(guards));         // `policy` is the one in AgentOptions
//! ```
//!
//! What it does per call (`before`):
//! 1. `exit_plan_mode`: only valid in `plan`; asks the user to approve the plan.
//! 2. Tools the policy cannot map to an action (MCP, skills, plugins): rules
//!    `mcp__server__*` / `skill(name)`; an `mcp__*` tool with no rule asks
//!    (denied in `dontAsk`, allowed in `bypassPermissions`).
//! 3. `PreToolUse` hooks: deny, ask, rewrite arguments, inject context. New
//!    arguments go through step 2 again here, and through the policy and the
//!    approval in the pipeline; the model is told they changed.
//! 4. `PermissionRequest` hooks, when the call is about to ask the user: a
//!    hook may deny it. (A hook cannot *approve*: `Middleware` can only make a
//!    call stricter, so the prompt still appears.)
//!
//! and after it: `PostToolUse` or `PostToolUseFailure` hooks, whose context is
//! handed to the model.
//!
//! Hooks fail open: a crash or timeout is recorded (see [`Guards::take_errors`])
//! and the call proceeds. Only exit 2 or an explicit deny blocks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_core::hooks::{run_hooks, HookEvent, HookOutcome, HookPayload};
use zerith_core::settings::{Decision, Settings};
use zerith_security::{Mode, ModeHandle, Policy, Verdict};
use zerith_tools::{CallCtx, Exec, Permission, Tool, ToolError, ToolSpec};

use crate::pipeline::{Flow, Middleware, ToolInvocation, ToolOutcome};

pub const EXIT_PLAN_MODE: &str = "exit_plan_mode";
pub const ENTER_PLAN_MODE: &str = "enter_plan_mode";

/// Tools that are Zerith's own and need no rule.
const INTERNAL: &[&str] = &[EXIT_PLAN_MODE, ENTER_PLAN_MODE, "memory_recall"];

/// The plan gate: remembers the mode to return to and switches the shared
/// [`ModeHandle`]. Clones share state.
#[derive(Debug, Clone)]
pub struct PlanGate {
    mode: ModeHandle,
    previous: Arc<Mutex<Option<Mode>>>,
}

impl PlanGate {
    pub fn new(mode: ModeHandle) -> Self {
        Self {
            mode,
            previous: Arc::default(),
        }
    }

    pub fn in_plan(&self) -> bool {
        self.mode.get() == Mode::Plan
    }

    /// Enters `plan`, remembering where to go back to. Already in plan: no-op.
    pub fn enter(&self) {
        let now = self.mode.get();
        if now != Mode::Plan {
            if let Ok(mut p) = self.previous.lock() {
                *p = Some(now);
            }
            self.mode.set(Mode::Plan);
        }
    }

    /// Leaves `plan` after approval: back to the mode before it, or
    /// `acceptEdits` when plan was the starting mode (or the UI switched to it).
    /// Returns the new mode. Outside plan it changes nothing.
    pub fn approve(&self) -> Mode {
        if !self.in_plan() {
            return self.mode.get();
        }
        let back = self
            .previous
            .lock()
            .ok()
            .and_then(|mut p| p.take())
            .filter(|m| *m != Mode::Plan)
            .unwrap_or(Mode::AcceptEdits);
        self.mode.set(back);
        back
    }
}

/// `exit_plan_mode{plan}`: the model proposes a plan; the user approves it and
/// the session leaves `plan`. Register the tool *and* add [`Guards`] as
/// middleware: the tool alone would run without asking.
pub fn exit_plan_mode_spec() -> ToolSpec {
    spec(
        EXIT_PLAN_MODE,
        "Use no modo plan quando o plano estiver pronto. Mostra o plano ao usuário e, se \
         aprovado, libera edições e comandos. Não use para perguntas: pergunte direto.",
        json!({"type": "object", "properties": {
            "plan": {"type": "string", "description": "o plano completo, em Markdown"}
        }, "required": ["plan"]}),
    )
}

pub fn enter_plan_mode_spec() -> ToolSpec {
    spec(
        ENTER_PLAN_MODE,
        "Entra no modo plan: só leitura até o usuário aprovar um plano com exit_plan_mode. \
         Use antes de mudanças grandes ou ambíguas.",
        json!({"type": "object", "properties": {}}),
    )
}

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
        returns: "confirmação da troca de modo",
        permission: Permission::Read,
        timeout: Duration::from_secs(5),
        cancellable: false,
        local: true,
        source: "built-in".into(),
    }
}

struct PlanTool {
    spec: ToolSpec,
    gate: PlanGate,
    exit: bool,
}

impl Tool for PlanTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    // Never an Action: the approval comes from `Guards`, not from the policy.
    fn action(
        &self,
        _args: &Value,
        _ctx: &CallCtx<'_>,
    ) -> zerith_tools::Result<Option<zerith_security::Action>> {
        Ok(None)
    }

    fn run(
        &self,
        _args: &Value,
        _ctx: &CallCtx<'_>,
        _exec: &Exec<'_>,
    ) -> zerith_tools::Result<String> {
        if self.exit {
            if !self.gate.in_plan() {
                return Err(ToolError::Failed("não está em modo plan".into()));
            }
            let m = self.gate.approve();
            Ok(format!(
                "plano aprovado; modo agora {}. Execute o plano.",
                m.settings_name()
            ))
        } else {
            self.gate.enter();
            Ok("modo plan ativo: somente leitura. Chame exit_plan_mode com o plano.".into())
        }
    }
}

/// The middleware. Build one per session.
pub struct Guards {
    settings: Settings,
    policy: Policy,
    gate: PlanGate,
    session_id: String,
    errors: Mutex<Vec<String>>,
    /// Hook context from `before`, delivered by `after`, keyed by session and
    /// call id: subagents share this instance and their ids may repeat.
    pending: Mutex<HashMap<(String, String), Vec<String>>>,
}

impl Guards {
    /// `policy` must be the one handed to the agent loop: it gets a live
    /// [`ModeHandle`] here (if it has none) so the plan gate and the UI can
    /// switch modes for both.
    pub fn new(settings: Settings, policy: &mut Policy, session_id: impl Into<String>) -> Self {
        let handle = policy.mode_handle();
        Self {
            settings,
            policy: policy.clone(),
            gate: PlanGate::new(handle),
            session_id: session_id.into(),
            errors: Mutex::default(),
            pending: Mutex::default(),
        }
    }

    pub fn plan_gate(&self) -> PlanGate {
        self.gate.clone()
    }

    pub fn mode_handle(&self) -> ModeHandle {
        self.gate.mode.clone()
    }

    /// Tool to register in the `Registry` (the spec the model sees).
    pub fn exit_plan_mode_tool(&self) -> Box<dyn Tool> {
        Box::new(PlanTool {
            spec: exit_plan_mode_spec(),
            gate: self.gate.clone(),
            exit: true,
        })
    }

    pub fn enter_plan_mode_tool(&self) -> Box<dyn Tool> {
        Box::new(PlanTool {
            spec: enter_plan_mode_spec(),
            gate: self.gate.clone(),
            exit: false,
        })
    }

    /// Hook failures since the last call (crash, timeout, bad exit): log them.
    pub fn take_errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    fn hooks(&self, event: HookEvent, call: &ToolInvocation, output: Option<&str>) -> HookOutcome {
        let payload = HookPayload {
            session_id: self.session_id.clone(),
            tool_name: Some(call.name.clone()),
            tool_input: Some(call.args.clone()),
            tool_output: output.map(|o| Value::String(o.to_string())),
            cwd: call.cwd.clone(),
            subject: tool_subject(&call.name, &call.args),
            ..Default::default()
        };
        let out = run_hooks(event, &payload, &self.settings);
        if !out.errors.is_empty() {
            if let Ok(mut e) = self.errors.lock() {
                e.extend(out.errors.iter().cloned());
            }
        }
        out
    }

    /// `Bypass` asks nothing; `Ask` becomes `Deny` in `dontAsk`.
    fn ask(&self, why: String) -> Flow {
        if self.policy.mode() == Mode::Bypass {
            return Flow::Continue;
        }
        if self.policy.mode() == Mode::DontAsk {
            Flow::Deny(format!(
                "modo dontAsk: {why} (comandos e rede ficam de fora; use Livre para isso)"
            ))
        } else {
            Flow::Ask(why)
        }
    }

    /// Rules for the tools the policy cannot turn into an action (MCP,
    /// skills, plugins). `Ok(Some(why))`: ask; `Err`: the refusal.
    fn external_rules(&self, call: &ToolInvocation) -> Result<Option<String>, Flow> {
        let builtin = self.policy.action_for(&call.name, &call.args).is_some();
        // `memory` com action=recall responde também por memory_recall: uma
        // regra no nome antigo vale para o novo e vice-versa.
        let mut names = zerith_core::tool_alias::names(&call.name, &call.args);
        // `process` (ex-shell_output/shell_kill) tem o próprio mapa.
        for n in rule_names(&call.name, &call.args) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        if builtin || names.iter().any(|n| INTERNAL.contains(n)) {
            return Ok(None);
        }
        let subject = tool_subject(&call.name, &call.args);
        match self.policy.check_rules_any(&names, subject.as_deref()) {
            Some(Verdict::Deny(why)) => Err(Flow::Deny(why)),
            Some(Verdict::Ask(why)) => Ok(Some(why)),
            Some(Verdict::Allow) => Ok(None),
            None if call.name.starts_with("mcp__") => match self.policy.mode() {
                Mode::Bypass => Ok(None),
                Mode::DontAsk => Err(Flow::Deny(format!(
                    "modo dontAsk: ferramenta externa {} sem regra allow",
                    call.name
                ))),
                _ => Ok(Some(format!("ferramenta externa: {}", call.name))),
            },
            None => Ok(None),
        }
    }

    fn exit_plan(&self, call: &ToolInvocation) -> Flow {
        if !self.gate.in_plan() {
            return Flow::Deny("exit_plan_mode só vale no modo plan".into());
        }
        let plan = call.args.get("plan").and_then(Value::as_str).unwrap_or("");
        if plan.trim().is_empty() {
            return Flow::Deny("o plano está vazio; escreva o plano em `plan`".into());
        }
        Flow::Ask(format!("Plano proposto:\n\n{}", plan.trim()))
    }
}

/// The names whose permission rules apply to this call: the tool itself and,
/// for the background-job tool, its old name for the same action
/// (`process` kill ⇄ `shell_kill`, output ⇄ `shell_output`).
fn rule_names<'a>(name: &'a str, args: &Value) -> Vec<&'a str> {
    match (name, args.get("action").and_then(Value::as_str)) {
        ("process", Some("kill")) => vec![name, "shell_kill"],
        ("process", Some("output")) => vec![name, "shell_output"],
        ("shell_kill" | "shell_output", _) => vec![name, "process"],
        _ => vec![name],
    }
}

/// What a `Tool(specifier)` matcher sees: the command, path or URL.
pub fn tool_subject(tool: &str, args: &Value) -> Option<String> {
    let key = match tool {
        "shell" | "monitor" => "command",
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "list_dir" | "glob" | "grep" => {
            "path"
        }
        "skill" => "name",
        _ => "url",
    };
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

impl Middleware for Guards {
    fn before(&self, call: &mut ToolInvocation) -> Flow {
        if call.name == EXIT_PLAN_MODE {
            return self.exit_plan(call);
        }
        let mut ask = match self.external_rules(call) {
            Ok(a) => a,
            Err(deny) => return deny,
        };

        // PreToolUse.
        let pre = self.hooks(HookEvent::PreToolUse, call, None);
        if pre.blocked() {
            return Flow::Deny(reason(&pre, "bloqueado por hook PreToolUse"));
        }
        let mut context = pre.additional_context.clone();
        if let Some(new_args) = pre.updated_input.clone().filter(|a| *a != call.args) {
            call.args = new_args;
            // The new arguments face every check again: the rules here, the
            // policy and the approval in the pipeline. A hook never waives one.
            ask = match self.external_rules(call) {
                Ok(a) => a,
                Err(deny) => return deny,
            };
            // The model reads why the result is not what it asked for.
            context.push(format!(
                "[um hook PreToolUse alterou os argumentos desta chamada de {} para: {}]",
                call.name, call.args
            ));
        }
        if !context.is_empty() {
            if let Ok(mut p) = self.pending.lock() {
                p.entry((call.session_id.clone(), call.id.clone()))
                    .or_default()
                    .extend(context);
            }
        }
        if pre.decision == Some(Decision::Ask) {
            ask.get_or_insert_with(|| reason(&pre, "um hook PreToolUse pede confirmação"));
        }

        // PermissionRequest: only when the user is about to be asked.
        let policy_ask = self
            .policy
            .check_call(&call.name, &call.args)
            .and_then(|v| match v {
                Verdict::Ask(why) => Some(why),
                _ => None,
            });
        if let Some(why) = policy_ask.as_ref().or(ask.as_ref()) {
            let req = self.hooks(HookEvent::PermissionRequest, call, None);
            if req.blocked() {
                return Flow::Deny(reason(
                    &req,
                    &format!("hook PermissionRequest negou: {why}"),
                ));
            }
        }

        match ask {
            Some(why) => self.ask(why),
            None => Flow::Continue,
        }
    }

    fn after(&self, call: &ToolInvocation, result: &mut ToolOutcome) {
        let event = if result.ok {
            HookEvent::PostToolUse
        } else {
            HookEvent::PostToolUseFailure
        };
        let post = self.hooks(event, call, Some(&result.output));
        if let Some(ctx) = self
            .pending
            .lock()
            .ok()
            .and_then(|mut p| p.remove(&(call.session_id.clone(), call.id.clone())))
        {
            result.context.extend(ctx);
        }
        result
            .context
            .extend(post.additional_context.iter().cloned());
        // A post hook cannot undo the call; its objection is feedback.
        if post.blocked() {
            result
                .context
                .push(reason(&post, "um hook PostToolUse objetou ao resultado"));
        }
    }
}

fn reason(o: &HookOutcome, fallback: &str) -> String {
    if o.reasons.is_empty() {
        fallback.to_string()
    } else {
        o.reasons.join("; ")
    }
}
