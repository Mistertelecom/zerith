//! `chatgpt.com/backend-api/codex`: the Responses API a Codex subscription
//! answers on.
//!
//! Different wire than `chat/completions`: a flat `input` array, tools without
//! a `function` wrapper, and a typed SSE (`response.output_text.delta`,
//! `response.output_item.done`, `response.completed`) instead of choice deltas.

use crate::{
    Cancel, ChatRequest, Client, Completion, ProviderError, Result, Role, StreamEvent, ToolCall,
    Usage,
};
use serde_json::{json, Value};

const PATH: &str = "responses";

pub(crate) fn body(req: &ChatRequest, stream: bool) -> Value {
    let mut instructions = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => instructions.push(m.content.clone()),
            Role::User => {
                let mut content = vec![json!({"type": "input_text", "text": m.content})];
                content.extend(
                    crate::images::local_images(&m.content)
                        .iter()
                        .map(|i| json!({"type": "input_image", "image_url": i.data_url()})),
                );
                input.push(json!({"type": "message", "role": "user", "content": content}));
            }
            Role::Assistant => {
                if !m.content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": m.content}],
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        "arguments": c.arguments,
                    }));
                }
            }
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content,
            })),
        }
    }

    let mut b = json!({
        "model": req.model,
        "input": input,
        "stream": stream,
        // The subscription endpoint keeps no server-side conversation: the whole
        // history travels in `input` every turn.
        "store": false,
        "instructions": instructions.join("\n\n"),
    });
    if !req.tools.is_empty() {
        b["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
    }
    if let Some(t) = req.temperature {
        b["temperature"] = json!(t);
    }
    b
}

pub(crate) fn complete(client: &Client, req: &ChatRequest, cancel: &Cancel) -> Result<Completion> {
    let resp = client.post(PATH, &body(req, false), cancel)?;
    let text = resp
        .into_string()
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    parse_completion(&text)
}

pub(crate) fn stream(
    client: &Client,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut impl FnMut(StreamEvent),
) -> Result<()> {
    let resp = client.post_stream(PATH, &body(req, true), cancel)?;
    let mut state = CodexStream::default();
    crate::stream::for_each_data(resp, cancel, |data| state.feed(data, on_event))?;
    state.finish(on_event)
}

fn parse_completion(text: &str) -> Result<Completion> {
    let v: Value = serde_json::from_str(text)?;
    if let Some(e) = v.get("error") {
        return Err(ProviderError::Malformed(crate::openai::error_message(e)));
    }
    let mut out = Completion {
        text: String::new(),
        reasoning: None,
        usage: usage(&v["usage"]),
        finish_reason: None,
        tool_calls: Vec::new(),
    };
    for item in v["output"].as_array().into_iter().flatten() {
        match item["type"].as_str().unwrap_or("") {
            "message" => {
                for c in item["content"].as_array().into_iter().flatten() {
                    if let Some(t) = c["text"].as_str() {
                        out.text.push_str(t);
                    }
                }
            }
            "function_call" => out.tool_calls.push(ToolCall {
                id: item["call_id"].as_str().unwrap_or_default().to_string(),
                name: item["name"].as_str().unwrap_or_default().to_string(),
                arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
            }),
            _ => {}
        }
    }
    if out.text.is_empty() && out.tool_calls.is_empty() {
        return Err(ProviderError::Malformed("resposta sem output".into()));
    }
    Ok(out)
}

fn usage(v: &Value) -> Option<Usage> {
    (v.is_object()).then(|| Usage {
        prompt_tokens: v["input_tokens"].as_u64().unwrap_or(0),
        completion_tokens: v["output_tokens"].as_u64().unwrap_or(0),
        total_tokens: v["total_tokens"].as_u64().unwrap_or(0),
    })
}

/// Accumulates one Responses stream. Text is forwarded as it arrives; tool calls
/// are collected from `response.output_item.done` and emitted once before
/// `Done`, so no caller ever sees half an argument string.
#[derive(Default)]
pub(crate) struct CodexStream {
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    done: bool,
    seen: usize,
}

impl CodexStream {
    pub(crate) fn feed(&mut self, data: &str, on: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        // A keep-alive or a comment between events is not an error.
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        self.seen += 1;
        match v["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(d) = v["delta"].as_str().filter(|d| !d.is_empty()) {
                    on(StreamEvent::Delta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(d) = v["delta"].as_str().filter(|d| !d.is_empty()) {
                    on(StreamEvent::Reasoning(d.to_string()));
                }
            }
            "response.output_item.done" => {
                let item = &v["item"];
                if item["type"] == "function_call" {
                    self.calls.push(ToolCall {
                        id: item["call_id"].as_str().unwrap_or_default().to_string(),
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
                    });
                }
            }
            "response.completed" => {
                self.usage = usage(&v["response"]["usage"]).or(self.usage.take());
                self.done = true;
                return Ok(true);
            }
            "response.failed" | "error" => {
                return Err(ProviderError::Malformed(
                    v["response"]["error"]["message"]
                        .as_str()
                        .or_else(|| v["message"].as_str())
                        .unwrap_or("stream codex falhou")
                        .to_string(),
                ));
            }
            _ => {}
        }
        Ok(false)
    }

    pub(crate) fn finish(self, on: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if self.seen == 0 {
            return Err(ProviderError::Malformed(
                "o stream do codex terminou sem nenhum evento".into(),
            ));
        }
        if !self.done {
            return Err(ProviderError::Malformed(
                "o stream do codex terminou sem response.completed".into(),
            ));
        }
        if !self.calls.is_empty() {
            on(StreamEvent::ToolCalls(self.calls));
        }
        on(StreamEvent::Done { usage: self.usage });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatMessage, ToolDef};

    fn replay(sse: &str) -> Vec<StreamEvent> {
        let mut st = CodexStream::default();
        let mut events = Vec::new();
        for line in sse.lines().filter_map(|l| l.strip_prefix("data: ")) {
            st.feed(line, &mut |e| events.push(e)).unwrap();
        }
        st.finish(&mut |e| events.push(e)).unwrap();
        events
    }

    #[test]
    fn sse_becomes_text_tool_calls_and_done() {
        let sse = include_str!("../tests/fixtures/codex.sse");
        let events = replay(sse);
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Delta(d) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Olá mundo");
        let calls = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCalls(c) => Some(c.clone()),
                _ => None,
            })
            .expect("tool call");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert_eq!(calls[0].id, "call_1");
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done { usage: Some(u) }) if u.total_tokens == 13
        ));
    }

    #[test]
    fn a_stream_without_completion_is_an_error_not_a_short_answer() {
        let sse = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"a\"}";
        let mut st = CodexStream::default();
        let mut ev = Vec::new();
        for line in sse.lines().filter_map(|l| l.strip_prefix("data: ")) {
            st.feed(line, &mut |e| ev.push(e)).unwrap();
        }
        assert!(st.finish(&mut |e| ev.push(e)).is_err());
    }

    #[test]
    fn a_failed_event_surfaces_the_provider_message() {
        let mut st = CodexStream::default();
        let mut ev = Vec::new();
        let err = st
            .feed(
                r#"{"type":"response.failed","response":{"error":{"message":"quota"}}}"#,
                &mut |e| ev.push(e),
            )
            .unwrap_err();
        assert!(err.to_string().contains("quota"));
    }

    #[test]
    fn body_uses_instructions_and_flat_tools() {
        let req = ChatRequest::new("gpt-5-codex", vec![ChatMessage::system("sys")])
            .max_tokens(10)
            .tools(vec![ToolDef {
                name: "read_file".into(),
                description: "lê".into(),
                parameters: json!({"type": "object"}),
            }]);
        let b = body(&req, true);
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "read_file");
        assert_eq!(b["stream"], true);
        assert_eq!(b["store"], false);
        assert!(b["input"].is_array());
    }

    #[test]
    fn a_tool_result_travels_as_function_call_output() {
        let req = ChatRequest::new(
            "m",
            vec![
                ChatMessage::user("oi"),
                ChatMessage::assistant(
                    "",
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                ),
                ChatMessage::tool("c1", "conteudo"),
            ],
        );
        let b = body(&req, false);
        assert_eq!(b["input"][1]["type"], "function_call");
        assert_eq!(b["input"][2]["type"], "function_call_output");
        assert_eq!(b["input"][2]["call_id"], "c1");
    }

    #[test]
    fn a_blocking_answer_with_a_tool_call_parses() {
        let c = parse_completion(
            r#"{"output":[{"type":"message","content":[{"text":"ok"}]},{"type":"function_call","call_id":"x","name":"glob","arguments":"{}"}],"usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}"#,
        )
        .unwrap();
        assert_eq!(c.text, "ok");
        assert_eq!(c.tool_calls[0].name, "glob");
        assert_eq!(c.usage.unwrap().total_tokens, 3);
    }

    #[test]
    fn an_empty_output_is_malformed() {
        assert!(matches!(
            parse_completion(r#"{"output":[]}"#),
            Err(ProviderError::Malformed(_))
        ));
    }
}
