//! The tool loop: request → stream → tool calls → policy → approval → run →
//! persist → request again, until the model answers without calling a tool,
//! the user cancels, or `max_steps` runs out.
//!
//! Durable records are written once each: one assistant row per step (with
//! its tool calls as JSON), one tool row per result. Deltas are never written.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_providers::{
    Cancel, ChatMessage, ChatRequest, Client, ProviderError, Role as PRole, StreamEvent, ToolCall,
    ToolDef, Usage,
};
use zerith_security::{Policy, Verdict};
use zerith_sessions::{Message, NewMessage, Role, Store};
use zerith_tools::{CallCtx, Registry};

use crate::{Result, RunError, RunEvent, TurnContext, TurnReport, HISTORY_LIMIT};

/// The user's answer to an `ApprovalNeeded` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    AllowOnce,
    /// Allow this tool for as long as the host keeps the same `ApprovalGate`.
    AllowSession,
    Deny,
}

/// The UI's half: answers an `ApprovalNeeded { id, .. }` from any thread.
#[derive(Clone)]
pub struct ApprovalSender(mpsc::Sender<(u64, Approval)>);

impl ApprovalSender {
    /// Returns false when the turn is no longer listening (it ended or was
    /// cancelled); the answer is then moot.
    pub fn answer(&self, id: u64, approval: Approval) -> bool {
        self.0.send((id, approval)).is_ok()
    }
}

/// The turn's half. Keep one per conversation: `AllowSession` grants live
/// here, so a new gate starts asking again.
pub struct ApprovalGate {
    rx: Mutex<mpsc::Receiver<(u64, Approval)>>,
    granted: Mutex<HashSet<String>>,
}

pub fn approval_channel() -> (ApprovalSender, ApprovalGate) {
    let (tx, rx) = mpsc::channel();
    (
        ApprovalSender(tx),
        ApprovalGate {
            rx: Mutex::new(rx),
            granted: Mutex::new(HashSet::new()),
        },
    )
}

impl ApprovalGate {
    /// Blocks until the answer for `id` arrives. Fails closed: a cancel, or a
    /// sender that went away, is a Deny.
    fn wait(&self, id: u64, cancel: &Cancel) -> Approval {
        let Ok(rx) = self.rx.lock() else {
            return Approval::Deny;
        };
        loop {
            if cancel.is_cancelled() {
                return Approval::Deny;
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((got, a)) if got == id => return a,
                // A late answer to an earlier prompt: ignore it.
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Approval::Deny,
            }
        }
    }

    // ponytail: grants are per tool name ("allow shell for the session"), not
    // per command prefix. Key on (tool, pattern) if users want finer grants.
    fn granted(&self, tool: &str) -> bool {
        self.granted
            .lock()
            .map(|g| g.contains(tool))
            .unwrap_or(false)
    }

    fn grant(&self, tool: &str) {
        if let Ok(mut g) = self.granted.lock() {
            g.insert(tool.to_string());
        }
    }
}

/// What the loop may use besides the conversation. `Default` is "no tools":
/// one request, one answer, exactly the old turn.
pub struct AgentOptions<'a> {
    pub tools: Option<&'a Registry>,
    /// Required for tools to run; without it every call is refused.
    pub policy: Option<&'a Policy>,
    /// Without a gate, a call that needs approval is refused (fail closed).
    pub approvals: Option<&'a ApprovalGate>,
    pub max_steps: usize,
    /// Past messages replayed to the model. The execution profile sets it
    /// (`Profile::limits().history_window`); the default is `HISTORY_LIMIT`.
    pub history: usize,
}

pub const DEFAULT_MAX_STEPS: usize = 25;

impl Default for AgentOptions<'_> {
    fn default() -> Self {
        Self {
            tools: None,
            policy: None,
            approvals: None,
            max_steps: DEFAULT_MAX_STEPS,
            history: HISTORY_LIMIT,
        }
    }
}

impl AgentOptions<'_> {
    /// No tools, with the history window of an execution profile.
    pub fn for_profile(p: zerith_core::settings::Profile) -> Self {
        Self {
            history: p.limits().history_window,
            ..Self::default()
        }
    }
}

/// Replaces a tool result dropped to fit the context window.
pub const DROPPED: &str = "[saída removida para caber no contexto]";
/// Stands in for a result that was never recorded (crash or cancel mid-step):
/// the API rejects a tool call without an answer.
const INTERRUPTED: &str = "[sem resultado: o turno foi interrompido]";
const PREVIEW_CHARS: usize = 200;

static NEXT_APPROVAL: AtomicU64 = AtomicU64::new(1);

/// Runs one agent turn with tools. See the module docs for the loop.
#[allow(clippy::too_many_arguments)]
pub fn run_agent_turn(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    agent: AgentOptions<'_>,
    session_id: &str,
    model: &str,
    prompt: &str,
    cancel: &Cancel,
    mut on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    let on_event: &mut dyn FnMut(RunEvent) = &mut on_event;
    let session = store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;

    store.append(session_id, &NewMessage::new(Role::User, prompt))?;

    let mut messages = vec![ChatMessage::system(crate::system_prompt(
        &session.title,
        ctx,
        session_id,
        prompt,
    ))];
    // The newest rows, which include the prompt just appended.
    messages.extend(replay(
        &store.recent_messages(session_id, agent.history.max(1))?,
    ));

    let defs: Vec<ToolDef> = agent
        .tools
        .map(|r| {
            r.specs()
                .into_iter()
                .map(|s| ToolDef {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    parameters: s.parameters.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let budget = client.config().context_limit.map(|l| l as usize * 8 / 10);

    let (mut prompt_tokens, mut completion_tokens) = (0, 0);
    let mut last = (0i64, String::new(), None::<String>);
    let max_steps = agent.max_steps.max(1);

    for step in 1..=max_steps {
        on_event(RunEvent::Step { n: step });
        if let Some(b) = budget {
            fit_context(&mut messages, b);
        }
        let mut req = ChatRequest::new(model, messages.clone());
        if !defs.is_empty() {
            req = req.tools(defs.clone()).tool_choice("auto");
        }
        let out = request(client, &req, cancel, on_event)?;
        if let Some(u) = &out.usage {
            prompt_tokens += u.prompt_tokens;
            completion_tokens += u.completion_tokens;
            on_event(RunEvent::Usage {
                input: u.prompt_tokens,
                output: u.completion_tokens,
            });
        }

        let id = store.append(
            session_id,
            &NewMessage {
                reasoning: out.reasoning.clone(),
                token_count: out.usage.as_ref().map(|u| u.completion_tokens as i64),
                tool_calls: (!out.calls.is_empty()).then(|| json!(out.calls)),
                ..NewMessage::new(Role::Assistant, out.text.clone())
            },
        )?;
        last = (id, out.text.clone(), out.reasoning.clone());
        messages.push(ChatMessage::assistant(out.text, out.calls.clone()));
        if out.calls.is_empty() {
            break;
        }

        for (i, call) in out.calls.iter().enumerate() {
            if cancel.is_cancelled() {
                // Close every open call so the stored history stays paired.
                for c in &out.calls[i..] {
                    record(store, session_id, c, "[cancelado]", &mut messages)?;
                }
                return Err(ProviderError::Cancelled.into());
            }
            on_event(RunEvent::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.arguments.clone(),
            });
            let (ok, output) = execute(call, ctx, &agent, cancel, on_event);
            record(store, session_id, call, &output, &mut messages)?;
            on_event(RunEvent::ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                ok,
                output_preview: output.chars().take(PREVIEW_CHARS).collect(),
            });
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled.into());
        }
        if step == max_steps {
            // Said out loud and stored: a turn that silently stops mid-task
            // reads as a finished one.
            let note = format!("[parado: limite de {max_steps} passos atingido]");
            on_event(RunEvent::Delta(note.clone()));
            let id = store.append(session_id, &NewMessage::new(Role::Assistant, note.clone()))?;
            last = (id, note, None);
        }
    }

    on_event(RunEvent::Done {
        stored: Some(last.0),
    });
    Ok(TurnReport {
        session_id: session_id.to_string(),
        message_id: last.0,
        text: last.1,
        reasoning: last.2,
        prompt_tokens,
        completion_tokens,
    })
}

fn record(
    store: &Store,
    session_id: &str,
    call: &ToolCall,
    output: &str,
    messages: &mut Vec<ChatMessage>,
) -> Result<()> {
    store.append(
        session_id,
        &NewMessage {
            tool_name: Some(call.name.clone()),
            tool_calls: Some(json!({"tool_call_id": call.id})),
            ..NewMessage::new(Role::Tool, output)
        },
    )?;
    messages.push(ChatMessage::tool(call.id.clone(), output));
    Ok(())
}

/// Policy, approval, run. Every failure becomes text for the model: a refused
/// or broken call is information, not a reason to end the turn.
fn execute(
    call: &ToolCall,
    ctx: TurnContext<'_>,
    agent: &AgentOptions<'_>,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
) -> (bool, String) {
    let (Some(tools), Some(policy)) = (agent.tools, agent.policy) else {
        return (false, "erro: ferramentas desativadas neste turno".into());
    };
    let args: Value = match serde_json::from_str(if call.arguments.trim().is_empty() {
        "{}"
    } else {
        &call.arguments
    }) {
        Ok(v) => v,
        Err(e) => return (false, format!("erro: argumentos não são JSON válido: {e}")),
    };
    let cctx = CallCtx {
        workspace: &policy.workspace,
        policy,
        memory: ctx.memory,
        scope: ctx.scope,
    };
    let verdict = match tools.authorize(&call.name, &args, &cctx) {
        Ok((v, _)) => v,
        Err(e) => return (false, format!("erro: {e}")),
    };
    match verdict {
        Verdict::Allow => {}
        Verdict::Deny(why) => return (false, format!("negado: {why}")),
        Verdict::Ask(why) => {
            let Some(gate) = agent.approvals else {
                return (
                    false,
                    format!("negado: precisa de aprovação ({why}) e não há quem aprove"),
                );
            };
            if !gate.granted(&call.name) {
                let id = NEXT_APPROVAL.fetch_add(1, Ordering::Relaxed);
                on_event(RunEvent::ApprovalNeeded {
                    id,
                    tool: call.name.clone(),
                    summary: why,
                });
                match gate.wait(id, cancel) {
                    Approval::AllowOnce => {}
                    Approval::AllowSession => gate.grant(&call.name),
                    Approval::Deny => return (false, "negado pelo usuário".into()),
                }
            }
        }
    }
    match tools.run_approved(&call.name, &args, &cctx, cancel) {
        Ok(out) => (true, out),
        Err(e) => (false, format!("erro: {e}")),
    }
}

#[derive(Default)]
struct StepOut {
    text: String,
    reasoning: Option<String>,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
}

/// One model request. Streaming first: a turn that only paints when it
/// finishes reads as a hang. If the stream fails before anything was shown,
/// fall back once to a blocking request — some OpenAI-compatible servers
/// answer 400 to `stream: true`.
fn request(
    client: &Client,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
) -> Result<StepOut> {
    let mut out = StepOut::default();
    let mut painted = false;
    let streamed = client.stream(req, cancel, |event| match event {
        StreamEvent::Delta(t) => {
            painted = true;
            out.text.push_str(&t);
            on_event(RunEvent::Delta(t));
        }
        StreamEvent::Reasoning(r) => {
            painted = true;
            out.reasoning.get_or_insert_with(String::new).push_str(&r);
            on_event(RunEvent::Reasoning(r));
        }
        StreamEvent::ToolCalls(c) => out.calls = c,
        // A trailing chunk without usage must not erase earlier counts.
        StreamEvent::Done { usage } => {
            if usage.is_some() {
                out.usage = usage;
            }
        }
    });
    let Err(e) = streamed else {
        return Ok(out);
    };
    // Repeating a stream that already painted would show the answer twice;
    // a 429 already went through its retries; a cancel is final.
    if painted
        || matches!(
            e,
            ProviderError::Cancelled | ProviderError::RateLimited { .. } | ProviderError::NoKey
        )
    {
        return Err(e.into());
    }
    let c = client.complete(req, cancel)?;
    if let Some(r) = &c.reasoning {
        on_event(RunEvent::Reasoning(r.clone()));
    }
    if !c.text.is_empty() {
        on_event(RunEvent::Delta(c.text.clone()));
    }
    Ok(StepOut {
        text: c.text,
        reasoning: c.reasoning,
        calls: c.tool_calls,
        usage: c.usage,
    })
}

/// Stored rows as provider messages, with tool calls and results paired.
pub fn replay(rows: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for m in rows {
        match m.role {
            Role::User | Role::System => {
                if m.content.trim().is_empty() {
                    continue;
                }
                let role = if m.role == Role::User {
                    PRole::User
                } else {
                    PRole::System
                };
                out.push(ChatMessage::new(role, m.content.clone()));
            }
            Role::Assistant => {
                let calls: Vec<ToolCall> = m
                    .tool_calls
                    .clone()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                if m.content.trim().is_empty() && calls.is_empty() {
                    continue;
                }
                out.push(ChatMessage::assistant(m.content.clone(), calls));
            }
            // A tool row without its call id (imported, or pre-tool-loop)
            // cannot be paired, and unpaired it reads as a claim nobody made.
            Role::Tool => {
                if let Some(id) = m
                    .tool_calls
                    .as_ref()
                    .and_then(|v| v["tool_call_id"].as_str())
                {
                    out.push(ChatMessage::tool(id, m.content.clone()));
                }
            }
        }
    }
    pair(out)
}

/// Every assistant tool call gets exactly one result right after it; results
/// without a call (the call fell outside the history window) are dropped.
fn pair(msgs: Vec<ChatMessage>) -> Vec<ChatMessage> {
    fn close(out: &mut Vec<ChatMessage>, open: &mut Vec<String>) {
        for id in open.drain(..) {
            out.push(ChatMessage::tool(id, INTERRUPTED));
        }
    }
    let mut out = Vec::with_capacity(msgs.len());
    let mut open: Vec<String> = Vec::new();
    for m in msgs {
        if m.role == PRole::Tool {
            let id = m.tool_call_id.as_deref().unwrap_or("");
            if let Some(pos) = open.iter().position(|o| o == id) {
                open.remove(pos);
                out.push(m);
            }
            continue;
        }
        close(&mut out, &mut open);
        if m.role == PRole::Assistant {
            open = m.tool_calls.iter().map(|c| c.id.clone()).collect();
        }
        out.push(m);
    }
    close(&mut out, &mut open);
    out
}

/// A rough 4 chars per token, plus a little per message for the framing.
pub fn estimate_tokens(msgs: &[ChatMessage]) -> usize {
    msgs.iter()
        .map(|m| {
            let calls: usize = m
                .tool_calls
                .iter()
                .map(|c| c.name.len() + c.arguments.len())
                .sum();
            (m.content.len() + calls) / 4 + 4
        })
        .sum()
}

/// Shrinks the history to `budget` tokens: oldest tool results lose their
/// body first, then whole oldest turns go. The system prompt (index 0) and
/// everything from the latest user message on are always kept.
pub fn fit_context(msgs: &mut Vec<ChatMessage>, budget: usize) {
    for i in 0..msgs.len() {
        if estimate_tokens(msgs) <= budget {
            return;
        }
        if msgs[i].role == PRole::Tool && msgs[i].content != DROPPED {
            msgs[i].content = DROPPED.into();
        }
    }
    let mut last_user = msgs
        .iter()
        .rposition(|m| m.role == PRole::User)
        .unwrap_or(0);
    while estimate_tokens(msgs) > budget {
        let Some(start) = msgs.iter().position(|m| m.role != PRole::System) else {
            return;
        };
        if start >= last_user {
            return;
        }
        // A turn runs to the next user message, so a tool call never loses
        // its result to the cut.
        let end = msgs[start + 1..]
            .iter()
            .position(|m| m.role == PRole::User)
            .map(|p| start + 1 + p)
            .unwrap_or(msgs.len())
            .min(last_user);
        msgs.drain(start..end);
        last_user -= end - start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "t".into(),
            arguments: "{}".into(),
        }
    }

    #[test]
    fn pairing_fills_missing_results_and_drops_orphans() {
        let msgs = pair(vec![
            ChatMessage::tool("orphan", "x"),
            ChatMessage::user("u"),
            ChatMessage::assistant("", vec![call("a"), call("b")]),
            ChatMessage::tool("a", "ra"),
            ChatMessage::user("u2"),
        ]);
        let roles: Vec<_> = msgs
            .iter()
            .map(|m| (m.role, m.tool_call_id.clone()))
            .collect();
        assert_eq!(
            roles,
            vec![
                (PRole::User, None),
                (PRole::Assistant, None),
                (PRole::Tool, Some("a".into())),
                (PRole::Tool, Some("b".into())),
                (PRole::User, None),
            ]
        );
        assert_eq!(msgs[3].content, INTERRUPTED);
    }

    #[test]
    fn context_drops_tool_bodies_first_then_old_turns_but_keeps_the_prompt() {
        let big = "x".repeat(4000);
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("velho"),
            ChatMessage::assistant("", vec![call("a")]),
            ChatMessage::tool("a", big.clone()),
            ChatMessage::assistant(big.clone(), vec![]),
            ChatMessage::user("atual"),
        ];
        let mut one = msgs.clone();
        fit_context(&mut one, 1100);
        assert_eq!(one[3].content, DROPPED);
        assert_eq!(one.len(), 6, "a tool body alone was enough");

        fit_context(&mut msgs, 50);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "sys");
        assert_eq!(msgs[1].content, "atual");
    }
}
