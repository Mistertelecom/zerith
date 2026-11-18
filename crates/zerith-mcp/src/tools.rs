//! MCP servers inside the agent loop.
//!
//! [`McpRegistry`] owns one lazily connected [`Client`] per configured server
//! and registers every tool a server advertises as `mcp__<server>__<tool>`
//! with the server's own JSON schema, implementing `zerith_tools::Tool`.
//!
//! Lifecycle, stated plainly:
//! - nothing spawns when the registry is built;
//! - [`McpRegistry::register_into`] connects each server once, because tool
//!   names and schemas only exist after `tools/list`. A server that fails is
//!   reported and contributes no tools; the others still register;
//! - the connection then stays alive and is shared by every call (a mutex
//!   serialises them: one stdio pipe, one request at a time);
//! - a call that finds the server dead (crash, timeout kill) drops the
//!   connection and the *next* call spawns a fresh one;
//! - dropping the registry (and the tools holding its servers) kills stdio
//!   children, via `Client::drop`.
//!
//! Permissions: an MCP tool can do anything its server can, so every call is a
//! `Network` action in policy terms and the tool name is the rule subject
//! (`ask = ["mcp__github__*"]` works like any other rule).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_core::Paths;
use zerith_security::Action;
use zerith_tools::{CallCtx, Exec, Permission, Registry, Result, Tool, ToolError, ToolSpec};

use crate::config::{load_servers, Extra, Loaded, ServerConfig};
use crate::{Client, McpError, ToolInfo, DEFAULT_TIMEOUT};

/// Largest resource text one `@server:resource` mention may inject.
pub const RESOURCE_CAP_CHARS: usize = 100_000;

/// Providers accept `^[a-zA-Z0-9_-]{1,64}$` as a function name.
const NAME_CAP: usize = 64;

/// `mcp__<server>__<tool>`, cleaned to what providers accept and cut to 64.
pub fn tool_name(server: &str, tool: &str) -> String {
    let clean: String = tool
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut name = format!("mcp__{server}__{clean}");
    name.truncate(NAME_CAP);
    name
}

/// One configured server and its (possibly absent) live connection.
struct Server {
    cfg: ServerConfig,
    extra: Extra,
    client: Mutex<Option<Client>>,
}

impl Server {
    fn timeout(&self) -> Duration {
        self.cfg
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT)
    }

    fn connect(&self) -> std::result::Result<Client, McpError> {
        let timeout = self.timeout();
        let pairs = |m: &std::collections::BTreeMap<String, String>| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        };
        match (&self.cfg.command, &self.cfg.url) {
            (Some(cmd), _) => Client::connect_env(
                &self.cfg.name,
                cmd,
                &self.cfg.args,
                &pairs(&self.extra.env),
                timeout,
            ),
            (None, Some(url)) => {
                let auth = self
                    .cfg
                    .is_oauth()
                    .then(|| crate::oauth::Auth::os(&self.cfg.name));
                let mut c = Client::connect_http_auth(
                    &self.cfg.name,
                    url,
                    &pairs(&self.extra.headers),
                    auth,
                )?;
                c.set_timeout(timeout);
                Ok(c)
            }
            (None, None) => Err(McpError::Protocol("servidor sem command nem url".into())),
        }
    }

    /// Runs `f` on the live connection, connecting first when there is none.
    /// A transport failure discards the connection so the next call reconnects;
    /// an `Rpc` error is the server answering, so the connection stays.
    fn with<R>(
        &self,
        f: impl FnOnce(&mut Client) -> std::result::Result<R, McpError>,
    ) -> std::result::Result<R, McpError> {
        let mut slot = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(self.connect()?);
        }
        let Some(client) = slot.as_mut() else {
            return Err(McpError::Closed);
        };
        let result = f(client);
        if matches!(
            result,
            Err(McpError::Closed
                | McpError::Timeout(_)
                | McpError::Io(_)
                | McpError::Json(_)
                | McpError::Http(_))
        ) {
            *slot = None;
        }
        result
    }
}

/// A server tool as the agent loop sees it.
struct McpTool {
    spec: ToolSpec,
    server: Arc<Server>,
    /// The name the server knows, before sanitising.
    remote: String,
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn action(&self, _args: &Value, _ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        Ok(Some(Action::Network(format!(
            "mcp:{}/{}",
            self.server.cfg.name, self.remote
        ))))
    }

    fn run(&self, args: &Value, _ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        let args = match args {
            Value::Null => json!({}),
            Value::Object(_) => args.clone(),
            other => {
                return Err(ToolError::BadArguments(format!(
                    "{}: os argumentos devem ser um objeto JSON, não {other}",
                    self.spec.name
                )))
            }
        };
        exec.check()?;
        let result = self
            .server
            .with(|c| c.call(&self.remote, args))
            .map_err(|e| ToolError::Failed(format!("MCP {}: {e}", self.server.cfg.name)))?;
        let text = result.text();
        if result.is_error == Some(true) {
            return Err(ToolError::Failed(text));
        }
        Ok(text)
    }
}

/// A prompt a server offers, ready to list as a slash command.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptCommand {
    pub server: String,
    pub prompt: String,
    /// `/mcp__<server>__<prompt>`
    pub command: String,
    pub description: String,
    /// `(name, required)` for each argument, in the server's order.
    pub arguments: Vec<(String, bool)>,
}

/// `@server:resource` found in user text. `resource` is a URI or a resource
/// name as `resources/list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub server: String,
    pub resource: String,
    /// The text as typed, to replace or strip.
    pub raw: String,
}

/// Finds `@server:resource` tokens. A token starts at `@` at the start of the
/// text or after whitespace (so an e-mail address is not a mention), names a
/// server with `[A-Za-z0-9_-]`, then `:`, then everything up to whitespace,
/// minus trailing sentence punctuation.
pub fn parse_mentions(text: &str) -> Vec<Mention> {
    let mut out = Vec::new();
    let mut prev_ws = true;
    for (i, c) in text.char_indices() {
        if c == '@' && prev_ws {
            let rest = &text[i + 1..];
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = rest[..end].trim_end_matches(['.', ',', ';', '!', '?', ')', ']']);
            if let Some((server, resource)) = token.split_once(':') {
                let ok = !server.is_empty()
                    && !resource.is_empty()
                    && server
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if ok {
                    out.push(Mention {
                        server: server.to_string(),
                        resource: resource.to_string(),
                        raw: format!("@{token}"),
                    });
                }
            }
        }
        prev_ws = c.is_whitespace();
    }
    out
}

/// What `register_into` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// Registered tool names.
    pub tools: Vec<String>,
    /// One line per server or tool that could not be used.
    pub errors: Vec<String>,
}

/// The configured servers of this process.
pub struct McpRegistry {
    servers: Vec<Arc<Server>>,
    /// Config problems and project servers still waiting for approval.
    pub notes: Vec<String>,
}

impl McpRegistry {
    pub fn new(configs: Vec<ServerConfig>) -> Self {
        Self {
            servers: configs
                .into_iter()
                .map(|cfg| {
                    Arc::new(Server {
                        cfg,
                        extra: Extra::default(),
                        client: Mutex::new(None),
                    })
                })
                .collect(),
            notes: Vec::new(),
        }
    }

    /// From settings files (`[[mcp.servers]]`), user and project layers.
    /// Project-defined servers need `zerith mcp approve` first; they show up
    /// in `notes`, never in `register_into`.
    pub fn load(paths: &Paths, project_dir: Option<&std::path::Path>) -> Self {
        let Loaded {
            servers,
            unapproved,
            errors,
            extras,
            ..
        } = load_servers(paths, project_dir);
        let mut reg = Self::new(servers);
        reg.with_extras(&extras);
        reg.notes = errors;
        for s in unapproved {
            reg.notes.push(format!(
                "servidor MCP {} vem do projeto e não foi aprovado: rode `zerith mcp approve {}`",
                s.name, s.name
            ));
        }
        reg
    }

    /// Adds servers that did not come from the settings files (plugin
    /// bundles). A server whose name is already present is skipped: the
    /// settings file wins.
    pub fn extend(&mut self, configs: Vec<ServerConfig>) {
        for cfg in configs {
            if self.servers.iter().any(|s| s.cfg.name == cfg.name) {
                continue;
            }
            self.servers.push(Arc::new(Server {
                cfg,
                extra: Extra::default(),
                client: Mutex::new(None),
            }));
        }
    }

    /// Attaches env/headers by server name (before anything connects).
    pub fn with_extras(&mut self, extras: &std::collections::BTreeMap<String, Extra>) {
        for s in &mut self.servers {
            if let (Some(x), Some(srv)) = (extras.get(&s.cfg.name), Arc::get_mut(s)) {
                srv.extra = x.clone();
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn server_names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.cfg.name.as_str()).collect()
    }

    /// Connects every server (first use), then adds its tools to `registry`
    /// under `mcp__<server>__<tool>`. Never fails as a whole.
    ///
    /// The host builds the `McpRegistry` once and keeps it for the life of the
    /// session: the connections live as long as it does. Registering twice
    /// reuses the live connections and replaces the same-named tools.
    pub fn register_into(&self, registry: &mut Registry) -> Registered {
        let mut done = Registered::default();
        for server in &self.servers {
            let listed = server.with(|c| c.tools());
            let infos: Vec<ToolInfo> = match listed {
                Ok(t) => t,
                Err(e) => {
                    done.errors
                        .push(format!("servidor MCP {}: {e}", server.cfg.name));
                    continue;
                }
            };
            let mut seen: Vec<String> = Vec::new();
            for info in infos {
                let name = tool_name(&server.cfg.name, &info.name);
                if seen.contains(&name) {
                    done.errors.push(format!(
                        "servidor MCP {}: a ferramenta {} colide com outra após ajustar o nome para {name}; ignorada",
                        server.cfg.name, info.name
                    ));
                    continue;
                }
                seen.push(name.clone());
                registry.register(Box::new(McpTool {
                    spec: ToolSpec {
                        name: name.clone(),
                        description: info.description.clone().unwrap_or_else(|| {
                            format!(
                                "Ferramenta {} do servidor MCP {}",
                                info.name, server.cfg.name
                            )
                        }),
                        parameters: info
                            .input_schema
                            .clone()
                            .filter(Value::is_object)
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        returns: "texto devolvido pelo servidor MCP",
                        permission: Permission::Network,
                        timeout: server.timeout(),
                        // `Client::request` blocks until the answer or its own
                        // timeout; a cancel is seen only between calls.
                        cancellable: false,
                        local: !server.cfg.is_http(),
                        source: format!("mcp:{}", server.cfg.name),
                    },
                    server: Arc::clone(server),
                    remote: info.name,
                }));
                done.tools.push(name);
            }
        }
        done
    }

    fn server(&self, name: &str) -> std::result::Result<&Arc<Server>, McpError> {
        self.servers
            .iter()
            .find(|s| s.cfg.name == name)
            .ok_or_else(|| McpError::Protocol(format!("servidor MCP desconhecido: {name}")))
    }

    /// The raw `tools/list` of one configured server.
    pub fn list_tools(&self, server: &str) -> std::result::Result<Vec<ToolInfo>, McpError> {
        self.server(server)?.with(|c| c.tools())
    }

    /// One `tools/call` on a configured server, outside the agent loop (the
    /// CLI's `mcp call`). No policy check: the user typed it.
    pub fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> std::result::Result<crate::CallResult, McpError> {
        self.server(server)?.with(|c| c.call(tool, arguments))
    }

    /// Reads one resource, by URI or by the name `resources/list` reports.
    /// Text only; a binary blob is named, not inlined. Capped at
    /// [`RESOURCE_CAP_CHARS`].
    pub fn read_resource(
        &self,
        server: &str,
        resource: &str,
    ) -> std::result::Result<String, McpError> {
        let s = self.server(server)?;
        s.with(|c| {
            let uri = if resource.contains("://") {
                resource.to_string()
            } else {
                c.resources_list()?
                    .into_iter()
                    .find(|r| r.name.as_deref() == Some(resource) || r.uri == resource)
                    .map(|r| r.uri)
                    .unwrap_or_else(|| resource.to_string())
            };
            let mut text = String::new();
            for part in c.resources_read(&uri)? {
                match (part.text, part.blob) {
                    (Some(t), _) => text.push_str(&t),
                    (None, Some(_)) => text.push_str(&format!(
                        "[blob {} omitido]",
                        part.mime_type.as_deref().unwrap_or("binário")
                    )),
                    (None, None) => {}
                }
            }
            Ok(cap(text))
        })
    }

    /// Expands `@server:resource` mentions: the text is returned unchanged,
    /// followed by one `<recurso>` block per mention that resolved. A mention
    /// that fails is reported and left in the text, never silently dropped.
    pub fn expand_mentions(&self, text: &str) -> (String, Vec<String>) {
        let mut out = text.to_string();
        let mut errors = Vec::new();
        for m in parse_mentions(text) {
            // A server we do not know is probably not a mention at all.
            if self.server(&m.server).is_err() {
                continue;
            }
            match self.read_resource(&m.server, &m.resource) {
                Ok(body) => out.push_str(&format!(
                    "\n\n<recurso servidor=\"{}\" id=\"{}\">\n{body}\n</recurso>",
                    m.server, m.resource
                )),
                Err(e) => errors.push(format!("{}: {e}", m.raw)),
            }
        }
        (out, errors)
    }

    /// Prompts of every server that offers any, as slash-command entries.
    /// Servers that fail or do not support prompts are skipped.
    pub fn prompt_commands(&self) -> Vec<PromptCommand> {
        let mut out = Vec::new();
        for s in &self.servers {
            let Ok(list) = s.with(|c| c.prompts_list()) else {
                continue;
            };
            for p in list {
                out.push(PromptCommand {
                    server: s.cfg.name.clone(),
                    command: format!("/{}", tool_name(&s.cfg.name, &p.name)),
                    description: p.description.unwrap_or_default(),
                    arguments: p
                        .arguments
                        .iter()
                        .map(|a| (a.name.clone(), a.required.unwrap_or(false)))
                        .collect(),
                    prompt: p.name,
                });
            }
        }
        out
    }

    /// Expands a prompt to text: the messages of `prompts/get`, joined, each
    /// prefixed by its role.
    pub fn get_prompt(
        &self,
        server: &str,
        prompt: &str,
        arguments: Value,
    ) -> std::result::Result<String, McpError> {
        let got = self
            .server(server)?
            .with(|c| c.prompts_get(prompt, arguments))?;
        let mut out = String::new();
        for m in got.messages {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = &m["content"];
            let text = content
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let kind = content.get("type").and_then(Value::as_str).unwrap_or("?");
                    format!("[{kind} omitido]")
                });
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("[{role}] {text}"));
        }
        Ok(out)
    }
}

fn cap(s: String) -> String {
    if s.chars().count() <= RESOURCE_CAP_CHARS {
        return s;
    }
    let head: String = s.chars().take(RESOURCE_CAP_CHARS).collect();
    format!("{head}\n[recurso truncado em {RESOURCE_CAP_CHARS} caracteres]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_provider_safe_and_bounded() {
        assert_eq!(tool_name("gh", "list issues"), "mcp__gh__list_issues");
        assert_eq!(tool_name("gh", "a.b/c"), "mcp__gh__a_b_c");
        assert_eq!(tool_name("s", &"x".repeat(200)).len(), 64);
    }

    #[test]
    fn mentions_need_a_server_a_colon_and_a_word_boundary() {
        let m = parse_mentions("veja @docs:guia, e @fs:file:///a/b.txt. Mail a@b:c, @x: e @:y");
        assert_eq!(
            m.iter()
                .map(|m| (m.server.as_str(), m.resource.as_str()))
                .collect::<Vec<_>>(),
            [("docs", "guia"), ("fs", "file:///a/b.txt")]
        );
        assert_eq!(m[0].raw, "@docs:guia");
        assert!(parse_mentions("sem nada @aqui").is_empty());
    }
}
