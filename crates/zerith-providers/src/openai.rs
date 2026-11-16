//! The OpenAI-compatible path used by Zerith's native multi-provider gateway,
//! and the `Client` that routes to the Anthropic module when the protocol says
//! so.
//!
//! `ureq` is blocking on purpose: the CLI probes an endpoint synchronously and
//! the agent loop calls this from a worker thread. No async runtime in a
//! harness that does not need one.

use crate::config::{Protocol, ProviderConfig};
use crate::retry::{self, map_err, RetryPolicy};
use crate::stream::OpenAiStream;
use crate::{Cancel, ChatRequest, ProviderError, Result, Role, StreamEvent, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

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
    retry: RetryPolicy,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub reasoning: Option<String>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

impl Client {
    /// Building a client only needs an endpoint: `GET /models` is usually
    /// public, and requiring a key first would hide a working local server.
    /// The key is demanded where it is actually spent, in `complete`/`stream`.
    pub fn new(cfg: ProviderConfig) -> Result<Self> {
        if cfg.base_url.trim().is_empty() {
            return Err(ProviderError::NoEndpoint);
        }
        // Connect and per-read timeouts, never a total one: in ureq 2 a total
        // timeout covers the body too, and would cut a long stream that is
        // still producing tokens. A read that stalls this long is a dead peer.
        let t = Duration::from_millis(cfg.timeout_ms);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(t.min(Duration::from_secs(30)))
            .timeout_read(t)
            .timeout_write(t)
            .build();
        Ok(Self {
            cfg,
            agent,
            retry: RetryPolicy::default(),
        })
    }

    /// Replaces the retry policy (tests use a fast one, hosts may disable it).
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    pub(crate) fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn request(&self, url: &str) -> ureq::Request {
        self.authored(self.agent.get(url))
    }

    /// Applies whichever credential the protocol expects. The key never reaches
    /// a log or an error string.
    fn authored(&self, mut req: ureq::Request) -> ureq::Request {
        if self.cfg.protocol == Protocol::Anthropic {
            req = req.set("anthropic-version", "2023-06-01");
        }
        if let Some(key) = &self.cfg.api_key {
            req = match self.cfg.protocol {
                Protocol::OpenAi => req.set("Authorization", &format!("Bearer {key}")),
                Protocol::Anthropic => req.set("x-api-key", key),
            };
        }
        req
    }

    /// POSTs JSON with bounded retry. Only the send is retried: by the time
    /// this returns, no byte of the answer has been shown to anyone.
    // `ureq::Error` is large by its own design; boxing it here buys nothing.
    #[allow(clippy::result_large_err)]
    pub(crate) fn post(&self, path: &str, body: &Value, cancel: &Cancel) -> Result<ureq::Response> {
        let url = self.endpoint(path);
        retry::send(&self.retry, cancel, || {
            self.authored(self.agent.post(&url))
                .set("Content-Type", "application/json")
                .send_json(body)
        })
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
        if self.cfg.protocol == Protocol::Anthropic {
            return crate::anthropic::complete(self, req, cancel);
        }
        let resp = self.post("chat/completions", &body(req, false), cancel)?;
        let text = resp
            .into_string()
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        parse_completion(&text)
    }

    /// Streams a completion, calling `on_event` per chunk. Cancellation is
    /// checked between chunks; the response is dropped, which closes the socket.
    ///
    /// Tool calls arrive as one `StreamEvent::ToolCalls` before the final
    /// `Done`, already assembled from their fragments.
    pub fn stream(
        &self,
        req: &ChatRequest,
        cancel: &Cancel,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<()> {
        self.ensure_key()?;
        if self.cfg.protocol == Protocol::Anthropic {
            return crate::anthropic::stream(self, req, cancel, &mut on_event);
        }
        let resp = self.post("chat/completions", &body(req, true), cancel)?;
        let mut state = OpenAiStream::default();
        crate::stream::for_each_data(resp, cancel, |data| state.feed(data, &mut on_event))?;
        state.finish(&mut on_event)
    }
}

pub(crate) fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// The request body. Built as a `Value` because the optional fields differ by
/// message role, and a struct per role would be more code than the JSON.
fn body(req: &ChatRequest, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut v = json!({"role": role_str(m.role), "content": m.content});
            if !m.tool_calls.is_empty() {
                // `null` content beside tool calls: some servers reject "".
                if m.content.is_empty() {
                    v["content"] = Value::Null;
                }
                v["tool_calls"] = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {"name": c.name, "arguments": c.arguments}
                        })
                    })
                    .collect();
            }
            if let Some(id) = &m.tool_call_id {
                v["tool_call_id"] = json!(id);
            }
            v
        })
        .collect();
    let mut b = json!({"model": req.model, "messages": messages, "stream": stream});
    if let Some(n) = req.max_tokens {
        b["max_tokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        b["temperature"] = json!(t);
    }
    if let Some(e) = &req.reasoning_effort {
        b["reasoning_effort"] = json!(e);
    }
    if stream {
        // OpenAI-compatible servers only report usage on a stream when asked;
        // without this the window shows no token counts at all.
        b["stream_options"] = json!({"include_usage": true});
    }
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();
        if let Some(choice) = &req.tool_choice {
            b["tool_choice"] = match choice.as_str() {
                "auto" | "none" | "required" => json!(choice),
                name => json!({"type": "function", "function": {"name": name}}),
            };
        }
    }
    b
}

fn parse_completion(text: &str) -> Result<Completion> {
    let v: Value = serde_json::from_str(text)?;
    if let Some(e) = v.get("error") {
        return Err(ProviderError::Malformed(error_message(e)));
    }
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| ProviderError::Malformed("resposta sem choices".into()))?;
    let msg = &choice["message"];
    let str_at = |v: &Value| v.as_str().map(str::to_string);
    let tool_calls = msg["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .map(|(i, c)| ToolCall {
                    id: str_at(&c["id"]).unwrap_or_else(|| format!("call_{i}")),
                    name: str_at(&c["function"]["name"]).unwrap_or_default(),
                    arguments: str_at(&c["function"]["arguments"]).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Completion {
        text: str_at(&msg["content"]).unwrap_or_default(),
        reasoning: str_at(&msg["reasoning_content"]),
        usage: v
            .get("usage")
            .and_then(|u| serde_json::from_value(u.clone()).ok()),
        finish_reason: str_at(&choice["finish_reason"]),
        tool_calls,
    })
}

pub(crate) fn error_message(e: &Value) -> String {
    e.get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatMessage, ToolDef};

    #[test]
    fn a_tool_round_trip_is_serialized_in_openai_shape() {
        let req = ChatRequest::new(
            "m",
            vec![
                ChatMessage::user("oi"),
                ChatMessage::assistant(
                    "",
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: "{\"path\":\"a\"}".into(),
                    }],
                ),
                ChatMessage::tool("c1", "conteudo"),
            ],
        )
        .tools(vec![ToolDef {
            name: "read_file".into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }])
        .tool_choice("auto");
        let b = body(&req, true);
        assert_eq!(b["messages"][1]["content"], Value::Null);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(b["messages"][2]["role"], "tool");
        assert_eq!(b["messages"][2]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn a_blocking_answer_with_tool_calls_is_parsed() {
        let c = parse_completion(
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"x","type":"function","function":{"name":"glob","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();
        assert_eq!(c.text, "");
        assert_eq!(c.tool_calls[0].name, "glob");
        assert_eq!(c.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn an_answer_without_choices_is_malformed_not_a_panic() {
        assert!(matches!(
            parse_completion("{}"),
            Err(ProviderError::Malformed(_))
        ));
    }
}
