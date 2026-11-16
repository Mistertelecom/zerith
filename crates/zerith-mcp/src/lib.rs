//! MCP client over stdio.
//!
//! A server is a child process speaking JSON-RPC, one object per line. The
//! client owns the handshake and the request/response pairing; nothing here
//! decides whether a tool call is allowed — that belongs to the permission
//! layer, and putting it here would hide it from the user.

pub mod protocol;

pub use protocol::{CallResult, RpcError, ToolInfo, ToolsList, PROTOCOL_VERSION};

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("spawn {cmd}: {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("server sent invalid json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("rpc {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("server closed the connection")]
    Closed,
}

pub type Result<T> = std::result::Result<T, McpError>;

/// A live server. Dropping it kills the child: a leaked MCP process would
/// outlive the window it serves.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
    pub name: String,
}

impl Client {
    /// Starts a server and completes the `initialize` handshake.
    ///
    /// `command` is the executable and `args` its arguments, exactly as the
    /// user configured them. No shell is involved, so a config value cannot
    /// become a shell command by accident.
    pub fn connect(name: &str, command: &str, args: &[String]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| McpError::Spawn {
                cmd: command.to_string(),
                source: e,
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Protocol("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Protocol("no stdout".into()))?;
        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            name: name.to_string(),
        };
        client.request(
            "initialize",
            Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "zerith", "version": zerith_core::VERSION },
            })),
        )?;
        client.notify("notifications/initialized")?;
        Ok(client)
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }))
    }

    /// Sends one request and reads until its answer arrives. A notification or
    /// a server-initiated request in between is skipped, not mistaken for the
    /// answer.
    pub fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = protocol::Request::new(id, method, params);
        self.send(&serde_json::to_value(&req)?)?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(McpError::Closed);
            }
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                // A server that writes a log line to stdout violates the
                // transport. Skipping keeps the session alive; hiding it would
                // make a broken server look healthy.
                Err(_) => continue,
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            let resp: protocol::Response = serde_json::from_value(v)?;
            if let Some(e) = resp.error {
                return Err(McpError::Rpc {
                    code: e.code,
                    message: e.message,
                });
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    pub fn tools(&mut self) -> Result<Vec<ToolInfo>> {
        let result = self.request("tools/list", None)?;
        let list: ToolsList = serde_json::from_value(result)?;
        Ok(list.tools)
    }

    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<CallResult> {
        let result = self.request(
            "tools/call",
            Some(json!({ "name": tool, "arguments": arguments })),
        )?;
        Ok(serde_json::from_value(result)?)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
