//! The rest of a Hermes setup: its skills folder and the `mcp_servers:` block
//! of `config.yaml`. Read-only on the Hermes side, like the history importer:
//! files are copied *out*, nothing under the Hermes home is ever written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use zerith_core::Paths;
use zerith_mcp::config::{add_server, load_servers, set_enabled, set_extra};
use zerith_mcp::{Extra, Origin, ServerConfig};
use zerith_skills::Library;

/// The Hermes home: `HERMES_HOME`, `~/.hermes`, then `%LOCALAPPDATA%\hermes`
/// (the Windows installer). The first that has `config.yaml` or `skills/`.
pub fn hermes_home() -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("HERMES_HOME") {
        v.push(d.into());
    }
    if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        v.push(PathBuf::from(h).join(".hermes"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(l).join("hermes"));
    }
    v.into_iter()
        .find(|d| d.join("config.yaml").is_file() || d.join("skills").is_dir())
}

// ------------------------------------------------------------------ skills

/// One skill folder in the Hermes home.
#[derive(Debug, Clone, PartialEq)]
pub struct HermesSkill {
    pub name: String,
    /// Folder path under `skills/` above the skill (`mlops/inference`), `""` at the top.
    pub category: String,
    pub description: String,
    pub dir: PathBuf,
    pub installed: bool,
    /// Why this folder cannot be imported (bad frontmatter), if it cannot.
    pub error: Option<String>,
}

/// Every `SKILL.md` under `<home>/skills`, at any depth. Hidden folders
/// (`.archive`, `.hub`, `.curator_backups`) are Hermes' own bookkeeping and are
/// skipped; a skill folder is not searched further (its `references/` are its own).
pub fn hermes_skills(home: &Path, lib: &Library) -> Vec<HermesSkill> {
    let installed: Vec<String> = lib
        .manifest()
        .map(|m| m.skills.into_iter().map(|s| s.name).collect())
        .unwrap_or_default();
    let root = home.join("skills");
    let mut out = Vec::new();
    walk(&root, &root, 0, &mut out);
    for s in &mut out {
        s.installed = installed.contains(&s.name);
    }
    out.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<HermesSkill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if hidden || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if !p.join("SKILL.md").is_file() {
            if depth < 4 {
                walk(root, &p, depth + 1, out);
            }
            continue;
        }
        let category = dir
            .strip_prefix(root)
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .replace('\\', "/");
        let folder = e.file_name().to_string_lossy().into_owned();
        let (name, description, error) = match zerith_skills::scan(&p) {
            Ok(s) => (s.frontmatter.name, s.frontmatter.description, None),
            Err(err) => (folder, String::new(), Some(err.to_string())),
        };
        out.push(HermesSkill {
            name,
            category,
            description,
            dir: p,
            installed: false,
            error,
        });
    }
}

/// What an import did, by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetupReport {
    pub imported: Vec<String>,
    pub existed: Vec<String>,
    /// `"<name>: <motivo>"`; never carries an env value or a header.
    pub failed: Vec<String>,
}

/// Copies the named skills through `Library::install` (scan, name checks,
/// atomic manifest), so they land exactly like any other install: enabled.
pub fn import_hermes_skills(home: &Path, lib: &Library, names: &[String]) -> SetupReport {
    let mut r = SetupReport::default();
    for s in hermes_skills(home, lib)
        .into_iter()
        .filter(|s| names.contains(&s.name))
    {
        if s.installed || r.imported.contains(&s.name) {
            r.existed.push(s.name);
            continue;
        }
        let source = format!("hermes:{}", s.dir.display());
        match lib.install(&s.dir, &source) {
            Ok(_) => r.imported.push(s.name),
            Err(zerith_skills::install::InstallError::NameConflict(_)) => r.existed.push(s.name),
            Err(e) => r.failed.push(format!("{}: {e}", s.name)),
        }
    }
    r
}

// --------------------------------------------------------------------- mcp

/// One entry of Hermes' `mcp_servers:`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HermesMcp {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    /// `auth: oauth` or an `oauth:` block: imported as `auth = "oauth"`; the
    /// person logs in afterwards ("Entrar" / `zerith mcp login`). Hermes'
    /// tokens are never copied: its refresh tokens rotate, using one here
    /// would log Hermes out.
    pub oauth: bool,
    pub enabled: bool,
    /// Hermes `timeout` is in seconds.
    pub timeout_secs: Option<u64>,
}

/// What importing one server would do here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStatus {
    Importable,
    NotFound,
    /// Importable; needs a login afterwards.
    OAuth,
    Exists,
    Invalid,
}

impl McpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            McpStatus::Importable => "importavel",
            McpStatus::NotFound => "nao-encontrado",
            McpStatus::OAuth => "oauth",
            McpStatus::Exists => "existe",
            McpStatus::Invalid => "invalido",
        }
    }
}

impl HermesMcp {
    fn config(&self) -> ServerConfig {
        ServerConfig {
            name: self.name.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            url: self.url.clone(),
            timeout_ms: self.timeout_secs.map(|s| s * 1000),
            auth: self.oauth.then(|| "oauth".to_string()),
            origin: Origin::User,
        }
    }
}

/// The servers of `<home>/config.yaml`, each with what importing it would do.
pub fn hermes_mcp(home: &Path, paths: &Paths) -> Result<Vec<(HermesMcp, McpStatus)>, String> {
    let text = std::fs::read_to_string(home.join("config.yaml"))
        .map_err(|e| format!("{}: {e}", home.join("config.yaml").display()))?;
    let loaded = load_servers(paths, None);
    let have: Vec<&str> = loaded
        .servers
        .iter()
        .chain(&loaded.disabled)
        .map(|s| s.name.as_str())
        .collect();
    Ok(parse_mcp_servers(&text)?
        .into_iter()
        .map(|m| {
            let status = if have.contains(&m.name.as_str()) {
                McpStatus::Exists
            } else if m.config().validate().is_err() {
                McpStatus::Invalid
            } else if m.oauth {
                McpStatus::OAuth
            } else if m
                .command
                .as_deref()
                .is_some_and(|c| zerith_mcp::which(c).is_none())
            {
                McpStatus::NotFound
            } else {
                McpStatus::Importable
            };
            (m, status)
        })
        .collect())
}

/// Writes the named, importable servers to `<root>/mcp.toml`, env and headers
/// included, and keeps Hermes' `enabled: false` as switched off.
pub fn import_hermes_mcp(
    home: &Path,
    paths: &Paths,
    names: &[String],
) -> Result<SetupReport, String> {
    let mut r = SetupReport::default();
    for (m, status) in hermes_mcp(home, paths)?
        .into_iter()
        .filter(|(m, _)| names.contains(&m.name))
    {
        let why = match status {
            McpStatus::Importable | McpStatus::OAuth => None,
            McpStatus::Exists => {
                r.existed.push(m.name);
                continue;
            }
            McpStatus::NotFound => Some("comando não encontrado neste computador"),
            McpStatus::Invalid => Some("precisa de `command` ou `url` (http/https)"),
        };
        if let Some(why) = why {
            r.failed.push(format!("{}: {why}", m.name));
            continue;
        }
        let extra = Extra {
            env: m.env.clone(),
            headers: m.headers.clone(),
        };
        let done = add_server(paths, m.config())
            .and_then(|_| set_extra(paths, &m.name, &extra))
            .and_then(|_| set_enabled(paths, &m.name, m.enabled));
        match done {
            Ok(()) => r.imported.push(m.name),
            Err(e) => r.failed.push(format!("{}: {e}", m.name)),
        }
    }
    Ok(r)
}

/// A YAML node, as far as `mcp_servers:` needs.
#[derive(Debug, Clone, PartialEq)]
enum Y {
    Str(String),
    List(Vec<Y>),
    Map(Vec<(String, Y)>),
}

impl Y {
    fn get(&self, key: &str) -> Option<&Y> {
        match self {
            Y::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self) -> Option<String> {
        match self {
            Y::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
    fn strings(&self) -> Vec<String> {
        match self {
            Y::List(l) => l.iter().filter_map(Y::str).collect(),
            Y::Str(s) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }
    fn pairs(&self) -> BTreeMap<String, String> {
        match self {
            Y::Map(m) => m
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.str()?)))
                .collect(),
            _ => BTreeMap::new(),
        }
    }
}

/// Parses the top-level `mcp_servers:` block of a Hermes `config.yaml`.
///
// ponytail: hand-written indentation parser for exactly this block: block
// maps, block lists (`- x`, `- k: v`), flow lists `[a, b]`, `{}`, plain and
// quoted scalars, comments. No anchors/aliases, block scalars (`|`, `>`),
// nested flow collections or multi-documents; those come back as the raw
// string. If Hermes ever writes them here, add a YAML crate then.
pub fn parse_mcp_servers(yaml: &str) -> Result<Vec<HermesMcp>, String> {
    let mut lines: Vec<(usize, usize, String)> = Vec::new();
    let mut inside = false;
    for (no, raw) in yaml.lines().enumerate() {
        let text = strip_comment(raw.trim_end());
        if text.trim().is_empty() {
            continue;
        }
        let indent = text.len() - text.trim_start().len();
        if indent == 0 {
            if inside {
                break;
            }
            inside = text.trim_end() == "mcp_servers:";
            continue;
        }
        if inside {
            lines.push((no + 1, indent, text.trim().to_string()));
        }
    }
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let mut i = 0;
    let first = lines[0].1;
    let root = block(&lines, &mut i, first)?;
    if i < lines.len() {
        return Err(format!(
            "config.yaml linha {}: indentação inesperada",
            lines[i].0
        ));
    }
    let Y::Map(entries) = root else {
        return Err("mcp_servers não é um mapa".into());
    };
    Ok(entries
        .into_iter()
        .map(|(name, v)| HermesMcp {
            command: v.get("command").and_then(Y::str).filter(|s| !s.is_empty()),
            args: v.get("args").map(Y::strings).unwrap_or_default(),
            url: v.get("url").and_then(Y::str).filter(|s| !s.is_empty()),
            env: v.get("env").map(Y::pairs).unwrap_or_default(),
            headers: v.get("headers").map(Y::pairs).unwrap_or_default(),
            oauth: v.get("auth").and_then(Y::str).as_deref() == Some("oauth")
                || v.get("oauth").is_some(),
            enabled: v.get("enabled").and_then(Y::str).as_deref() != Some("false"),
            timeout_secs: v
                .get("timeout")
                .and_then(Y::str)
                .and_then(|s| s.parse().ok()),
            name,
        })
        .collect())
}

/// One node whose lines sit at exactly `indent`.
fn block(lines: &[(usize, usize, String)], i: &mut usize, indent: usize) -> Result<Y, String> {
    if lines[*i].2 == "-" || lines[*i].2.starts_with("- ") {
        let mut items = Vec::new();
        while *i < lines.len() && lines[*i].1 == indent && lines[*i].2.starts_with('-') {
            let rest = lines[*i].2[1..].trim_start().to_string();
            let line = lines[*i].0;
            if rest.is_empty() {
                *i += 1;
                match lines.get(*i) {
                    Some(l) if l.1 > indent => items.push(block(lines, i, l.1)?),
                    _ => items.push(Y::Str(String::new())),
                }
            } else if key_value(&rest).is_some() {
                // `- k: v` opens a map whose further keys align with `k`.
                let mut sub: Vec<(usize, usize, String)> = vec![(line, indent + 2, rest)];
                *i += 1;
                while *i < lines.len() && lines[*i].1 > indent {
                    sub.push(lines[*i].clone());
                    *i += 1;
                }
                let mut j = 0;
                items.push(block(&sub, &mut j, indent + 2)?);
            } else {
                items.push(scalar(&rest));
                *i += 1;
            }
        }
        return Ok(Y::List(items));
    }
    let mut map = Vec::new();
    while *i < lines.len() && lines[*i].1 == indent {
        let (no, _, text) = &lines[*i];
        let (k, v) = key_value(text)
            .ok_or_else(|| format!("config.yaml linha {no}: esperava `chave: valor`"))?;
        *i += 1;
        let value = if !v.is_empty() {
            scalar(v)
        } else {
            match lines.get(*i) {
                Some(l) if l.1 > indent => block(lines, i, l.1)?,
                // `key:` followed by `- item` at the same indent is a list.
                Some(l) if l.1 == indent && l.2.starts_with("- ") => block(lines, i, indent)?,
                _ => Y::Str(String::new()),
            }
        };
        map.push((unquote(k), value));
    }
    if *i < lines.len() && lines[*i].1 > indent {
        return Err(format!(
            "config.yaml linha {}: indentação inesperada",
            lines[*i].0
        ));
    }
    Ok(Y::Map(map))
}

/// `key: value` / `key:`; the key may be quoted. `C:\x` and URLs are not keys
/// because a key's colon is followed by a space or the end of the line.
fn key_value(text: &str) -> Option<(&str, &str)> {
    let end = if text.starts_with(['"', '\'']) {
        let q = text.chars().next()?;
        text[1..].find(q)? + 2
    } else {
        0
    };
    let pos = text[end..]
        .match_indices(':')
        .map(|(p, _)| p + end)
        .find(|&p| text[p + 1..].is_empty() || text[p + 1..].starts_with(' '))?;
    let key = text[..pos].trim();
    (!key.is_empty() && !key.starts_with('-')).then(|| (key, text[pos + 1..].trim()))
}

fn scalar(v: &str) -> Y {
    let v = v.trim();
    if v == "{}" {
        return Y::Map(Vec::new());
    }
    if let Some(inner) = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return Y::List(
            inner
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Y::Str(unquote(s)))
                .collect(),
        );
    }
    Y::Str(unquote(v))
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let mut out = String::new();
        let mut chars = s[1..s.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

/// Drops a ` # comment` (or a whole-line `#`) that is not inside quotes.
fn strip_comment(line: &str) -> &str {
    let (mut single, mut double) = (false, false);
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && prev.is_whitespace() => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape of the real `config.yaml` on the dev machine, secrets replaced.
    const FIXTURE: &str = r#"toolsets:
  - file
  - web
mcp_servers:
  centauri:
    command: /Users/apple/.local/share/pycentauri/centauri-mcp.sh
    enabled: false
  vercel:
    url: https://mcp.vercel.com
    auth: oauth
    enabled: true
    oauth:
      timeout: 1800
    tools:
      include:
        - search_vercel_documentation
        - get_auth_user
  canva:
    url: https://mcp.canva.com/mcp
    auth: oauth
  web:
    url: "https://example.test/mcp"   # comment after a value
    headers:
      Authorization: "Bearer REDACTED"
  codebase-memory-mcp:
    command: npx
    args:
      - --yes
      - codebase-memory-mcp
    enabled: true
  rea:
    command: C:\Program Files\nodejs\node.exe
    args: ['C:\Users\x\rea.mjs', "mcp"]
    enabled: true

# ── Fallback Model ── (a column-0 comment does not end the block)
# fallback_model:
#   provider: openrouter
    env:
      JAVA_HOME: C:\tools\jdk
      API_KEY: 'REDACTED # not a comment'
    timeout: 1500
_left_core_scoped:
  - homeassistant
"#;

    #[test]
    fn nested_hermes_skills_import_once_with_their_files() {
        let t = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("hermes-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let skill = |rel: &str, name: &str| {
            let d = t.join("home/skills").join(rel);
            std::fs::create_dir_all(d.join("references")).unwrap();
            std::fs::write(
                d.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\nmetadata:\n  hermes:\n    tags: [a, b]\n---\nbody\n"),
            )
            .unwrap();
            std::fs::write(d.join("references/x.md"), "ref").unwrap();
        };
        skill("top", "top");
        skill("mlops/inference/deep", "deep");
        skill(".archive/old", "old");
        let lib = Library::open(t.join("lib")).unwrap();
        let found = hermes_skills(&t.join("home"), &lib);
        let seen: Vec<(&str, &str)> = found
            .iter()
            .map(|s| (s.category.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(seen, [("", "top"), ("mlops/inference", "deep")]);
        let names = vec!["top".to_string(), "deep".to_string(), "nope".to_string()];
        let r = import_hermes_skills(&t.join("home"), &lib, &names);
        assert_eq!(r.imported, ["top", "deep"]);
        assert!(lib.root().join("deep/references/x.md").is_file());
        assert_eq!(lib.enabled().unwrap().len(), 2);
        let again = import_hermes_skills(&t.join("home"), &lib, &names);
        assert!(again.imported.is_empty());
        assert_eq!(again.existed, ["top", "deep"]);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn the_hermes_mcp_block_parses() {
        let s = parse_mcp_servers(FIXTURE).unwrap();
        let names: Vec<&str> = s.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "centauri",
                "vercel",
                "canva",
                "web",
                "codebase-memory-mcp",
                "rea"
            ]
        );
        assert!(!s[0].enabled && s[1].enabled && s[2].enabled);
        assert!(s[1].oauth && s[2].oauth && !s[3].oauth);
        assert_eq!(s[3].url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(s[3].headers["Authorization"], "Bearer REDACTED");
        assert_eq!(s[4].args, ["--yes", "codebase-memory-mcp"]);
        let rea = &s[5];
        assert_eq!(
            rea.command.as_deref(),
            Some(r"C:\Program Files\nodejs\node.exe")
        );
        assert_eq!(rea.args, [r"C:\Users\x\rea.mjs", "mcp"]);
        assert_eq!(rea.env["JAVA_HOME"], r"C:\tools\jdk");
        assert_eq!(rea.env["API_KEY"], "REDACTED # not a comment");
        assert_eq!(rea.timeout_secs, Some(1500));
        assert!(parse_mcp_servers("model: x\n").unwrap().is_empty());
        assert!(parse_mcp_servers("mcp_servers:\n  a:\n      b: 1\n    c: 2\n").is_err());
    }

    #[test]
    fn hermes_oauth_servers_import_as_oauth_and_wait_for_a_login() {
        let t = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("hermes-oauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("home")).unwrap();
        std::fs::write(t.join("home/config.yaml"), FIXTURE).unwrap();
        let paths = Paths::with_root(t.join("root"));
        let found = hermes_mcp(&t.join("home"), &paths).unwrap();
        let st = |n: &str| found.iter().find(|(m, _)| m.name == n).unwrap().1;
        assert_eq!(st("vercel"), McpStatus::OAuth);
        assert_eq!(st("canva"), McpStatus::OAuth);
        let r =
            import_hermes_mcp(&t.join("home"), &paths, &["vercel".into(), "canva".into()]).unwrap();
        assert_eq!(r.imported, ["vercel", "canva"], "{:?}", r.failed);
        let l = load_servers(&paths, None);
        let v = l.servers.iter().find(|s| s.name == "vercel").unwrap();
        assert!(v.is_oauth());
        assert_eq!(v.url.as_deref(), Some("https://mcp.vercel.com"));
        let toml = std::fs::read_to_string(t.join("root/mcp.toml")).unwrap();
        assert!(toml.contains("auth = \"oauth\""));
        assert!(!toml.to_ascii_lowercase().contains("token"));
        let again = hermes_mcp(&t.join("home"), &paths).unwrap();
        assert!(again
            .iter()
            .any(|(m, s)| m.name == "vercel" && *s == McpStatus::Exists));
        let _ = std::fs::remove_dir_all(&t);
    }
}
