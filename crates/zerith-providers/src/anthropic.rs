//! Anthropic Messages API: `POST {base}/messages`.
//!
//! Differs from OpenAI where it matters: the system prompt is a top-level
//! field, messages must alternate user/assistant, tool calls are `tool_use`
//! content blocks and their results are `tool_result` blocks inside a *user*
//! message, and the stream is typed events instead of choice deltas.

use crate::openai::error_message;
use crate::{
    Cancel, ChatRequest, Client, Completion, ProviderError, Result, Role, StreamEvent, ToolCall,
    Usage,
};
use serde_json::{json, Value};

/// Anthropic requires `max_tokens`; this is what a request without one gets.
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub(crate) fn body(req: &ChatRequest, stream: bool) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        let (role, blocks) = match m.role {
            Role::System => {
                system.push(m.content.as_str());
                continue;
            }
            Role::User => ("user", vec![json!({"type": "text", "text": m.content})]),
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
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": stream,
    });
    if !system.is_empty() {
        b["system"] = json!(system.join("\n\n"));
    }
    if let Some(t) = req.temperature {
        b["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
            .collect();
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
        if let Some(own) = b.get("system").and_then(Value::as_str) {
            blocks.push(json!({"type": "text", "text": own}));
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
        return Err(ProviderError::Malformed(error_message(e)));
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
    let input = u["input_tokens"].as_u64().unwrap_or(input_seen);
    let output = u["output_tokens"].as_u64().unwrap_or(0);
    Some(Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: input + output,
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
    usage: Option<Usage>,
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
                self.input_tokens = v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
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
            Some("message_delta") => self.usage = usage(&v["usage"], self.input_tokens),
            Some("message_stop") => {
                self.stopped = true;
                return Ok(true);
            }
            Some("error") => return Err(ProviderError::Malformed(error_message(&v["error"]))),
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
        on(StreamEvent::Done { usage: self.usage });
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
        assert_eq!(b["system"][1]["text"], "sou o zerith");

        // an API key keeps the plain string: nothing to prove there
        let key = Client::new(crate::ProviderConfig::new(crate::ProviderKind::Anthropic)).unwrap();
        assert_eq!(body_for(&key, &req, false)["system"], "sou o zerith");
    }
    use crate::ChatMessage;

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
        assert_eq!(b["system"], "s");
        assert_eq!(b["max_tokens"], DEFAULT_MAX_TOKENS);
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
}
