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
use zerith_tools::{
    CallCtx, Exec, Permission, Registry, Result, Tool, ToolError, ToolSpec, DEFERRED_SOURCE,
};

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
    /// Cached `tools/list` response so warm registries re-populate fresh `Registry`
    /// instances without extra RPC round-trips or child respawns.
    cached_tools: Mutex<Option<Vec<ToolInfo>>>,
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
                Client::connect_http_auth_with_timeout(
                    &self.cfg.name,
                    url,
                    &pairs(&self.extra.headers),
                    auth,
                    timeout,
                )
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
            if let Ok(mut cache) = self.cached_tools.lock() {
                *cache = None;
            }
        }
        result
    }

    fn tools_cached(&self) -> std::result::Result<Vec<ToolInfo>, McpError> {
        if let Ok(cache) = self.cached_tools.lock() {
            if let Some(list) = cache.as_ref() {
                return Ok(list.clone());
            }
        }
        let tools = self.with(|c| c.tools())?;
        if let Ok(mut cache) = self.cached_tools.lock() {
            *cache = Some(tools.clone());
        }
        Ok(tools)
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
                        cached_tools: Mutex::new(None),
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
                cached_tools: Mutex::new(None),
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
        let mut listed: Vec<(&Arc<Server>, Vec<ToolInfo>)> = Vec::new();
        if self.servers.len() <= 1 {
            for server in &self.servers {
                match server.tools_cached() {
                    Ok(t) => listed.push((server, t)),
                    Err(e) => done
                        .errors
                        .push(format!("servidor MCP {}: {e}", server.cfg.name)),
                }
            }
        } else {
            type ProbeSlot = Mutex<Option<std::result::Result<Vec<ToolInfo>, McpError>>>;
            let mut results: Vec<Option<std::result::Result<Vec<ToolInfo>, McpError>>> =
                (0..self.servers.len()).map(|_| None).collect();
            let slots: Vec<ProbeSlot> = (0..self.servers.len()).map(|_| Mutex::new(None)).collect();
            std::thread::scope(|s| {
                for (i, server) in self.servers.iter().enumerate() {
                    let slot = &slots[i];
                    s.spawn(move || {
                        let res = server.tools_cached();
                        if let Ok(mut g) = slot.lock() {
                            *g = Some(res);
                        }
                    });
                }
            });
            for (i, slot) in slots.into_iter().enumerate() {
                results[i] = slot.into_inner().ok().flatten();
            }
            for (server, res) in self.servers.iter().zip(results) {
                match res {
                    Some(Ok(t)) => listed.push((server, t)),
                    Some(Err(e)) => done
                        .errors
                        .push(format!("servidor MCP {}: {e}", server.cfg.name)),
                    None => done.errors.push(format!(
                        "servidor MCP {}: inicialização interrompida",
                        server.cfg.name
                    )),
                }
            }
        }
        let sizes: Vec<(usize, usize)> = listed
            .iter()
            .map(|(_, t)| (t.len(), t.iter().map(weight).sum()))
            .collect();
        let defer = deferred_servers(&sizes);
        let mut hidden: Vec<McpTool> = Vec::new();
        let mut counts: Vec<(String, usize)> = Vec::new();
        for ((server, infos), defer) in listed.into_iter().zip(defer) {
            let before = hidden.len();
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
                let tool = McpTool {
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
                        source: if defer {
                            format!("{DEFERRED_SOURCE}mcp:{}", server.cfg.name)
                        } else {
                            format!("mcp:{}", server.cfg.name)
                        },
                    },
                    server: Arc::clone(server),
                    remote: info.name,
                };
                if defer {
                    hidden.push(tool);
                } else {
                    registry.register(Box::new(tool));
                }
                done.tools.push(name);
            }
            if defer {
                counts.push((server.cfg.name.clone(), hidden.len() - before));
            }
        }
        if hidden.is_empty() {
            return done;
        }
        let index = Arc::new(Deferred {
            tools: hidden
                .iter()
                .map(|t| {
                    (
                        t.spec.name.clone(),
                        t.spec.description.clone(),
                        t.spec.parameters.clone(),
                    )
                })
                .collect(),
            counts,
        });
        for t in hidden {
            registry.register(Box::new(t));
        }
        registry.register(Box::new(McpSearch {
            spec: search_spec(&index.counts),
            index: Arc::clone(&index),
        }));
        registry.register(Box::new(McpCall {
            spec: call_spec(&index.counts),
            index,
        }));
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

// ---- deferred servers (tool search) ----------------------------------------

/// ponytail: fixed budget, no setting. 40 tools / 40k chars of name +
/// description + schema (~10k tokens) is where MCP schemas start to outweigh
/// the whole rest of the prompt (~6k tokens). Measured: codebase-memory-mcp =
/// 17 tools / 21k chars (stays direct), rea = 170 tools / 415k chars (deferred).
/// Make it a setting if someone needs a different line.
pub const DIRECT_MAX_TOOLS: usize = 40;
pub const DIRECT_MAX_CHARS: usize = 40_000;

/// What one tool costs the request, in chars.
fn weight(t: &ToolInfo) -> usize {
    t.name.len()
        + t.description.as_ref().map_or(0, String::len)
        + t.input_schema.as_ref().map_or(0, |s| s.to_string().len())
}

/// Which servers (by `(tool count, chars)`) go behind `mcp_search`. Under the
/// budget nothing changes. Over it, the biggest servers are deferred first
/// until what stays direct fits, so a small server keeps its direct tools.
pub fn deferred_servers(sizes: &[(usize, usize)]) -> Vec<bool> {
    let mut defer = vec![false; sizes.len()];
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sizes[i].1));
    let (mut n, mut c): (usize, usize) = sizes.iter().fold((0, 0), |(n, c), s| (n + s.0, c + s.1));
    for i in order {
        if n <= DIRECT_MAX_TOOLS && c <= DIRECT_MAX_CHARS {
            break;
        }
        defer[i] = true;
        n -= sizes[i].0;
        c -= sizes[i].1;
    }
    defer
}

/// The deferred tools of this registry: `(name, description, schema)`, plus
/// `(server, tool count)` for the line that tells the model they exist.
struct Deferred {
    tools: Vec<(String, String, Value)>,
    counts: Vec<(String, usize)>,
}

fn servers_line(counts: &[(String, usize)]) -> String {
    counts
        .iter()
        .map(|(s, n)| format!("Servidor MCP {s}: {n} ferramentas"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn search_spec(counts: &[(String, usize)]) -> ToolSpec {
    ToolSpec {
        name: "mcp_search".into(),
        description: format!(
            "Busca ferramentas de servidores MCP grandes, que não aparecem na sua lista de ferramentas. {} — use mcp_search para achar a ferramenta (devolve nome, descrição e esquema de entrada) e depois mcp_call para chamá-la. Busque por palavras do que quer fazer, em inglês e português.",
            servers_line(counts)
        ),
        parameters: json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "o que a ferramenta deve fazer, em palavras-chave"},
            "server": {"type": "string", "description": "limita a um servidor MCP"},
            "limit": {"type": "integer", "description": "quantas devolver (padrão 5, máximo 20)"}
        }, "required": ["query"]}),
        returns: "as ferramentas que mais casam, com descrição e esquema",
        permission: Permission::Read,
        timeout: Duration::from_secs(5),
        cancellable: false,
        local: true,
        source: "mcp".into(),
    }
}

fn call_spec(counts: &[(String, usize)]) -> ToolSpec {
    ToolSpec {
        name: zerith_tools::DISPATCH_TOOL.into(),
        description: format!(
            "Chama uma ferramenta de servidor MCP achada com mcp_search ({}). Passe o nome completo `mcp__<servidor>__<ferramenta>` em `tool` e os argumentos do esquema dela em `arguments`. Pede a mesma aprovação que a ferramenta real.",
            servers_line(counts)
        ),
        parameters: json!({"type": "object", "properties": {
            "tool": {"type": "string", "description": "nome completo, ex.: mcp__servidor__ferramenta"},
            "arguments": {"type": "object", "description": "argumentos conforme o esquema devolvido por mcp_search"}
        }, "required": ["tool"]}),
        returns: "texto devolvido pelo servidor MCP",
        permission: Permission::Network,
        timeout: DEFAULT_TIMEOUT,
        cancellable: false,
        local: false,
        source: "mcp".into(),
    }
}

/// Lowercase, accent-free words, split on anything not a letter or digit
/// (so `list_procedures` is two words).
fn words(text: &str) -> Vec<String> {
    let fold = |c: char| match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        c => c,
    };
    text.chars()
        .flat_map(char::to_lowercase)
        .map(fold)
        .collect::<String>()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// BM25 (k1 1.2, b 0.75) over name ×3 + description, best first; tools that
/// match nothing are left out. Names are `mcp__server__tool`: the `mcp` and
/// server words match every tool, so idf zeroes them out by itself.
///
/// ponytail: a copy of the core of `zerith_skills::search` (that crate is not
/// a dependency here and its index is built from a skill library). No stems or
/// glossary; add them if the model's queries miss.
fn rank<'a>(
    tools: &'a [(String, String, Value)],
    query: &str,
    server: Option<&str>,
    limit: usize,
) -> Vec<&'a (String, String, Value)> {
    let prefix = server.map(|s| format!("mcp__{s}__"));
    let pool: Vec<&(String, String, Value)> = tools
        .iter()
        .filter(|t| prefix.as_deref().is_none_or(|p| t.0.starts_with(p)))
        .collect();
    let docs: Vec<Vec<String>> = pool
        .iter()
        .map(|t| {
            let name = words(&t.0);
            let mut d = Vec::new();
            for _ in 0..3 {
                d.extend(name.iter().cloned());
            }
            d.extend(words(&t.1));
            d
        })
        .collect();
    let n = docs.len().max(1) as f64;
    let avg = docs.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let q = words(query);
    let mut scored: Vec<(f64, usize)> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let mut s = 0.0;
            for w in &q {
                let tf = d.iter().filter(|x| *x == w).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = docs.iter().filter(|d| d.contains(w)).count() as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                s += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * d.len() as f64 / avg.max(1.0)));
            }
            (s, i)
        })
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, i)| pool[i])
        .collect()
}

/// `mcp_search`: reads the in-memory index, touches no server.
struct McpSearch {
    spec: ToolSpec,
    index: Arc<Deferred>,
}

impl Tool for McpSearch {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, _args: &Value, _ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        Ok(None)
    }
    fn concurrency_safe(&self, _args: &Value) -> bool {
        true
    }
    fn run(&self, args: &Value, _ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
        let query = args["query"].as_str().unwrap_or_default();
        let limit = args["limit"].as_u64().unwrap_or(5).clamp(1, 20) as usize;
        let hits = rank(&self.index.tools, query, args["server"].as_str(), limit);
        if hits.is_empty() {
            return Ok(format!(
                "nenhuma ferramenta MCP casa com {query:?}. {}. Tente outras palavras (em inglês).",
                servers_line(&self.index.counts)
            ));
        }
        Ok(hits
            .iter()
            .map(|(name, desc, schema)| format!("{name} — {desc}\n  esquema: {schema}"))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

/// `mcp_call`. The runtime pipeline swaps a call naming a deferred tool for
/// the real tool before policy and approval, so this `run` only sees the
/// names it must refuse (and direct `Registry::call` use, where its Network
/// action keeps the same verdict as the real tool's).
struct McpCall {
    spec: ToolSpec,
    index: Arc<Deferred>,
}

impl McpCall {
    fn target(&self, args: &Value) -> Result<String> {
        let tool = args["tool"].as_str().unwrap_or_default();
        if self.index.tools.iter().any(|t| t.0 == tool) {
            return Ok(tool.to_string());
        }
        Err(ToolError::BadArguments(format!(
            "mcp_call: {tool:?} não é uma ferramenta MCP adiada; use mcp_search para achar o nome exato ({})",
            servers_line(&self.index.counts)
        )))
    }
}

impl Tool for McpCall {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, args: &Value, _ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        Ok(Some(Action::Network(format!("mcp:{}", self.target(args)?))))
    }
    fn run(&self, args: &Value, _ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
        let tool = self.target(args)?;
        Err(ToolError::Failed(format!(
            "mcp_call: {tool} só roda pelo laço do agente, que pede a aprovação da ferramenta real"
        )))
    }
}

#[cfg(test)]
mod deferred_tests {
    use super::*;

    fn fake() -> Vec<(String, String, Value)> {
        [
            ("list_prompts", "List the prompts the server offers"),
            (
                "list_procedures",
                "List every analyzed procedure as address/value pairs",
            ),
            (
                "decompile_function",
                "Decompile one function to pseudo code",
            ),
            ("open_binary", "Open a native binary for analysis (análise)"),
            ("list_strings", "List strings found in the binary"),
        ]
        .iter()
        .map(|(n, d)| {
            (
                tool_name("rea", n),
                d.to_string(),
                json!({"type": "object"}),
            )
        })
        .collect()
    }

    #[test]
    fn small_setups_stay_direct_and_the_big_server_is_deferred() {
        // codebase-memory-mcp alone (measured): direct, as before.
        assert_eq!(deferred_servers(&[(17, 21_000)]), [false]);
        // with rea: only rea goes behind mcp_search.
        assert_eq!(
            deferred_servers(&[(17, 21_000), (170, 415_000)]),
            [false, true]
        );
        // many small ones over budget: the biggest go first until the rest fit.
        assert_eq!(
            deferred_servers(&[(20, 15_000), (20, 30_000), (5, 1_000)]),
            [false, true, false]
        );
        assert_eq!(deferred_servers(&[]), Vec::<bool>::new());
    }

    #[test]
    fn search_ranks_the_right_tool_first() {
        let t = fake();
        let top = |q: &str| rank(&t, q, None, 3)[0].0.clone();
        assert_eq!(top("list prompts"), "mcp__rea__list_prompts");
        assert_eq!(
            top("decompilar function pseudo code"),
            "mcp__rea__decompile_function"
        );
        assert_eq!(top("abrir binário para análise"), "mcp__rea__open_binary");
        assert!(rank(&t, "list", Some("outro"), 5).is_empty());
        assert!(rank(&t, "zzz", None, 5).is_empty());
    }

    #[test]
    fn call_refuses_unknown_names_and_judges_like_the_real_tool() {
        let call = McpCall {
            spec: call_spec(&[("rea".into(), 5)]),
            index: Arc::new(Deferred {
                tools: fake(),
                counts: vec![("rea".into(), 5)],
            }),
        };
        assert_eq!(call.spec.permission, Permission::Network);
        assert!(call
            .spec
            .description
            .contains("Servidor MCP rea: 5 ferramentas"));
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            &dir,
        );
        let ctx = CallCtx {
            workspace: &dir,
            policy: &policy,
            memory: None,
            scope: None,
        };
        for bad in ["mcp__rea__nope", "shell", "mcp__codebase__list_projects"] {
            assert!(call.action(&json!({"tool": bad}), &ctx).is_err(), "{bad}");
        }
        let a = call
            .action(&json!({"tool": "mcp__rea__list_prompts"}), &ctx)
            .unwrap();
        assert_eq!(
            a,
            Some(Action::Network("mcp:mcp__rea__list_prompts".into()))
        );
    }
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
