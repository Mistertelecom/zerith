//! Native multi-provider client and local failover gateway.
//!
//! The rule that shapes this crate: **do not pretend two APIs are the same**.
//! OpenAI-compatible endpoints share one wire format; Anthropic's Messages API
//! does not, and lives in its own module rather than as a pile of special
//! cases inside the OpenAI path.
//!
//! Security: an API key lives in this struct in memory and is written to a
//! request header, never to a log or an error message.

mod anthropic;
mod antigravity;
pub mod bili;
mod codex;
mod config;
pub mod explain;
pub mod gateway;
pub mod images;
pub mod media;
pub mod models;
pub mod oauth;
mod openai;
mod retry;
pub mod stream;

pub use config::{AuthMode, OAuthCredential, OAuthTool, Protocol, ProviderConfig, ProviderKind};
pub use gateway::{
    infer_provider, is_failover_error, suggested_cooldown_ms, Gateway, GatewayOutcome, ModelSpec,
    RouteCandidate,
};
pub use openai::{Client, Completion, Probe, Usage};
pub use retry::RetryPolicy;

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no endpoint configured")]
    NoEndpoint,
    #[error("no api key configured")]
    NoKey,
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    /// 429 after the retries ran out. Separate from `Http` so a host can say
    /// "wait" instead of "broken".
    #[error("rate limited (429): {body}")]
    RateLimited {
        retry_after_ms: Option<u64>,
        body: String,
    },
    #[error("transport: {0}")]
    Transport(String),
    #[error("endpoint returned invalid json: {0}")]
    Json(#[from] serde_json::Error),
    /// The endpoint answered 200 with something that is not a completion.
    #[error("malformed response: {0}")]
    Malformed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, ProviderError>;

/// Cooperative cancellation. A stream checks it between chunks, so a cancelled
/// turn stops at the next network read instead of draining the response.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool result; `ChatMessage::tool_call_id` names the call it answers.
    Tool,
}

/// A tool call the model asked for. `arguments` is the raw JSON string the
/// model wrote: parsing is the caller's job, so a malformed one can be
/// reported back to the model instead of failing the turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A tool offered to the model: name, description, JSON schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Assistant only: the calls this message made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool only: the call this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new(Role::Assistant, content)
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, content)
        }
    }
}

/// One completion request. `model` is required: guessing a route is how a
/// harness silently bills a different model than the user picked.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<String>,
    /// Session cache affinity key sent to providers that support it (OpenAI/Codex).
    pub prompt_cache_key: Option<String>,
    /// Empty means no tools are offered.
    pub tools: Vec<ToolDef>,
    /// `auto`, `none`, `required`, or a tool name.
    pub tool_choice: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            prompt_cache_key: None,
            tools: Vec::new(),
            tool_choice: None,
        }
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn reasoning_effort(mut self, e: impl Into<String>) -> Self {
        self.reasoning_effort = Some(e.into());
        self
    }

    pub fn prompt_cache_key(mut self, k: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(k.into());
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    pub fn tool_choice(mut self, choice: impl Into<String>) -> Self {
        self.tool_choice = Some(choice.into());
        self
    }
}

/// A piece of a streamed answer.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Incremental text.
    Delta(String),
    /// Reasoning/thinking text, when the provider sends it separately.
    Reasoning(String),
    /// The complete tool calls of this answer, emitted once before `Done`:
    /// fragments are assembled in the client so no caller sees half an
    /// argument.
    ToolCalls(Vec<ToolCall>),
    /// The stream finished; carries usage if the provider reported it, and
    /// the provider's stop/finish reason as sent (`end_turn`, `length`, ...).
    Done {
        usage: Option<Usage>,
        stop_reason: Option<String>,
    },
}

/// Whether a stop/finish reason means the output hit the token cap: the
/// answer, and any tool call in it, is incomplete.
pub fn is_cut_off(stop_reason: &str) -> bool {
    matches!(
        stop_reason,
        "max_tokens"
            | "MAX_TOKENS"
            | "length"
            | "max_output_tokens"
            | "model_context_window_exceeded"
    )
}

/// Normalizes an optional reasoning-effort setting (`low`, `medium`, `high`),
/// treating empty strings and `"off"` as disabled (`None`).
pub(crate) fn normalize_reasoning_effort(effort: Option<&str>) -> Option<String> {
    let trimmed = effort?.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("off") {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

/// Maps `reasoning_effort` levels to provider thinking token budgets.
pub(crate) fn thinking_budget(effort: Option<&str>) -> Option<u32> {
    match normalize_reasoning_effort(effort)?.as_str() {
        "low" => Some(2048),
        "medium" => Some(8192),
        "high" => Some(16384),
        _ => None,
    }
}

#[cfg(test)]
mod reasoning_effort_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn off_is_omitted_and_levels_map_per_provider_protocol() {
        let off = ChatRequest::new("claude-sonnet-4-5", vec![ChatMessage::user("oi")])
            .temperature(0.4)
            .max_tokens(1024)
            .reasoning_effort("off");
        assert!(openai::body(&off, false).get("reasoning_effort").is_none());
        assert!(codex::body(&off, false).get("reasoning").is_none());
        let anth_off = anthropic::body(&off, false);
        assert!(anth_off.get("thinking").is_none());
        assert!(anth_off.get("temperature").is_some());
        assert!(antigravity::body(&off, None)
            .get("request")
            .and_then(|r| r.get("generationConfig"))
            .and_then(|g| g.get("thinkingConfig"))
            .is_none());

        for (level, budget) in [("low", 2048u32), ("medium", 8192), ("high", 16384)] {
            let req = ChatRequest::new("claude-sonnet-4-5", vec![ChatMessage::user("oi")])
                .temperature(0.3)
                .max_tokens(1024)
                .reasoning_effort(level);
            assert_eq!(openai::body(&req, false)["reasoning_effort"], json!(level));
            assert_eq!(
                codex::body(&req, false)["reasoning"],
                json!({"effort": level})
            );

            let anth = anthropic::body(&req, false);
            assert_eq!(
                anth["thinking"],
                json!({"type": "enabled", "budget_tokens": budget})
            );
            assert!(
                anth.get("temperature").is_none(),
                "Anthropic forbids temperature when thinking is enabled"
            );
            assert!(
                anth["max_tokens"].as_u64().unwrap() >= u64::from(budget) + 1024,
                "Anthropic requires max_tokens > budget_tokens"
            );

            let ag = antigravity::body(&req, Some("proj-1"));
            assert_eq!(
                ag["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
                json!(budget)
            );
        }
    }
}
