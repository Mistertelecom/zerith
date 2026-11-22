//! The OpenAI-compatible path used by Zerith's native multi-provider gateway,
//! and the `Client` that routes to the Anthropic module when the protocol says
//! so.
//!
//! `ureq` is blocking on purpose: the CLI probes an endpoint synchronously and
//! the agent loop calls this from a worker thread. No async runtime in a
//! harness that does not need one.

use crate::config::{AuthMode, OAuthTool, Protocol, ProviderConfig, ProviderKind};
use crate::retry::{self, map_err, RetryPolicy};
use crate::stream::OpenAiStream;
use crate::{Cancel, ChatRequest, ProviderError, Result, Role, StreamEvent, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub text: String,
    pub reasoning: Option<String>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<serde_json::Value>,
}

/// Reuses HTTP/TLS connection pools across turns and steps for each timeout bucket.
pub fn pooled_agent(timeout_ms: u64) -> ureq::Agent {
    static POOL: OnceLock<Mutex<HashMap<u64, ureq::Agent>>> = OnceLock::new();
    let pool = POOL.get_or_init(Default::default);
    let mut guard = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(&timeout_ms) {
        return existing.clone();
    }
    // Connect and per-read timeouts, never a total one: in ureq 2 a total
    // timeout covers the body too, and would cut a long stream that is
    // still producing tokens. A read that stalls this long is a dead peer.
    let t = Duration::from_millis(timeout_ms);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(t.min(Duration::from_secs(30)))
        .timeout_read(t)
        .timeout_write(t)
        .max_idle_connections(32)
        .max_idle_connections_per_host(8)
        .build();
    if guard.len() >= 16 {
        guard.clear();
    }
    guard.insert(timeout_ms, agent.clone());
    agent
}

impl Client {
    /// Building a client only needs an endpoint: `GET /models` is usually
    /// public, and requiring a key first would hide a working local server.
    /// The key is demanded where it is actually spent, in `complete`/`stream`.
    pub fn new(cfg: ProviderConfig) -> Result<Self> {
        if cfg.base_url.trim().is_empty() {
            return Err(ProviderError::NoEndpoint);
        }
        let agent = pooled_agent(cfg.timeout_ms);
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
        for (name, value) in self.headers() {
            req = req.set(&name, &value);
        }
        req
    }

    /// Exactly the headers auth contributes, lowercase-keyed. Separate from
    /// `authored` so a test can assert them without a socket.
    fn headers(&self) -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        if self.cfg.protocol == Protocol::Anthropic {
            h.insert("anthropic-version".into(), "2023-06-01".into());
        }
        match (self.cfg.auth, &self.cfg.oauth) {
            (AuthMode::Oauth, Some(cred)) => {
                h.insert(
                    "authorization".into(),
                    format!("Bearer {}", cred.access_token),
                );
                match cred.tool {
                    OAuthTool::ClaudeCode => {
                        h.insert("anthropic-beta".into(), "oauth-2025-04-20".into());
                    }
                    OAuthTool::Codex => {
                        h.insert("originator".into(), "codex_cli_rs".into());
                        if let Some(id) = &cred.account_id {
                            h.insert("chatgpt-account-id".into(), id.clone());
                        }
                    }
                    OAuthTool::Antigravity => {
                        h.insert("user-agent".into(), crate::antigravity::USER_AGENT.into());
                    }
                }
            }
            _ => {
                if let Some(key) = &self.cfg.api_key {
                    match self.cfg.protocol {
                        Protocol::Anthropic => {
                            h.insert("x-api-key".into(), key.clone());
                        }
                        // Responses takes the same Bearer the OpenAI path takes.
                        Protocol::OpenAi | Protocol::Responses | Protocol::Gemini => {
                            h.insert("authorization".into(), format!("Bearer {key}"));
                        }
                    }
                }
            }
        }
        h
    }

    /// POSTs JSON with bounded retry. Only the send is retried: by the time
    /// this returns, no byte of the answer has been shown to anyone.
    // `ureq::Error` is large by its own design; boxing it here buys nothing.
    #[allow(clippy::result_large_err)]
    pub(crate) fn post(&self, path: &str, body: &Value, cancel: &Cancel) -> Result<ureq::Response> {
        let url = crate::bili::url(&self.cfg, self.endpoint(path));
        retry::send(&self.retry, cancel, || {
            self.routed(self.authored(self.agent.post(&url)))
                .set("Content-Type", "application/json")
                .send_json(body)
        })
    }

    /// The conversation id, when the request goes through billion-context.
    fn routed(&self, req: ureq::Request) -> ureq::Request {
        match crate::bili::session_header(&self.cfg) {
            Some(id) => req.set("x-acp-session", &id),
            None => req,
        }
    }

    /// Same as `post`, declaring an SSE answer. Streaming endpoints refuse a
    /// buffered `Accept`, so the header is not decoration here.
    #[allow(clippy::result_large_err)]
    pub(crate) fn post_stream(
        &self,
        path: &str,
        body: &Value,
        cancel: &Cancel,
    ) -> Result<ureq::Response> {
        let url = crate::bili::url(&self.cfg, self.endpoint(path));
        retry::send(&self.retry, cancel, || {
            self.routed(self.authored(self.agent.post(&url)))
                .set("Content-Type", "application/json")
                .set("Accept", "text/event-stream")
                .send_json(body)
        })
    }

    /// Fails before the request when the provider needs a key it does not have.
    fn ensure_key(&self) -> Result<()> {
        let has_oauth = self.cfg.auth == AuthMode::Oauth && self.cfg.oauth.is_some();
        if self.cfg.kind.needs_key()
            && !has_oauth
            && self.cfg.api_key.as_deref().unwrap_or("").is_empty()
        {
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
        if self.cfg.protocol == Protocol::Responses {
            return crate::codex::complete(self, req, cancel);
        }
        if self.cfg.protocol == Protocol::Gemini {
            return crate::antigravity::complete(self, req, cancel);
        }
        let resp = self.post("chat/completions", &body_for(self, req, false), cancel)?;
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
        if self.cfg.protocol == Protocol::Responses {
            return crate::codex::stream(self, req, cancel, &mut on_event);
        }
        if self.cfg.protocol == Protocol::Gemini {
            return crate::antigravity::stream(self, req, cancel, &mut on_event);
        }
        let resp = self.post("chat/completions", &body_for(self, req, true), cancel)?;
        let mut state = OpenAiStream::default();
        crate::stream::for_each_data(resp, cancel, |data| state.feed(data, &mut on_event))?;
        state.finish(&mut on_event)
    }
}

pub(crate) fn is_openai_reasoning_model(model: &str) -> bool {
    let bare = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    ["o1", "o3", "o4", "gpt-5"].iter().any(|prefix| {
        bare == *prefix
            || bare
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(['-', '.', ':']))
    })
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
pub(crate) fn body(req: &ChatRequest, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let mut v = json!({"role": role_str(m.role), "content": m.content});
            let images = match m.role {
                Role::User if !m.synthetic => crate::images::local_images(&m.content),
                _ => Vec::new(),
            };
            if !images.is_empty() {
                let mut parts = vec![json!({"type": "text", "text": m.content})];
                parts.extend(
                    images
                        .iter()
                        .map(|i| json!({"type": "image_url", "image_url": {"url": i.data_url()}})),
                );
                v["content"] = json!(parts);
            }
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
    if let Some(t) = req
        .temperature
        .filter(|_| !is_openai_reasoning_model(&req.model))
    {
        b["temperature"] = json!(t);
    }
    // OpenAI-compatible servers take low/medium/high: `xhigh`/`max` go down to `high`.
    if let Some(e) =
        crate::clamp_effort(req.reasoning_effort.as_deref(), &["low", "medium", "high"])
    {
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

pub(crate) fn body_for(client: &Client, req: &ChatRequest, stream: bool) -> Value {
    let mut b = body(req, stream);
    // OpenAI's reasoning models reject `max_tokens` and other OpenAI models
    // reject `reasoning_effort`; compatible servers keep the classic names.
    if client.cfg.kind == ProviderKind::OpenAi {
        if let Some(obj) = b.as_object_mut() {
            if is_openai_reasoning_model(&req.model) {
                if let Some(n) = obj.remove("max_tokens") {
                    obj.insert("max_completion_tokens".into(), n);
                }
            } else {
                obj.remove("reasoning_effort");
            }
        }
    }
    if let Some(key) = req.prompt_cache_key.as_deref().filter(|k| !k.is_empty()) {
        if matches!(client.cfg.kind, ProviderKind::OpenAi | ProviderKind::Codex) {
            b["prompt_cache_key"] = json!(key);
        }
    }
    // OpenRouter preserves Anthropic prompt cache_control breakpoints when
    // targeting Claude models over the OpenAI-compatible wire.
    if client.cfg.kind == ProviderKind::OpenRouter
        && req.model.to_ascii_lowercase().contains("claude")
    {
        if let Some(msgs) = b.get_mut("messages").and_then(Value::as_array_mut) {
            if let Some(sys) = msgs.iter_mut().find(|m| m["role"] == "system") {
                if let Some(text) = sys
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    sys["content"] = json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": {"type": "ephemeral"}
                    }]);
                }
            }
        }
    }
    b
}

pub(crate) fn parse_openai_usage(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
    let completion_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
    let total_tokens = u["total_tokens"]
        .as_u64()
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    let cache_read_tokens = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| u["cache_read_input_tokens"].as_u64())
        .unwrap_or(0);
    let cache_write_tokens = u["cache_creation_input_tokens"]
        .as_u64()
        .or_else(|| u["prompt_tokens_details"]["cache_write_tokens"].as_u64())
        .or_else(|| u["prompt_cache_creation_tokens"].as_u64())
        .unwrap_or(0);
    Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
    })
}

fn parse_completion(text: &str) -> Result<Completion> {
    let v: Value = serde_json::from_str(text)?;
    if let Some(e) = v.get("error") {
        return Err(crate::stream::stream_error(e));
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
                    id: str_at(&c["id"]).unwrap_or_else(|| crate::stream::fallback_call_id(i)),
                    name: str_at(&c["function"]["name"]).unwrap_or_default(),
                    arguments: str_at(&c["function"]["arguments"]).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Completion {
        text: str_at(&msg["content"]).unwrap_or_default(),
        reasoning: str_at(&msg["reasoning_content"]),
        usage: v.get("usage").and_then(parse_openai_usage),
        finish_reason: str_at(&choice["finish_reason"]),
        tool_calls,
        thinking_blocks: Vec::new(),
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

    #[test]
    fn oauth_mode_sends_bearer_and_the_anthropic_beta_header() {
        let mut cfg = ProviderConfig::new(crate::config::ProviderKind::Anthropic);
        cfg.base_url = "http://127.0.0.1:9".into();
        cfg.auth = AuthMode::Oauth;
        cfg.oauth = Some(crate::config::OAuthCredential {
            access_token: "tok-123".into(),
            refresh_token: Some("ref-456".into()),
            expires_at: 4_000_000_000,
            account_id: None,
            tool: OAuthTool::ClaudeCode,
            imported: Some(false),
        });
        let client = Client::new(cfg).unwrap();
        let h = client.headers();
        assert_eq!(
            h.get("authorization").map(String::as_str),
            Some("Bearer tok-123")
        );
        assert_eq!(
            h.get("anthropic-beta").map(String::as_str),
            Some("oauth-2025-04-20")
        );
        assert_eq!(h.get("x-api-key"), None);
        assert!(client.ensure_key().is_ok());
    }

    #[test]
    fn api_key_mode_keeps_x_api_key_and_no_beta_header() {
        let mut cfg = ProviderConfig::new(crate::config::ProviderKind::Anthropic);
        cfg.base_url = "http://127.0.0.1:9".into();
        cfg.api_key = Some("sk-ant-1".into());
        let h = Client::new(cfg).unwrap().headers();
        assert_eq!(h.get("x-api-key").map(String::as_str), Some("sk-ant-1"));
        assert_eq!(h.get("authorization"), None);
        assert_eq!(h.get("anthropic-beta"), None);
    }

    /// A `{:?}` of the config must never be enough to steal the session.
    #[test]
    fn debug_of_a_credential_hides_the_tokens() {
        let cred = crate::config::OAuthCredential {
            access_token: "secret-access".into(),
            refresh_token: Some("secret-refresh".into()),
            expires_at: 1,
            account_id: None,
            tool: OAuthTool::Codex,
            imported: Some(false),
        };
        let dbg = format!("{cred:?}");
        assert!(!dbg.contains("secret-access"));
        assert!(!dbg.contains("secret-refresh"));
    }

    #[test]
    fn prompt_cache_key_and_openrouter_claude_cache_control_are_emitted() {
        let openai = Client::new(ProviderConfig::new(ProviderKind::OpenAi)).unwrap();
        let req = ChatRequest::new(
            "gpt-4o",
            vec![ChatMessage::system("sys"), ChatMessage::user("hi")],
        )
        .prompt_cache_key("sess-42");
        let ob = body_for(&openai, &req, false);
        assert_eq!(ob["prompt_cache_key"], "sess-42");

        let or = Client::new(ProviderConfig::new(ProviderKind::OpenRouter)).unwrap();
        let creq = ChatRequest::new(
            "anthropic/claude-sonnet-4-5",
            vec![ChatMessage::system("sys"), ChatMessage::user("hi")],
        )
        .prompt_cache_key("sess-42");
        let orb = body_for(&or, &creq, false);
        assert!(orb.get("prompt_cache_key").is_none());
        assert_eq!(
            orb["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn openai_reasoning_models_get_max_completion_tokens_and_effort_only_there() {
        let openai = Client::new(ProviderConfig::new(ProviderKind::OpenAi)).unwrap();
        let mut req = ChatRequest::new("o3-mini", vec![ChatMessage::user("hi")]);
        req.max_tokens = Some(100);
        req.reasoning_effort = Some("high".into());
        let b = body_for(&openai, &req, false);
        assert_eq!(b["max_completion_tokens"], 100);
        assert!(b.get("max_tokens").is_none());
        assert_eq!(b["reasoning_effort"], "high");

        req.model = "gpt-4o".into();
        let b = body_for(&openai, &req, false);
        assert_eq!(b["max_tokens"], 100);
        assert!(b.get("reasoning_effort").is_none());

        // Other OpenAI-compatible servers keep the classic fields.
        let ds = Client::new(ProviderConfig::new(ProviderKind::DeepSeek)).unwrap();
        req.model = "o3-mini".into();
        let b = body_for(&ds, &req, false);
        assert_eq!(b["max_tokens"], 100);
        assert_eq!(b["reasoning_effort"], "high");
    }

    #[test]
    fn parse_openai_usage_reads_cached_tokens_across_providers() {
        let u1 = parse_openai_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050,
            "prompt_tokens_details": {"cached_tokens": 768}
        }))
        .unwrap();
        assert_eq!(u1.cache_read_tokens, 768);

        let u2 = parse_openai_usage(&json!({
            "prompt_tokens": 900,
            "completion_tokens": 40,
            "prompt_cache_hit_tokens": 640
        }))
        .unwrap();
        assert_eq!(u2.cache_read_tokens, 640);
        assert_eq!(u2.total_tokens, 940);
    }

    #[test]
    fn reasoning_models_omit_temperature_while_gpt_4o_keeps_it() {
        for model in [
            "o1",
            "o1-mini",
            "o3",
            "openai/o3",
            "openrouter/openai/o3-mini",
            "o4-mini",
            "gpt-5",
            "gpt-5-codex",
            "openai/gpt-5-codex",
        ] {
            let req = ChatRequest::new(model, vec![ChatMessage::user("oi")]).temperature(0.7);
            let b = body(&req, false);
            assert!(
                b.get("temperature").is_none(),
                "expected temperature omitted for {model}: {b}"
            );
        }

        for model in ["gpt-4o", "openai/gpt-4o"] {
            let req = ChatRequest::new(model, vec![ChatMessage::user("oi")]).temperature(0.7);
            let b = body(&req, false);
            assert!(
                b.get("temperature").is_some(),
                "expected temperature retained for {model}: {b}"
            );
        }
    }
}
