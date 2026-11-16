//! MCP wire types, as small as the spec allows.
//!
//! MCP is JSON-RPC 2.0 with `Content-Length` framing only in the LSP-style
//! transport; the stdio transport every server ships today uses one JSON
//! object per line. This crate speaks the line form, so a server that prints
//! a log line to stdout breaks the stream and says so loudly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Serialize)]
pub struct Request<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> Request<'a> {
    pub fn new(id: u64, method: &'a str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// One tool a server advertises.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsList {
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
}

/// A tool result.
///
/// Content blocks stay untyped on purpose: MCP keeps adding kinds (audio,
/// embedded resources), and a tagged enum would fail the whole call on a
/// block the model cannot read anyway. Unknown kinds are named, never dropped.
#[derive(Debug, Deserialize)]
pub struct CallResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub is_error: Option<bool>,
}

impl CallResult {
    /// Everything a model can read, joined. Non-text blocks are named so the
    /// agent knows something came back that it cannot see.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match kind {
                "text" => out.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
                "image" => {
                    let mime = block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image");
                    out.push_str(&format!("[image {mime} omitted]"));
                }
                other => out.push_str(&format!("[{other} block omitted]")),
            }
        }
        out
    }
}

/// One resource a server advertises (`resources/list`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ResourceInfo {
    pub uri: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResourcesList {
    #[serde(default)]
    pub resources: Vec<ResourceInfo>,
}

/// One block of `resources/read`. Text or base64 blob, never both.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceRead {
    #[serde(default)]
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

/// One prompt template a server advertises (`prompts/list`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Deserialize)]
pub struct PromptsList {
    #[serde(default)]
    pub prompts: Vec<PromptInfo>,
}

/// `prompts/get`: messages stay untyped for the same reason tool content does.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PromptGet {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub messages: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_and_prompts_parse_tolerantly() {
        let r: ResourcesList =
            serde_json::from_str(r#"{"resources":[{"uri":"file:///a","x":1}]}"#).unwrap();
        assert_eq!(r.resources[0].uri, "file:///a");
        let c: ResourceRead =
            serde_json::from_str(r#"{"contents":[{"uri":"u","text":"oi"}]}"#).unwrap();
        assert_eq!(c.contents[0].text.as_deref(), Some("oi"));
        let p: PromptsList = serde_json::from_str(
            r#"{"prompts":[{"name":"p","arguments":[{"name":"a","required":true}]}]}"#,
        )
        .unwrap();
        assert_eq!(p.prompts[0].arguments[0].required, Some(true));
        let g: PromptGet = serde_json::from_str(r#"{"messages":[{"role":"user"}]}"#).unwrap();
        assert_eq!(g.messages.len(), 1);
    }

    #[test]
    fn a_text_result_is_rendered_and_an_image_is_named() {
        let raw = r#"{"content":[{"type":"text","text":"ok"},{"type":"image","data":"x","mimeType":"image/png"}]}"#;
        let r: CallResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.text(), "ok[image image/png omitted]");
    }

    #[test]
    fn a_tool_list_without_schema_still_parses() {
        let r: ToolsList = serde_json::from_str(r#"{"tools":[{"name":"x"}]}"#).unwrap();
        assert_eq!(r.tools[0].name, "x");
        assert!(r.tools[0].input_schema.is_none());
    }
}
