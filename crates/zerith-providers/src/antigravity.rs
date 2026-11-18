//! `daily-cloudcode-pa.googleapis.com/v1internal`: what an Antigravity
//! subscription answers on, the same endpoint and headers 9router uses.
//!
//! Gemini's wire, wrapped: `{model, project?, request: {contents,
//! systemInstruction, tools, generationConfig}}`, answered as SSE where every
//! `data:` is `{response: {candidates, usageMetadata}}`. Only the `daily`
//! host lists Gemini 4 (Hydrogen, Argon); the stable one stops at 3.x.

use crate::{
    Cancel, ChatRequest, Client, Completion, ProviderError, Result, Role, StreamEvent, ToolCall,
    Usage,
};
use serde_json::{json, Value};

pub const BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
/// Without an Antigravity user agent the API answers 403 "no valid license".
pub const USER_AGENT: &str = "antigravity/hub/2.24.0 windows/amd64";
// Google's `resource:method`, so the version sits in the path, not the base.
const PATH: &str = "v1internal:streamGenerateContent?alt=sse";
/// Gemini 3+ refuses a replayed `functionCall` without a thought signature.
/// The real one is not kept across turns; this value is the documented way to
/// say so, and is what 9router sends too.
const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

pub(crate) fn body(req: &ChatRequest, project: Option<&str>) -> Value {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    // tool_call_id → name: a functionResponse must repeat the call's name
    let mut names = std::collections::HashMap::new();
    for m in &req.messages {
        match m.role {
            Role::System => system.push(json!({"text": m.content})),
            Role::User => {
                let mut parts = vec![json!({"text": m.content})];
                parts.extend(
                    crate::images::local_images(&m.content)
                        .iter()
                        .map(|i| json!({"inlineData": {"mimeType": i.mime, "data": i.data}})),
                );
                contents.push(json!({"role": "user", "parts": parts}));
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for c in &m.tool_calls {
                    names.insert(c.id.clone(), c.name.clone());
                    let args: Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                    parts.push(json!({
                        "functionCall": {"name": c.name, "args": args, "id": c.id},
                        "thoughtSignature": SKIP_SIGNATURE,
                    }));
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                let part = json!({"functionResponse": {
                    "name": names.get(&id).cloned().unwrap_or_default(),
                    "id": id,
                    "response": {"output": m.content},
                }});
                // parallel results travel together, in one user turn
                match contents.last_mut() {
                    Some(last)
                        if last["role"] == "user"
                            && last["parts"][0].get("functionResponse").is_some() =>
                    {
                        last["parts"].as_array_mut().unwrap().push(part)
                    }
                    _ => contents.push(json!({"role": "user", "parts": [part]})),
                }
            }
        }
    }
    let mut request = json!({"contents": contents});
    if !system.is_empty() {
        request["systemInstruction"] = json!({"role": "user", "parts": system});
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": clean_schema(t.parameters.clone()),
                })
            })
            .collect();
        request["tools"] = json!([{"functionDeclarations": decls}]);
    }
    let mut gen = json!({});
    if let Some(n) = req.max_tokens {
        gen["maxOutputTokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        gen["temperature"] = json!(t);
    }
    if gen.as_object().is_some_and(|o| !o.is_empty()) {
        request["generationConfig"] = gen;
    }
    let mut b = json!({"model": req.model, "userAgent": "antigravity", "request": request});
    if let Some(p) = project.filter(|p| !p.is_empty()) {
        b["project"] = json!(p);
    }
    b
}

/// The API is a protobuf behind JSON: it rejects `$schema` and a list in
/// `type`. Everything else a tool schema here uses is accepted as is.
fn clean_schema(mut v: Value) -> Value {
    match &mut v {
        Value::Object(o) => {
            o.remove("$schema");
            if let Some(Value::Array(ts)) = o.get("type") {
                let first = ts.iter().find(|t| *t != "null").cloned();
                match first {
                    Some(t) => o.insert("type".into(), t),
                    None => o.remove("type"),
                };
            }
            for x in o.values_mut() {
                *x = clean_schema(x.take());
            }
        }
        Value::Array(a) => {
            for x in a.iter_mut() {
                *x = clean_schema(x.take());
            }
        }
        _ => {}
    }
    v
}

pub(crate) fn complete(client: &Client, req: &ChatRequest, cancel: &Cancel) -> Result<Completion> {
    let mut out = Completion {
        text: String::new(),
        reasoning: None,
        usage: None,
        finish_reason: None,
        tool_calls: Vec::new(),
    };
    stream(client, req, cancel, &mut |e| match e {
        StreamEvent::Delta(d) => out.text.push_str(&d),
        StreamEvent::Reasoning(r) => out.reasoning.get_or_insert_with(String::new).push_str(&r),
        StreamEvent::ToolCalls(c) => out.tool_calls = c,
        StreamEvent::Done { usage } => out.usage = usage,
    })?;
    Ok(out)
}

pub(crate) fn stream(
    client: &Client,
    req: &ChatRequest,
    cancel: &Cancel,
    on_event: &mut impl FnMut(StreamEvent),
) -> Result<()> {
    let project = client
        .config()
        .oauth
        .as_ref()
        .and_then(|c| c.account_id.as_deref());
    let resp = client.post_stream(PATH, &body(req, project), cancel)?;
    let mut state = GeminiStream::default();
    crate::stream::for_each_data(resp, cancel, |data| state.feed(data, on_event))?;
    state.finish(on_event)
}

#[derive(Default)]
pub(crate) struct GeminiStream {
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    seen: usize,
}

impl GeminiStream {
    pub(crate) fn feed(&mut self, data: &str, on: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        self.seen += 1;
        let r = v.get("response").unwrap_or(&v);
        if let Some(e) = r.get("error") {
            return Err(ProviderError::Malformed(
                e["message"]
                    .as_str()
                    .unwrap_or("erro do antigravity")
                    .into(),
            ));
        }
        for part in r["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(f) = part.get("functionCall") {
                let n = self.calls.len();
                self.calls.push(ToolCall {
                    id: f["id"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{n}")),
                    name: f["name"].as_str().unwrap_or_default().to_string(),
                    arguments: f.get("args").unwrap_or(&json!({})).to_string(),
                });
            } else if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                on(if part["thought"] == true {
                    StreamEvent::Reasoning(t.to_string())
                } else {
                    StreamEvent::Delta(t.to_string())
                });
            }
        }
        if let Some(u) = r.get("usageMetadata") {
            let prompt = u["promptTokenCount"].as_u64().unwrap_or(0);
            let total = u["totalTokenCount"].as_u64().unwrap_or(0);
            self.usage = Some(Usage {
                prompt_tokens: prompt,
                completion_tokens: total.saturating_sub(prompt),
                total_tokens: total,
            });
        }
        Ok(false)
    }

    pub(crate) fn finish(self, on: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if self.seen == 0 {
            return Err(ProviderError::Malformed(
                "o stream do antigravity terminou sem nenhum evento".into(),
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

    #[test]
    fn a_tool_round_trip_becomes_gemini_contents() {
        let call = ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: r#"{"cmd":"echo hi"}"#.into(),
        };
        let req = ChatRequest::new(
            "gemini-4-hydrogen-medium",
            vec![
                ChatMessage::system("sys"),
                ChatMessage::user("oi"),
                ChatMessage::assistant("", vec![call]),
                ChatMessage::tool("c1", "hi"),
            ],
        )
        .tools(vec![ToolDef {
            name: "shell".into(),
            description: "d".into(),
            parameters: json!({"$schema": "x", "type": "object",
                "properties": {"a": {"type": ["string", "null"]}}}),
        }]);
        let b = body(&req, Some("proj"));
        assert_eq!(b["project"], "proj");
        let r = &b["request"];
        assert_eq!(r["systemInstruction"]["parts"][0]["text"], "sys");
        let fc = &r["contents"][1]["parts"][0];
        assert_eq!(fc["functionCall"]["args"]["cmd"], "echo hi");
        assert_eq!(fc["thoughtSignature"], SKIP_SIGNATURE);
        assert_eq!(
            r["contents"][2]["parts"][0]["functionResponse"]["name"],
            "shell"
        );
        let p = &r["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(p.get("$schema").is_none());
        assert_eq!(p["properties"]["a"]["type"], "string");
    }

    #[test]
    fn sse_becomes_text_reasoning_calls_and_usage() {
        let mut st = GeminiStream::default();
        let mut ev = Vec::new();
        for d in [
            r#"{"response":{"candidates":[{"content":{"parts":[{"thought":true,"text":"hm"}]}}]}}"#,
            r#"{"response":{"candidates":[{"content":{"parts":[{"text":"Olá"},{"functionCall":{"name":"glob","args":{"p":"*"},"id":"x"},"thoughtSignature":"s"}]}}],"usageMetadata":{"promptTokenCount":3,"totalTokenCount":10}}}"#,
        ] {
            st.feed(d, &mut |e| ev.push(e)).unwrap();
        }
        st.finish(&mut |e| ev.push(e)).unwrap();
        assert_eq!(ev[0], StreamEvent::Reasoning("hm".into()));
        assert_eq!(ev[1], StreamEvent::Delta("Olá".into()));
        let StreamEvent::ToolCalls(c) = &ev[2] else {
            panic!()
        };
        assert_eq!(
            (c[0].id.as_str(), c[0].arguments.as_str()),
            ("x", r#"{"p":"*"}"#)
        );
        assert!(matches!(&ev[3], StreamEvent::Done { usage: Some(u) } if u.completion_tokens == 7));
    }
}
