//! Anthropic Messages API: `POST {base}/messages`.
//!
//! Differs from OpenAI where it matters: the system prompt is a top-level
//! field, messages must alternate user/assistant, tool calls are `tool_use`
//! content blocks and their results are `tool_result` blocks inside a *user*
//! message, and the stream is typed events instead of choice deltas.

use crate::{
    Cancel, ChatRequest, Client, Completion, ProviderError, Result, Role, StreamEvent, ToolCall,
    Usage,
};
use serde_json::{json, Value};

/// Anthropic requires `max_tokens`; a request without one gets the model's
/// output cap. Too low and a long `write_file` is cut mid-argument.
// ponytail: name heuristic; a catalogue entry per model if one rejects these.
fn max_output(model: &str) -> u32 {
    let m = model.to_ascii_lowercase();
    if !m.contains("claude") {
        return 8192;
    }
    if ["claude-3-5", "claude-3.5", "claude-3-7", "claude-3.7"]
        .iter()
        .any(|v| m.contains(v))
    {
        8192
    } else if ["claude-3", "claude-2", "claude-instant"]
        .iter()
        .any(|v| m.contains(v))
    {
        4096
    } else {
        // Claude 4.x and later: opus 4/4.1 stop at 32k, the rest go higher.
        32000
    }
}

pub(crate) fn body(req: &ChatRequest, stream: bool) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        let (role, blocks) = match m.role {
            Role::System => {
                system.push(m.content.as_str());
                continue;
            }
            Role::User => {
                let mut b = vec![json!({"type": "text", "text": m.content})];
                b.extend(crate::images::local_images(&m.content).iter().map(|i| {
                    json!({"type": "image", "source":
                        {"type": "base64", "media_type": i.mime, "data": i.data}})
                }));
                ("user", b)
            }
            Role::Assistant => {
                let mut b = Vec::new();
                if !m.content.is_empty() {
                    b.push(json!({"type": "text", "text": m.content}));
                }
                for c in &m.tool_calls {
                    // `input` must be an object; a call the model botched is
                    // replayed as `{}` rather than breaking every later turn.
                    let input = serde_json::from_str::<Value>(&c.arguments)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    b.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": input}));
                }
                ("assistant", b)
            }
            Role::Tool => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content
                })],
            ),
        };
        if blocks.is_empty() {
            continue;
        }
        // Consecutive same-role messages are merged: the API rejects two user
        // messages in a row, which is exactly what N tool results are.
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(arr) = last["content"].as_array_mut() {
                    arr.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let mut b = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or_else(|| max_output(&req.model)),
        "stream": stream,
    });
    if !system.is_empty() {
        b["system"] = json!([{
            "type": "text",
            "text": system.join("\n\n"),
            "cache_control": {"type": "ephemeral"}
        }]);
    }
    // Last block of the conversation: the next request starts with all of it,
    // so it is read from cache instead of billed again (4 breakpoints max:
    // tools, system, this one).
    if let Some(block) = messages
        .last_mut()
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|c| c.last_mut())
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    b["messages"] = json!(messages);
    if let Some(t) = req.temperature {
        b["temperature"] = json!(t);
    }
    if let Some(budget) = crate::thinking_budget(req.reasoning_effort.as_deref()) {
        // Anthropic API rejects `temperature` when extended thinking is active
        // and requires `max_tokens > budget_tokens`.
        if let Some(obj) = b.as_object_mut() {
            obj.remove("temperature");
        }
        let current_max = b["max_tokens"].as_u64().unwrap_or(0);
        let floor = u64::from(budget).saturating_add(1024);
        if current_max < floor {
            b["max_tokens"] = json!(floor);
        }
        b["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
    }
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
            .collect();
        // Prompt cache (like Claude Code): the tool list rarely changes, so
        // the server reuses it and bills a fraction of the input on reuse.
        if let Some(last) = b["tools"].as_array_mut().and_then(|t| t.last_mut()) {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        if let Some(choice) = &req.tool_choice {
            b["tool_choice"] = match choice.as_str() {
                "auto" => json!({"type": "auto"}),
                "required" => json!({"type": "any"}),
                "none" => json!({"type": "none"}),
                name => json!({"type": "tool", "name": name}),
            };
        }
    }
    b
}

/// A Claude subscription token only serves requests that open with the
/// Claude Code identity as the FIRST system block; anything else gets a bare
/// `429 rate_limit_error` even with quota left (verified against the live API).
/// Our own system prompt follows as a second block.
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

fn body_for(client: &Client, req: &ChatRequest, stream: bool) -> Value {
    let mut b = body(req, stream);
    let cfg = client.config();
    let subscription = cfg.auth == crate::AuthMode::Oauth
        && cfg
            .oauth
            .as_ref()
            .is_some_and(|c| c.tool == crate::OAuthTool::ClaudeCode);
    if subscription {
        let mut blocks = vec![json!({"type": "text", "text": CLAUDE_CODE_IDENTITY})];
        match b.get("system") {
            Some(Value::Array(arr)) => blocks.extend(arr.iter().cloned()),
            Some(Value::String(own)) => blocks.push(json!({
                "type": "text",
                "text": own,
                "cache_control": {"type": "ephemeral"}
            })),
            _ => {}
        }
        b["system"] = json!(blocks);
    }
    b
}

pub(crate) fn complete(client: &Client, req: &ChatRequest, cancel: &Cancel) -> Result<Completion> {
    let resp = client.post("messages", &body_for(client, req, false), cancel)?;
    let text = resp
        .into_string()
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    parse_completion(&text)
}

fn parse_completion(text: &str) -> Result<Completion> {
    let v: Value = serde_json::from_str(text)?;
    if let Some(e) = v.get("error") {
        return Err(crate::stream::stream_error(e));
    }
    let blocks = v["content"]
        .as_array()
        .ok_or_else(|| ProviderError::Malformed("resposta sem content".into()))?;
    let mut out = Completion {
        text: String::new(),
        reasoning: None,
        usage: usage(&v["usage"], 0),
        finish_reason: v["stop_reason"].as_str().map(str::to_string),
        tool_calls: Vec::new(),
    };
    for b in blocks {
        match b["type"].as_str() {
            Some("text") => out.text.push_str(b["text"].as_str().unwrap_or("")),
            Some("thinking") => {
                out.reasoning
                    .get_or_insert_with(String::new)
                    .push_str(b["thinking"].as_str().unwrap_or(""));
            }
            Some("tool_use") => out.tool_calls.push(ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_string(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                arguments: b["input"].to_string(),
            }),
            _ => {}
        }
    }
    Ok(out)
}

fn usage(u: &Value, input_seen: u64) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    // With the prompt cache, `input_tokens` counts only the uncached part:
    // the prompt really sent is that plus what was read from / written to cache.
    let cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let cached = cache_read + cache_write;
    let input = u["input_tokens"].as_u64().unwrap_or(input_seen) + cached;
    let output = u["output_tokens"].as_u64().unwrap_or(0);
    Some(Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: input + output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    })
}

pub(crate) fn stream(
    client: &Client,
    req: &ChatRequest,
    cancel: &Cancel,
    on: &mut impl FnMut(StreamEvent),
) -> Result<()> {
    let resp = client.post("messages", &body_for(client, req, true), cancel)?;
    let mut state = AnthropicStream::default();
    crate::stream::for_each_data(resp, cancel, |data| state.feed(data, on))?;
    state.finish(on)
}

/// One Messages stream. Blocks are tracked by `index`, since a `tool_use`
/// block's JSON arrives as `input_json_delta` fragments addressed by index.
#[derive(Default)]
pub(crate) struct AnthropicStream {
    /// (index, call) for every tool_use block, in order.
    calls: Vec<(u64, ToolCall)>,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    usage: Option<Usage>,
    stop_reason: Option<String>,
    stopped: bool,
    events: usize,
}

impl AnthropicStream {
    pub(crate) fn feed(&mut self, data: &str, on: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        self.events += 1;
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str() {
            Some("message_start") => {
                // Cached prompt is reported apart: count it as input too and
                // retain cache counters when `message_delta` only reports output.
                if let Some(u) = usage(&v["message"]["usage"], 0) {
                    self.input_tokens = u.prompt_tokens;
                    self.cache_read_tokens = u.cache_read_tokens;
                    self.cache_write_tokens = u.cache_write_tokens;
                    self.usage = Some(u);
                }
            }
            Some("content_block_start") => {
                let b = &v["content_block"];
                if b["type"] == "tool_use" {
                    self.calls.push((
                        index,
                        ToolCall {
                            id: b["id"].as_str().unwrap_or_default().to_string(),
                            name: b["name"].as_str().unwrap_or_default().to_string(),
                            arguments: String::new(),
                        },
                    ));
                }
            }
            Some("content_block_delta") => {
                let d = &v["delta"];
                let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
                match d["type"].as_str() {
                    Some("text_delta") => on(StreamEvent::Delta(s("text"))),
                    Some("thinking_delta") => on(StreamEvent::Reasoning(s("thinking"))),
                    Some("input_json_delta") => {
                        if let Some((_, c)) = self.calls.iter_mut().find(|(i, _)| *i == index) {
                            c.arguments.push_str(&s("partial_json"));
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(mut u) = usage(&v["usage"], self.input_tokens) {
                    if u.cache_read_tokens == 0 && self.cache_read_tokens > 0 {
                        u.cache_read_tokens = self.cache_read_tokens;
                    }
                    if u.cache_write_tokens == 0 && self.cache_write_tokens > 0 {
                        u.cache_write_tokens = self.cache_write_tokens;
                    }
                    self.usage = Some(u);
                }
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
            }
            Some("message_stop") => {
                self.stopped = true;
                return Ok(true);
            }
            Some("error") => return Err(crate::stream::stream_error(&v["error"])),
            _ => {}
        }
        Ok(false)
    }

    pub(crate) fn finish(self, on: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if self.events == 0 {
            return Err(ProviderError::Malformed(
                "o stream terminou sem nenhum evento válido".into(),
            ));
        }
        if !self.stopped {
            return Err(ProviderError::Transport(crate::stream::CUT.into()));
        }
        let calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .map(|(_, mut c)| {
                // A tool with no parameters streams no JSON at all.
                if c.arguments.trim().is_empty() {
                    c.arguments = "{}".into();
                }
                c
            })
            .collect();
        if !calls.is_empty() {
            on(StreamEvent::ToolCalls(calls));
        }
        on(StreamEvent::Done {
            usage: self.usage,
            stop_reason: self.stop_reason,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_subscription_opens_with_the_claude_code_identity() {
        let mut cfg = crate::ProviderConfig::new(crate::ProviderKind::Anthropic);
        cfg.auth = crate::AuthMode::Oauth;
        cfg.oauth = Some(crate::OAuthCredential {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: 4_000_000_000,
            account_id: None,
            tool: crate::OAuthTool::ClaudeCode,
        });
        let oauth = Client::new(cfg).unwrap();
        let req = ChatRequest::new(
            "m",
            vec![
                crate::ChatMessage::system("sou o zerith"),
                crate::ChatMessage::user("oi"),
            ],
        );
        let b = body_for(&oauth, &req, false);
        assert_eq!(b["system"][0]["text"], CLAUDE_CODE_IDENTITY);
        assert!(b["system"][0].get("cache_control").is_none());
        assert_eq!(b["system"][1]["text"], "sou o zerith");
        assert_eq!(
            b["system"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );

        // API key mode marks the static system prompt block for prompt caching.
        let key = Client::new(crate::ProviderConfig::new(crate::ProviderKind::Anthropic)).unwrap();
        let kb = body_for(&key, &req, false);
        assert_eq!(kb["system"][0]["text"], "sou o zerith");
        assert_eq!(
            kb["system"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }
    use crate::ChatMessage;

    #[test]
    fn the_tools_and_the_conversation_so_far_are_marked_for_the_prompt_cache() {
        let mut req = ChatRequest::new(
            "claude",
            vec![
                ChatMessage::system("static sys"),
                ChatMessage::user("a"),
                ChatMessage::assistant("b", vec![]),
                ChatMessage::user("c"),
            ],
        );
        req.tools = vec![
            crate::ToolDef {
                name: "x".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            },
            crate::ToolDef {
                name: "y".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            },
        ];
        let b = body(&req, false);
        let eph = json!({"type": "ephemeral"});
        assert_eq!(b["tools"][1]["cache_control"], eph);
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["system"][0]["cache_control"], eph);
        assert_eq!(b["messages"][2]["content"][0]["cache_control"], eph);
        assert!(b["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        // what was read from cache still counts as prompt and surfaces cache metrics
        let u = usage(
            &json!({"input_tokens": 10, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 90, "output_tokens": 5}),
            0,
        )
        .unwrap();
        assert_eq!((u.prompt_tokens, u.total_tokens), (1000, 1005));
        assert_eq!((u.cache_read_tokens, u.cache_write_tokens), (900, 90));
    }

    #[test]
    fn system_is_top_level_and_tool_results_merge_into_one_user_turn() {
        let req = ChatRequest::new(
            "claude",
            vec![
                ChatMessage::system("s"),
                ChatMessage::user("u"),
                ChatMessage::assistant(
                    "vou ler",
                    vec![
                        ToolCall {
                            id: "a".into(),
                            name: "read_file".into(),
                            arguments: "{\"path\":\"x\"}".into(),
                        },
                        ToolCall {
                            id: "b".into(),
                            name: "glob".into(),
                            arguments: "not json".into(),
                        },
                    ],
                ),
                ChatMessage::tool("a", "ra"),
                ChatMessage::tool("b", "rb"),
            ],
        );
        let b = body(&req, false);
        assert_eq!(b["system"][0]["text"], "s");
        assert_eq!(
            b["system"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(b["max_tokens"], 32000);
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][1]["input"]["path"], "x");
        assert_eq!(msgs[1]["content"][2]["input"], json!({}));
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][1]["tool_use_id"], "b");
    }

    #[test]
    fn a_blocking_answer_reads_text_thinking_and_tool_use() {
        let c = parse_completion(
            r#"{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"ok"},{"type":"tool_use","id":"t","name":"glob","input":{"pattern":"*"}}],"stop_reason":"tool_use","usage":{"input_tokens":4,"output_tokens":6}}"#,
        )
        .unwrap();
        assert_eq!(c.text, "ok");
        assert_eq!(c.reasoning.as_deref(), Some("hm"));
        assert_eq!(c.tool_calls[0].arguments, r#"{"pattern":"*"}"#);
        assert_eq!(c.usage.unwrap().total_tokens, 10);
    }

    #[test]
    fn the_output_cap_follows_the_model() {
        assert_eq!(max_output("claude-haiku-4.5"), 32000);
        assert_eq!(max_output("anthropic/claude-sonnet-4-5"), 32000);
        assert_eq!(max_output("claude-opus-5-5"), 32000);
        assert_eq!(max_output("claude-3-5-sonnet-latest"), 8192);
        assert_eq!(max_output("claude-3-haiku-20240307"), 4096);
        assert_eq!(max_output("MiniMax-M2"), 8192);
        let req = ChatRequest::new("claude-3-7-sonnet", vec![]).max_tokens(10);
        assert_eq!(body(&req, false)["max_tokens"], 10, "an explicit cap wins");
    }

    fn replay(events: &[&str]) -> (Result<()>, Vec<StreamEvent>) {
        let mut st = AnthropicStream::default();
        let mut ev = Vec::new();
        for d in events {
            if let Err(e) = st.feed(d, &mut |e| ev.push(e)) {
                return (Err(e), ev);
            }
        }
        let r = st.finish(&mut |e| ev.push(e));
        (r, ev)
    }

    const START: &str = r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#;
    const TOOL: &str = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"write_file"}}"#;
    const HALF: &str = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a"}}"#;

    #[test]
    fn max_tokens_mid_tool_use_is_reported_with_the_stop_reason() {
        let (r, ev) = replay(&[
            START,
            TOOL,
            HALF,
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        r.unwrap();
        assert!(matches!(
            ev.last(),
            Some(StreamEvent::Done { stop_reason: Some(s), .. }) if crate::is_cut_off(s)
        ));
    }

    #[test]
    fn a_stream_without_message_stop_is_cut_not_complete() {
        let (r, ev) = replay(&[START, TOOL, HALF]);
        assert!(
            matches!(r, Err(ProviderError::Transport(ref m)) if m == crate::stream::CUT),
            "{r:?}"
        );
        assert!(!ev.iter().any(|e| matches!(e, StreamEvent::ToolCalls(_))));
    }

    #[test]
    fn stream_errors_are_typed_for_failover() {
        let err = |t: &str| {
            replay(&[
                START,
                &format!(r#"{{"type":"error","error":{{"type":"{t}","message":"x"}}}}"#),
            ])
            .0
        };
        assert!(matches!(
            err("overloaded_error"),
            Err(ProviderError::Http { status: 529, .. })
        ));
        assert!(matches!(
            err("api_error"),
            Err(ProviderError::Http { status: 500, .. })
        ));
        assert!(matches!(
            err("rate_limit_error"),
            Err(ProviderError::RateLimited { .. })
        ));
        let e = err("overloaded_error").unwrap_err();
        assert!(crate::is_failover_error(&e));
        let h = crate::explain::headline(&e);
        assert!(h.contains("sobrecarregado") && !h.contains("/v1"), "{h}");
    }
}
