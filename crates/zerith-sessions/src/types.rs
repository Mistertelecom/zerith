//! Wire-facing session types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Role::System),
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            "tool" => Some(Role::Tool),
            _ => None,
        }
    }
}

/// Where a session came from. Imported sessions stay marked and never
/// masquerade as native ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    Native,
    Hermes,
    /// A child session spawned by the `agent` tool. `source_id` holds the
    /// parent session id and `source_profile` the child's own id (the pair is
    /// unique, the parent alone would not be).
    Subagent,
    /// Imported from Claude Code (`~/.claude/projects/*/*.jsonl`).
    #[serde(rename = "claude-code")]
    ClaudeCode,
    /// Imported from Codex (`~/.codex/sessions/**/*.jsonl`).
    Codex,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionSource::Native => "native",
            SessionSource::Hermes => "hermes",
            SessionSource::Subagent => "subagent",
            SessionSource::ClaudeCode => "claude-code",
            SessionSource::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "native" => Some(SessionSource::Native),
            "hermes" => Some(SessionSource::Hermes),
            "subagent" => Some(SessionSource::Subagent),
            "claude-code" => Some(SessionSource::ClaudeCode),
            "codex" => Some(SessionSource::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub source: SessionSource,
    /// Original id in the source system, kept for idempotent re-import.
    pub source_id: Option<String>,
    pub source_profile: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewMessage {
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_calls: Option<serde_json::Value>,
    pub reasoning: Option<String>,
    pub token_count: Option<i64>,
    /// Extra source fields with no Zerith equivalent, kept verbatim.
    pub raw: Option<serde_json::Value>,
}

impl NewMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_name: None,
            tool_calls: None,
            reasoning: None,
            token_count: None,
            raw: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_calls: Option<serde_json::Value>,
    pub reasoning: Option<String>,
    pub token_count: Option<i64>,
    pub created_at: i64,
    pub raw: Option<serde_json::Value>,
}
