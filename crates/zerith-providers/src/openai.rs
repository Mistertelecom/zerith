//! The OpenAI-compatible path, which is what 9Router speaks.
//!
//! `ureq` is blocking on purpose: the CLI probes an endpoint synchronously and
//! the agent loop will call this from a worker thread. No async runtime in a
//! harness that does not need one.

use crate::config::{Protocol, ProviderConfig};
use crate::{Cancel, ChatRequest, ProviderError, Result, StreamEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// What `test connection` reports. Latency is measured locally, not taken from
/// the server, so it includes the network instead of hiding it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub models: Vec<String>,
    pub detail: String,
}

pub struct Client {
    cfg: ProviderConfig,
    agent: ureq::Agent,
}

#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    stream: bool,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub reasoning: Option<String>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

impl Client {
    /// Building a client only needs an endpoint: `GET /models` is usually
    /// public, and requiring a key first would hide a working local server.
    /// The key is demanded where it is actually spent, in `complete`/`stream`.
    pub fn new(cfg: ProviderConfig) -> Result<Self> {
        if cfg.base_url.trim().is_empty() {
            return Err(ProviderError::NoEndpoint);
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_millis(cfg.timeout_ms))
            .build();
        Ok(Self { cfg, agent })
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn request(&self, url: &str) -> ureq::Request {
        self.authored(self.agent.get(url))
    }

    /// Applies whichever credential the protocol expects. The key never reaches
    /// a log or an error string.
    fn authored(&self, mut req: ureq::Request) -> ureq::Request {
        if let Some(key) = &self.cfg.api_key {
            req = match self.cfg.protocol {
                Protocol::OpenAi => req.set("Authorization", &format!("Bearer {key}")),
                Protocol::Anthropic => req
                    .set("x-api-key", key)
                    .set("anthropic-version", "2023-06-01"),
            };
        }
        req
    }

    /// Fails before the request when the provider needs a key it does not have.
    fn ensure_key(&self) -> Result<()> {
        if self.cfg.kind.needs_key() && self.cfg.api_key.as_deref().unwrap_or("").is_empty() {
            return Err(ProviderError::NoKey);
        }
        Ok(())
    }

    /// Endpoint + auth in one round trip. A 401 is a real answer and is reported
    /// as such; only a transport failure becomes an error.
    pub fn probe(&self) -> Probe {
        let started = std::time::Instant::now();
        let url = self.endpoint("models");
        let resp = self.request(&url).call();
        let latency_ms = started.elapsed().as_millis() as u64;
        match resp {
            Ok(r) => {
                let status = r.status();
                let body = r.into_string().unwrap_or_default();
                let models: Vec<String> = serde_json::from_str::<ModelsResponse>(&body)
                    .map(|m| m.data.into_iter().map(|e| e.id).collect())
                    .unwrap_or_default();
                let detail = if models.is_empty() {
                    "endpoint answered but listed no models".to_string()
                } else {
                    format!("{} models", models.len())
                };
                Probe {
                    ok: (200..300).contains(&status),
                    status,
                    latency_ms,
                    models,
                    detail,
                }
            }
            Err(ureq::Error::Status(status, r)) => Probe {
                ok: false,
                status,
                latency_ms,
                models: Vec::new(),
                detail: r.into_string().unwrap_or_else(|_| "no body".into()),
            },
            Err(e) => Probe {
                ok: false,
                status: 0,
                latency_ms,
                models: Vec::new(),
                detail: e.to_string(),
            },
        }
    }

    pub fn models(&self) -> Result<Vec<String>> {
        let url = self.endpoint("models");
        let body = self
            .request(&url)
            .call()
            .map_err(map_err)?
            .into_string()
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let parsed: ModelsResponse = serde_json::from_str(&body)?;
        Ok(parsed.data.into_iter().map(|e| e.id).collect())
    }

    pub fn complete(&self, req: &ChatRequest, cancel: &Cancel) -> Result<Completion> {
        self.ensure_key()?;
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let body = ChatBody {
            model: &req.model,
            messages: req
                .messages
                .iter()
                .map(|m| WireMessage {
                    role: role_str(m.role),
                    content: &m.content,
                })
                .collect(),
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            reasoning_effort: req.reasoning_effort.as_deref(),
            stream: false,
        };
        let url = self.endpoint("chat/completions");
        let resp = self
            .authored(
                self.agent
                    .post(&url)
                    .set("Content-Type", "application/json"),
            )
            .send_json(&body)
            .map_err(map_err)?;
        let parsed: ChatResponse = serde_json::from_str(
            &resp
                .into_string()
                .map_err(|e| ProviderError::Transport(e.to_string()))?,
        )?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::Unsupported("empty choices"))?;
        Ok(Completion {
            text: choice.message.content,
            reasoning: choice.message.reasoning_content,
            usage: parsed.usage,
            finish_reason: choice.finish_reason,
        })
    }

    /// Streams a completion, calling `on_event` per chunk. Cancellation is
    /// checked between chunks; the response is dropped, which closes the socket.
    pub fn stream(
        &self,
        req: &ChatRequest,
        cancel: &Cancel,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<()> {
        self.ensure_key()?;
        let body = ChatBody {
            model: &req.model,
            messages: req
                .messages
                .iter()
                .map(|m| WireMessage {
                    role: role_str(m.role),
                    content: &m.content,
                })
                .collect(),
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            reasoning_effort: req.reasoning_effort.as_deref(),
            stream: true,
        };
        let url = self.endpoint("chat/completions");
        let resp = self
            .authored(
                self.agent
                    .post(&url)
                    .set("Content-Type", "application/json"),
            )
            .send_json(&body)
            .map_err(map_err)?;

        let reader = std::io::BufReader::new(resp.into_reader());
        for line in std::io::BufRead::lines(reader) {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let Ok(line) = line else { break };
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                on_event(StreamEvent::Done { usage: None });
                return Ok(());
            }
            if let Some(event) = crate::stream::parse_data_line(&line) {
                on_event(event);
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            if let Some(usage) = v.get("usage") {
                if let Ok(u) = serde_json::from_value::<Usage>(usage.clone()) {
                    on_event(StreamEvent::Done { usage: Some(u) });
                }
            }
        }
        Ok(())
    }
}

fn role_str(r: crate::Role) -> &'static str {
    match r {
        crate::Role::System => "system",
        crate::Role::User => "user",
        crate::Role::Assistant => "assistant",
    }
}

/// A 4xx/5xx is a provider answer, not a transport bug: keep the body so the
/// caller can show what the endpoint actually said.
fn map_err(e: ureq::Error) -> ProviderError {
    match e {
        ureq::Error::Status(status, r) => ProviderError::Http {
            status,
            body: r.into_string().unwrap_or_default(),
        },
        other => ProviderError::Transport(other.to_string()),
    }
}
