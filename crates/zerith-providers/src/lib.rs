//! Providers, and the 9Router in particular.
//!
//! The rule that shapes this crate: **do not pretend two APIs are the same**.
//! OpenAI-compatible endpoints share one wire format; Anthropic's Messages API
//! does not, and is a separate `Protocol` rather than a pile of special cases
//! inside the OpenAI path.
//!
//! Security: an API key lives in this struct in memory and is written to a
//! request header, never to a log or an error message.

mod config;
mod openai;
pub mod stream;

pub use config::{Protocol, ProviderConfig, ProviderKind};
pub use openai::{Client, Probe, Usage};

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
    #[error("transport: {0}")]
    Transport(String),
    #[error("endpoint returned invalid json: {0}")]
    Json(#[from] serde_json::Error),
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
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
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
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
}

/// A piece of a streamed answer.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Incremental text.
    Delta(String),
    /// Reasoning/thinking text, when the provider sends it separately.
    Reasoning(String),
    /// The stream finished; carries usage if the provider reported it.
    Done { usage: Option<Usage> },
}
