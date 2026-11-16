//! The tool registry.
//!
//! Every tool declares the same nine things the spec lists, in one struct, so a
//! UI can show them without the tool existing twice: name, description,
//! parameters, expected result, permissions, timeout, cancellation, locality
//! and provenance.
//!
//! Nothing here executes a shell. A shell tool without a jail would be a
//! promise the host cannot keep, so it is registered as *requiring approval*
//! and refuses when the policy is not the one that accepts any risk.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_security::{Action, Policy, Verdict};

/// What a caller may be asked to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Shell,
    Network,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
            Permission::Shell => "shell",
            Permission::Network => "network",
        }
    }
}

/// Everything a catalogue needs to display a tool honestly.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    /// What a successful call returns, in words. A schema for the result would
    /// be a lie for free-form tools; a sentence is not.
    pub returns: &'static str,
    pub permission: Permission,
    pub timeout: Duration,
    /// Whether a long call can be stopped. Reported, never assumed.
    pub cancellable: bool,
    pub local: bool,
    /// Where it came from: `built-in`, `mcp:<server>`, `plugin:<name>`.
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("no such tool: {0}")]
    NotFound(String),
    #[error("{0}")]
    BadArguments(String),
    #[error("permission denied: {0}")]
    Denied(String),
    #[error("timeout after {:?}", .0)]
    Timeout(Duration),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ToolError>;

/// What a tool may touch. Passed per call so two windows can hold different
/// workspaces without sharing mutable state.
pub struct CallCtx<'a> {
    pub workspace: &'a Path,
    pub policy: &'a Policy,
    pub memory: Option<&'a dyn zerith_memory::MemoryBackend>,
    pub scope: Option<&'a zerith_memory::Scope>,
}

pub struct Registry {
    tools: Vec<ToolSpec>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// The built-in set. Small on purpose: a tool that cannot be tested is a
    /// tool that cannot be trusted with a workspace.
    pub fn new() -> Self {
        let mut r = Self { tools: Vec::new() };
        r.register(ToolSpec {
            name: "read_file",
            description: "Le um arquivo de texto dentro do workspace.",
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            returns: "o conteudo do arquivo",
            permission: Permission::Read,
            timeout: Duration::from_secs(5),
            cancellable: false,
            local: true,
            source: "built-in".to_string(),
        });
        r.register(ToolSpec {
            name: "write_file",
            description: "Escreve um arquivo dentro do workspace, se a politica permitir.",
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["path", "content"]
            }),
            returns: "o numero de bytes escritos",
            permission: Permission::Write,
            timeout: Duration::from_secs(5),
            cancellable: false,
            local: true,
            source: "built-in".to_string(),
        });
        r.register(ToolSpec {
            name: "list_dir",
            description: "Lista entradas de um diretorio do workspace.",
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            returns: "um nome por linha; diretorios terminam com /",
            permission: Permission::Read,
            timeout: Duration::from_secs(5),
            cancellable: false,
            local: true,
            source: "built-in".to_string(),
        });
        r.register(ToolSpec {
            name: "memory_recall",
            description: "Consulta fatos duravevis da memoria do usuario.",
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
            returns: "linhas \"[confirmed|inferred] texto\"",
            permission: Permission::Read,
            timeout: Duration::from_secs(2),
            cancellable: false,
            local: true,
            source: "built-in".to_string(),
        });
        r.register(ToolSpec {
            name: "shell",
            description: "Executa um comando. Exige aprovacao: o host nao tem jail.",
            parameters: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
            returns: "a saida do comando",
            permission: Permission::Shell,
            timeout: Duration::from_secs(30),
            cancellable: true,
            local: true,
            source: "built-in".to_string(),
        });
        // An MCP tool is registered by the MCP client with its real name; the
        // registry does not know how it is implemented, only how it behaves.
        r.register(ToolSpec {
            name: "mcp_call",
            description: "Chama uma ferramenta de um servidor MCP conectado.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "tool": {"type": "string"},
                    "arguments": {"type": "object"}
                },
                "required": ["server", "tool"]
            }),
            returns: "o texto que o servidor devolveu",
            permission: Permission::Network,
            timeout: Duration::from_secs(60),
            cancellable: false,
            local: false,
            source: "mcp".to_string(),
        });
        r
    }

    pub fn register(&mut self, spec: ToolSpec) {
        self.tools.push(spec);
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.tools
    }

    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The prompt catalogue: one line per tool, name and description only.
    /// Parameter schemas go to the model when the provider supports them, not
    /// into the system prompt.
    pub fn catalog(&self) -> String {
        self.tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs one call. The policy is consulted *before* anything touches the
    /// filesystem, and a timeout is measured here so a slow tool cannot hang a
    /// caller that declared five seconds.
    pub fn call(&self, name: &str, args: &Value, ctx: &CallCtx<'_>) -> Result<Value> {
        let spec = self
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.into()))?;
        let started = Instant::now();
        let out = self.dispatch(name, args, ctx);
        if started.elapsed() > spec.timeout {
            return Err(ToolError::Timeout(spec.timeout));
        }
        out
    }

    fn dispatch(&self, name: &str, args: &Value, ctx: &CallCtx<'_>) -> Result<Value> {
        match name {
            "read_file" => {
                let p = path_arg(args)?;
                Ok(json!(std::fs::read_to_string(coerce(ctx, &p))?))
            }
            "write_file" => {
                let p = path_arg(args)?;
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| ToolError::BadArguments("content is not a string".into()))?;
                let full = coerce(ctx, &p);
                match ctx.policy.check(&Action::Write(full.clone())) {
                    Verdict::Allow => {
                        if let Some(parent) = full.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&full, content)?;
                        Ok(json!({"bytes": content.len()}))
                    }
                    Verdict::Ask(why) => Err(ToolError::Denied(format!("needs approval: {why}"))),
                    Verdict::Deny(why) => Err(ToolError::Denied(why)),
                }
            }
            "list_dir" => {
                let p = path_arg(args)?;
                let mut names = Vec::new();
                for entry in std::fs::read_dir(coerce(ctx, &p))? {
                    let entry = entry?;
                    let mut n = entry.file_name().to_string_lossy().to_string();
                    if entry.file_type()?.is_dir() {
                        n.push('/');
                    }
                    names.push(n);
                }
                names.sort();
                Ok(json!(names.join("\n")))
            }
            "memory_recall" => {
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| ToolError::BadArguments("query is not a string".into()))?;
                let (Some(mem), Some(scope)) = (ctx.memory, ctx.scope) else {
                    return Err(ToolError::Denied("no memory backend configured".into()));
                };
                let hits = mem
                    .recall(scope, query, 8)
                    .map_err(|e| ToolError::BadArguments(e.to_string()))?;
                let lines: Vec<String> = hits
                    .iter()
                    .map(|h| format!("[{}] {}", h.fact.confidence.as_str(), h.fact.text))
                    .collect();
                Ok(json!(lines.join("\n")))
            }
            // The shell is refused rather than half-implemented: without a
            // jail, "allow" would be a promise this host cannot keep.
            "shell" => {
                let cmd = args["command"]
                    .as_str()
                    .ok_or_else(|| ToolError::BadArguments("command is not a string".into()))?;
                match ctx.policy.check(&Action::Shell(cmd.to_string())) {
                    Verdict::Allow => Err(ToolError::Denied(
                        "the host has no sandbox for shell execution".into(),
                    )),
                    Verdict::Ask(why) => Err(ToolError::Denied(format!("needs approval: {why}"))),
                    Verdict::Deny(why) => Err(ToolError::Denied(why)),
                }
            }
            "mcp_call" => Err(ToolError::Denied(
                "connect an MCP server first: the registry has no transport".into(),
            )),
            other => Err(ToolError::NotFound(other.into())),
        }
    }
}

fn path_arg(args: &Value) -> Result<String> {
    args["path"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::BadArguments("path is not a string".into()))
}

/// Relative paths resolve against the workspace; absolute ones are taken as
/// given and still go through the policy (the policy is what bounds them).
fn coerce(ctx: &CallCtx<'_>, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        ctx.workspace.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ctx<'a>(policy: &'a Policy) -> CallCtx<'a> {
        CallCtx {
            workspace: policy.workspace.as_path(),
            policy,
            memory: None,
            scope: None,
        }
    }

    #[test]
    fn the_catalog_lists_every_tool_with_a_description() {
        let r = Registry::new();
        let cat = r.catalog();
        assert!(cat.contains("read_file: "));
        assert_eq!(cat.lines().count(), r.specs().len());
    }

    #[test]
    fn writing_inside_the_workspace_succeeds_and_is_readable() {
        let dir = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
        let policy = Policy::from_env(dir.path()).unwrap();
        let r = Registry::new();
        let c = ctx(&policy);
        let out = r
            .call(
                "write_file",
                &json!({"path": "a/b.txt", "content": "oi"}),
                &c,
            )
            .unwrap();
        assert_eq!(out["bytes"], 2);
        let read = r
            .call("read_file", &json!({"path": "a/b.txt"}), &c)
            .unwrap();
        assert_eq!(read, json!("oi"));
    }

    #[test]
    fn a_write_outside_the_workspace_is_denied_not_silently_redirected() {
        let dir = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
        let policy = Policy::from_env(dir.path()).unwrap();
        let r = Registry::new();
        let err = r
            .call(
                "write_file",
                &json!({"path": "../../outside.txt", "content": "x"}),
                &ctx(&policy),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)));
    }

    #[test]
    fn shell_is_refused_because_there_is_no_jail() {
        let dir = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
        let policy = Policy {
            preset: zerith_security::preset("danger-full-access").unwrap(),
            workspace: dir.path().to_path_buf(),
            enforcement: zerith_security::Enforcement::Partial,
        };
        let r = Registry::new();
        let err = r
            .call("shell", &json!({"command": "echo hi"}), &ctx(&policy))
            .unwrap_err();
        assert!(err.to_string().contains("no sandbox"));
    }

    #[test]
    fn list_dir_sorts_and_marks_directories() {
        let dir = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        let policy = Policy::from_env(dir.path()).unwrap();
        let r = Registry::new();
        let out = r
            .call("list_dir", &json!({"path": "."}), &ctx(&policy))
            .unwrap();
        assert_eq!(out, json!("b.txt\nsub/"));
    }

    #[test]
    fn an_unknown_tool_names_itself_in_the_error() {
        let dir = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
        let policy = Policy::from_env(dir.path()).unwrap();
        let err = Registry::new()
            .call("nope", &json!({}), &ctx(&policy))
            .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }
}
