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
#[cfg(test)]
mod tests {
    use super::*;

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
