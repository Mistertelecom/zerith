//! MCP servers from configuration: `[[mcp.servers]]` with `name` and either
//! `command` (+ `args`) or `url`.
//!
//! Read from the raw TOML of each settings layer instead of `Settings`, which
//! has no `mcp` field: that keeps this crate independent of `zerith-core`'s
//! typed settings. The CLI writes to `<root>/mcp.toml`, never to
//! `settings.toml`, because `Settings::save_user` rewrites that file from its
//! typed view and would drop a table it does not know.
//!
//! A server declared by a *project* file can spawn any program the repository
//! author likes, so it does not load until the user approves its name in a
//! user-level file (`[mcp] approved = ["name"]`, written by
//! `zerith mcp approve`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zerith_core::Paths;

/// Where a server definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    User,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Per-request timeout; the client default (30 s) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip, default = "user")]
    pub origin: Origin,
}

fn user() -> Origin {
    Origin::User
}

impl ServerConfig {
    /// Exactly one of `command` / `url`, and a name that survives becoming
    /// part of a tool name.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "nome de servidor MCP inválido: {:?} (use letras, números, - e _)",
                self.name
            ));
        }
        match (&self.command, &self.url) {
            (Some(c), None) if !c.trim().is_empty() => Ok(()),
            (None, Some(u)) if u.starts_with("http://") || u.starts_with("https://") => Ok(()),
            (Some(_), Some(_)) => Err(format!(
                "servidor {}: use `command` ou `url`, não os dois",
                self.name
            )),
            _ => Err(format!(
                "servidor {}: falta `command` ou `url` (http/https)",
                self.name
            )),
        }
    }

    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Section {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    servers: Vec<ServerConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    approved: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    mcp: Section,
}

fn read_file(path: &Path) -> Result<Option<File>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    toml::from_str::<File>(&text)
        .map(Some)
        .map_err(|e| format!("{}: {}", path.display(), e.message()))
}

/// The user-level file the CLI edits.
pub fn user_file(paths: &Paths) -> PathBuf {
    paths.root().join("mcp.toml")
}

/// What `load_servers` found.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Servers that may run: user-level ones, plus project ones the user
    /// approved. Later definitions with the same name replace earlier ones.
    pub servers: Vec<ServerConfig>,
    /// Project-level servers waiting for `zerith mcp approve <name>`.
    pub unapproved: Vec<ServerConfig>,
    /// Unreadable or invalid files and entries; the rest still loads.
    pub errors: Vec<String>,
}

/// Reads user (`settings.toml`, `mcp.toml`) and project
/// (`.zerith/settings.toml`, `settings.local.toml`, `mcp.toml`) layers.
pub fn load_servers(paths: &Paths, project_dir: Option<&Path>) -> Loaded {
    let mut out = Loaded::default();
    let mut approved: Vec<String> = Vec::new();
    let mut layers: Vec<(PathBuf, Origin)> = vec![
        (paths.settings(), Origin::User),
        (user_file(paths), Origin::User),
    ];
    if let Some(dir) = project_dir {
        let z = dir.join(".zerith");
        layers.push((z.join("settings.toml"), Origin::Project));
        layers.push((z.join("settings.local.toml"), Origin::Project));
        layers.push((z.join("mcp.toml"), Origin::Project));
    }
    let mut all: Vec<ServerConfig> = Vec::new();
    for (path, origin) in layers {
        match read_file(&path) {
            Ok(Some(f)) => {
                // Approvals only count from the user's own files.
                if origin == Origin::User {
                    approved.extend(f.mcp.approved);
                }
                for mut s in f.mcp.servers {
                    s.origin = origin;
                    match s.validate() {
                        Ok(()) => all.push(s),
                        Err(e) => out.errors.push(e),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => out.errors.push(e),
        }
    }
    // Last definition of a name wins, as with settings layers.
    let mut by_name: Vec<ServerConfig> = Vec::new();
    for s in all {
        by_name.retain(|x| x.name != s.name);
        by_name.push(s);
    }
    for s in by_name {
        if s.origin == Origin::Project && !approved.contains(&s.name) {
            out.unapproved.push(s);
        } else {
            out.servers.push(s);
        }
    }
    out
}

fn edit_user(paths: &Paths, edit: impl FnOnce(&mut Section)) -> Result<(), String> {
    let path = user_file(paths);
    let mut file = read_file(&path)?.unwrap_or_default();
    edit(&mut file.mcp);
    let text = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Adds or replaces a server in `<root>/mcp.toml`.
pub fn add_server(paths: &Paths, server: ServerConfig) -> Result<(), String> {
    server.validate()?;
    edit_user(paths, |s| {
        s.servers.retain(|x| x.name != server.name);
        s.servers.push(server);
    })
}

/// Removes a server (and its approval) from `<root>/mcp.toml`. `Ok(false)`:
/// it was not there — a definition in `settings.toml` has to be edited by hand.
pub fn remove_server(paths: &Paths, name: &str) -> Result<bool, String> {
    let mut found = false;
    edit_user(paths, |s| {
        let before = s.servers.len();
        s.servers.retain(|x| x.name != name);
        found = s.servers.len() != before;
        s.approved.retain(|x| x != name);
    })?;
    Ok(found)
}

/// Lets a project-defined server of this name run.
pub fn approve_server(paths: &Paths, name: &str) -> Result<(), String> {
    edit_user(paths, |s| {
        if !s.approved.iter().any(|x| x == name) {
            s.approved.push(name.to_string());
        }
    })
}

/// `zerith mcp add` arguments: `<name> -- <command> [args..]` or
/// `<name> --url <url>`.
pub fn parse_add(args: &[String]) -> Result<ServerConfig, String> {
    let name = args.first().ok_or("mcp add precisa de um nome")?.clone();
    let mut server = ServerConfig {
        name,
        command: None,
        args: Vec::new(),
        url: None,
        timeout_ms: None,
        origin: Origin::User,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                server.url = Some(args.get(i + 1).ok_or("--url precisa de um valor")?.clone());
                i += 2;
            }
            "--timeout-ms" => {
                server.timeout_ms = Some(
                    args.get(i + 1)
                        .ok_or("--timeout-ms precisa de um valor")?
                        .parse()
                        .map_err(|_| "--timeout-ms precisa de um número")?,
                );
                i += 2;
            }
            "--" => {
                let mut rest = args[i + 1..].iter();
                server.command = Some(rest.next().ok_or("falta o comando após --")?.clone());
                server.args = rest.cloned().collect();
                break;
            }
            other => return Err(format!("argumento desconhecido: {other}")),
        }
    }
    server.validate()?;
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_parses_both_transports_and_rejects_nonsense() {
        let s = parse_add(&v(&["fs", "--", "npx", "-y", "server-fs", "/tmp"])).unwrap();
        assert_eq!(s.command.as_deref(), Some("npx"));
        assert_eq!(s.args, ["-y", "server-fs", "/tmp"]);
        let h = parse_add(&v(&[
            "web",
            "--url",
            "https://x.test/mcp",
            "--timeout-ms",
            "5000",
        ]))
        .unwrap();
        assert!(h.is_http());
        assert_eq!(h.timeout_ms, Some(5000));
        assert!(parse_add(&v(&[])).is_err());
        assert!(parse_add(&v(&["a b", "--", "x"])).is_err());
        assert!(parse_add(&v(&["x"])).is_err());
        assert!(parse_add(&v(&["x", "--url", "ftp://y"])).is_err());
        assert!(parse_add(&v(&["x", "--"])).is_err());
        assert!(parse_add(&v(&["x", "--bogus"])).is_err());
    }
}
