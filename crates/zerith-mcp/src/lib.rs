//! MCP client over stdio or Streamable HTTP.
//!
//! A stdio server is a child process speaking JSON-RPC, one object per line.
//! An HTTP server takes one POST per message and answers with a JSON body or a
//! short SSE stream. The client owns the handshake and the request/response
//! pairing; nothing here decides whether a tool call is allowed — that belongs
//! to the permission layer, and putting it here would hide it from the user.

pub mod config;
pub mod oauth;
pub mod protocol;
pub mod tools;

pub use config::{load_servers, Extra, Loaded, Origin, ServerConfig};
pub use protocol::{
    CallResult, PromptArgument, PromptGet, PromptInfo, ResourceContent, ResourceInfo, RpcError,
    ToolInfo, ToolsList, PROTOCOL_VERSION,
};
pub use tools::{parse_mentions, McpRegistry, Mention, PromptCommand};

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long one request may wait for its answer before the server is
/// considered hung.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("server did not answer within {0:?}")]
    Timeout(Duration),
    #[error("http: {0}")]
    Http(String),
}

pub type Result<T> = std::result::Result<T, McpError>;

enum Transport {
    Stdio {
        child: Child,
        stdin: ChildStdin,
        /// Lines from a reader thread, so a silent server cannot block us.
        lines: Receiver<String>,
    },
    Http {
        agent: ureq::Agent,
        url: String,
        session: Option<String>,
        headers: Vec<(String, String)>,
        /// `auth = "oauth"` servers: bearer tokens from the vault.
        auth: Option<oauth::Auth>,
    },
}

/// A live server. Dropping it kills a stdio child: a leaked MCP process would
/// outlive the window it serves.
pub struct Client {
    transport: Transport,
    next_id: AtomicU64,
    timeout: Duration,
    pub name: String,
}

impl Client {
    /// Starts a server and completes the `initialize` handshake.
    ///
    /// `command` is the executable and `args` its arguments, exactly as the
    /// user configured them. No shell is involved, so a config value cannot
    /// become a shell command by accident.
    pub fn connect(name: &str, command: &str, args: &[String]) -> Result<Self> {
        Self::connect_with_timeout(name, command, args, DEFAULT_TIMEOUT)
    }

    pub fn connect_with_timeout(
        name: &str,
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_env(name, command, args, &[], timeout)
    }

    /// Like `connect_with_timeout`, with extra environment for the child.
    /// `npx`-style names resolve through `PATHEXT` on Windows (`npx.cmd`).
    pub fn connect_env(
        name: &str,
        command: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self> {
        let program = which(command).unwrap_or_else(|| command.into());
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| McpError::Spawn {
                cmd: command.to_string(),
                source: e,
            })?;
        let (stdin, stdout) = match (child.stdin.take(), child.stdout.take()) {
            (Some(i), Some(o)) => (i, o),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(McpError::Protocol("no stdio pipes".into()));
            }
        };
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self::handshake(Self {
            transport: Transport::Stdio {
                child,
                stdin,
                lines,
            },
            next_id: AtomicU64::new(1),
            timeout,
            name: name.to_string(),
        })
    }

    /// Connects to a Streamable HTTP endpoint (one URL, POST per message).
    pub fn connect_http(name: &str, url: &str) -> Result<Self> {
        Self::connect_http_with(name, url, &[])
    }

    /// Streamable HTTP with extra request headers (an `Authorization` token).
    pub fn connect_http_with(name: &str, url: &str, headers: &[(String, String)]) -> Result<Self> {
        Self::connect_http_auth(name, url, headers, None)
    }

    /// Streamable HTTP with an OAuth bearer token on every request.
    pub fn connect_http_auth(
        name: &str,
        url: &str,
        headers: &[(String, String)],
        auth: Option<oauth::Auth>,
    ) -> Result<Self> {
        Self::handshake(Self {
            transport: Transport::Http {
                agent: ureq::AgentBuilder::new().build(),
                url: url.to_string(),
                session: None,
                headers: headers.to_vec(),
                auth,
            },
            next_id: AtomicU64::new(1),
            timeout: DEFAULT_TIMEOUT,
            name: name.to_string(),
        })
    }

    fn handshake(mut client: Self) -> Result<Self> {
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

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        match &mut self.transport {
            Transport::Stdio { stdin, .. } => write_line(stdin, &msg),
            // The spec answers a notification with 202 and no body.
            Transport::Http { .. } => self.post(&msg).map(|_| ()),
        }
    }

    /// Sends one request and waits until its answer arrives. A notification or
    /// a server-initiated request in between is skipped, not mistaken for the
    /// answer. A server that stays silent past the timeout is killed.
    pub fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = serde_json::to_value(protocol::Request::new(id, method, params))?;
        let timeout = self.timeout;
        match &mut self.transport {
            Transport::Stdio {
                child,
                stdin,
                lines,
            } => {
                write_line(stdin, &req)?;
                let deadline = Instant::now() + timeout;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match lines.recv_timeout(left) {
                        Ok(line) => {
                            if let Some(r) = match_response(&line, id) {
                                return r;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(McpError::Timeout(timeout));
                        }
                        Err(RecvTimeoutError::Disconnected) => return Err(McpError::Closed),
                    }
                }
            }
            Transport::Http { .. } => {
                let resp = self.post(&req)?;
                let is_sse = resp.content_type() == "text/event-stream";
                let found = if is_sse {
                    read_sse(BufReader::new(resp.into_reader()), id)
                } else {
                    match_response(&resp.into_string()?, id)
                };
                found.unwrap_or_else(|| {
                    Err(McpError::Protocol(format!("no response for request {id}")))
                })
            }
        }
    }

    /// One POST. Captures `Mcp-Session-Id` from the server and resends it.
    /// With OAuth, a 401 refreshes the token and retries once.
    fn post(&mut self, msg: &Value) -> Result<ureq::Response> {
        let timeout = self.timeout;
        let Transport::Http {
            agent,
            url,
            session,
            headers,
            auth,
        } = &mut self.transport
        else {
            return Err(McpError::Protocol("not an http transport".into()));
        };
        let body = serde_json::to_string(msg)?;
        let mut retried = false;
        let resp = loop {
            let mut req = agent
                .post(url)
                .timeout(timeout)
                .set("Content-Type", "application/json")
                .set("Accept", "application/json, text/event-stream");
            for (k, v) in headers.iter() {
                req = req.set(k, v);
            }
            if let Some(a) = auth.as_ref() {
                let token = a.bearer(retried).map_err(McpError::Http)?;
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
            if let Some(s) = session.as_deref() {
                req = req.set("Mcp-Session-Id", s);
            }
            match req.send_string(&body) {
                Ok(r) => break r,
                Err(ureq::Error::Status(401, _)) if auth.is_some() && !retried => retried = true,
                Err(ureq::Error::Status(401, _)) if auth.is_some() => {
                    if let Some(a) = auth.as_ref() {
                        a.reject();
                    }
                    return Err(McpError::Http(oauth::EXPIRED.into()));
                }
                Err(ureq::Error::Status(code, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    return Err(McpError::Http(format!("status {code}: {body}")));
                }
                Err(e) => return Err(McpError::Http(e.to_string())),
            }
        };
        if let Some(s) = resp.header("Mcp-Session-Id") {
            *session = Some(s.to_string());
        }
        Ok(resp)
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

    pub fn resources_list(&mut self) -> Result<Vec<ResourceInfo>> {
        let result = self.request("resources/list", None)?;
        let list: protocol::ResourcesList = serde_json::from_value(result)?;
        Ok(list.resources)
    }

    pub fn resources_read(&mut self, uri: &str) -> Result<Vec<ResourceContent>> {
        let result = self.request("resources/read", Some(json!({ "uri": uri })))?;
        let read: protocol::ResourceRead = serde_json::from_value(result)?;
        Ok(read.contents)
    }

    pub fn prompts_list(&mut self) -> Result<Vec<PromptInfo>> {
        let result = self.request("prompts/list", None)?;
        let list: protocol::PromptsList = serde_json::from_value(result)?;
        Ok(list.prompts)
    }

    pub fn prompts_get(&mut self, name: &str, arguments: Value) -> Result<PromptGet> {
        let result = self.request(
            "prompts/get",
            Some(json!({ "name": name, "arguments": arguments })),
        )?;
        Ok(serde_json::from_value(result)?)
    }

    /// Child pid for stdio; 0 for HTTP, which has no local process.
    pub fn pid(&self) -> u32 {
        match &self.transport {
            Transport::Stdio { child, .. } => child.id(),
            Transport::Http { .. } => 0,
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Transport::Stdio { child, .. } = &mut self.transport {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Where `command` would run from: an existing path as is, else the first
/// `PATH` hit, trying `PATHEXT` (`.exe`, `.cmd`...) on Windows. `None` when it
/// is nowhere, which is what "comando não encontrado" means.
pub fn which(command: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(command);
    let exts: Vec<String> = if cfg!(windows) && p.extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    let hit = |base: &std::path::Path| {
        exts.iter()
            .map(|e| std::path::PathBuf::from(format!("{}{e}", base.display())))
            .find(|c| c.is_file())
    };
    if p.components().count() > 1 || p.is_absolute() {
        return hit(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| hit(&dir.join(command)))
}

fn write_line(w: &mut impl Write, msg: &Value) -> Result<()> {
    let line = serde_json::to_string(msg)?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}

/// The answer to request `id`, if `line` is one. Blank lines, log lines a
/// server wrongly prints to stdout, notifications and other ids are `None`:
/// skipping keeps the session alive.
pub fn match_response(line: &str, id: u64) -> Option<Result<Value>> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    let resp: protocol::Response = match serde_json::from_value(v) {
        Ok(r) => r,
        Err(e) => return Some(Err(e.into())),
    };
    Some(match resp.error {
        Some(e) => Err(McpError::Rpc {
            code: e.code,
            message: e.message,
        }),
        None => Ok(resp.result.unwrap_or(Value::Null)),
    })
}

/// Reads an SSE stream until the event answering `id` arrives. Stops there,
/// so a server that keeps the stream open does not hang the request.
pub fn read_sse(r: impl BufRead, id: u64) -> Option<Result<Value>> {
    let mut data = String::new();
    for line in r
        .lines()
        .map_while(std::result::Result::ok)
        .chain(Some(String::new()))
    {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                if let Some(found) = match_response(&data, id) {
                    return Some(found);
                }
                data.clear();
            }
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_matching_id_is_an_answer() {
        assert!(match_response("", 1).is_none());
        assert!(match_response("starting server...", 1).is_none());
        assert!(match_response(r#"{"jsonrpc":"2.0","method":"notifications/x"}"#, 1).is_none());
        assert!(match_response(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 1).is_none());
        let ok = match_response(r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#, 1).unwrap();
        assert_eq!(ok.unwrap()["a"], 1);
        let err = match_response(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#,
            1,
        )
        .unwrap();
        assert!(matches!(err, Err(McpError::Rpc { code: -32601, .. })));
    }

    #[test]
    fn sse_skips_other_events_and_joins_data_lines() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n\n\
                    : comment\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":7,\"result\":{\"ok\":true}}\r\n\r\n";
        let v = read_sse(body.as_bytes(), 7).unwrap().unwrap();
        assert_eq!(v["ok"], true);
        // Last event without a trailing blank line still counts.
        let tail = "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":1}";
        assert_eq!(read_sse(tail.as_bytes(), 3).unwrap().unwrap(), 1);
        assert!(read_sse("data: {}\n\n".as_bytes(), 3).is_none());
    }
}
