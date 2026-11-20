//! The Claude Code plugin layout, read into a Zerith [`Manifest`] at install.
//!
//! A Claude Code plugin is a folder with `.claude-plugin/plugin.json` and its
//! parts found by convention: `skills/<name>/SKILL.md`, `commands/*.md`,
//! `agents/*.md`, and MCP servers in `mcpServers` or `.mcp.json`. The
//! manifest may point elsewhere (`"skills": "./x"` or a list).
//!
//! A slash command (`commands/hunt.md`, typed `/hunt alvo`) becomes a skill
//! in the installed copy, so the model finds and loads it like any other
//! skill when the request matches; nothing in the runtime knows commands.
//! Hooks are not converted: their tool names and stdin contract are Claude
//! Code's own.

use std::path::Path;

use serde_json::Value;

use crate::{Manifest, McpEntry, Result};

/// Where generated command skills go inside the installed plugin.
const COMMANDS_DIR: &str = ".zerith-commands";

/// `./skills/` → `skills`: what `validate_bundle` accepts.
fn clean(p: &str) -> String {
    p.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

/// The paths a manifest field names (a string or a list); `default` when the
/// field is absent.
fn paths(v: &Value, default: &str) -> Vec<String> {
    let list: Vec<&str> = match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![default],
    };
    list.into_iter()
        .map(clean)
        .filter(|s| !s.is_empty())
        .collect()
}

fn sorted_entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn rel(root: &Path, p: &Path) -> Option<String> {
    Some(
        p.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

/// A folder with `SKILL.md` is a skill; otherwise each subfolder that has one.
fn skill_dirs(root: &Path, dirs: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for d in dirs {
        let dir = root.join(&d);
        if dir.join("SKILL.md").is_file() {
            out.push(d);
            continue;
        }
        out.extend(
            sorted_entries(&dir)
                .into_iter()
                .filter(|p| p.join("SKILL.md").is_file())
                .filter_map(|p| rel(root, &p)),
        );
    }
    out
}

fn md_files(root: &Path, dirs: Vec<String>) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for d in dirs {
        let p = root.join(&d);
        if p.is_file() {
            out.push(p);
        } else {
            out.extend(
                sorted_entries(&p)
                    .into_iter()
                    .filter(|f| f.is_file() && f.extension().is_some_and(|e| e == "md")),
            );
        }
    }
    out
}

/// A skill name the spec accepts: lowercase, digits and single hyphens.
fn skill_name(plugin: &str, command: &str) -> Option<String> {
    let raw = if command == plugin {
        command.to_string()
    } else {
        format!("{plugin}-{command}")
    };
    let mut name = String::new();
    for c in raw.to_lowercase().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if !(c == '-' && (name.is_empty() || name.ends_with('-'))) {
            name.push(c);
        }
    }
    let name: String = name.chars().take(64).collect();
    let name = name.trim_end_matches('-').to_string();
    (!name.is_empty()).then_some(name)
}

/// `(description, body)` of a command file: the frontmatter `description`,
/// else its first line of prose.
fn split_command(text: &str) -> (String, String) {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (header, body) = match text.strip_prefix("---\n").and_then(|r| {
        r.find("\n---")
            .map(|end| (&r[..end], r[end + 4..].trim_start_matches('\n')))
    }) {
        Some((h, b)) => (h.to_string(), b.to_string()),
        None => (String::new(), text.clone()),
    };
    let from_header = header.lines().find_map(|l| {
        let v = l.strip_prefix("description:")?.trim();
        Some(v.trim_matches(|c| c == '"' || c == '\'').to_string())
    });
    let desc = from_header.filter(|d| !d.is_empty()).unwrap_or_else(|| {
        body.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or_default()
            .to_string()
    });
    (desc, body)
}

/// Writes one skill per command into the installed copy; returns their
/// folders (relative to `root`).
fn commands(plugin: &str, root: &Path, dirs: Vec<String>) -> Result<Vec<String>> {
    let root_str = root.to_string_lossy().replace('\\', "/");
    let mut out = Vec::new();
    for file in md_files(root, dirs) {
        let Some(stem) = file.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let (Ok(text), Some(name)) = (std::fs::read_to_string(&file), skill_name(plugin, &stem))
        else {
            continue;
        };
        let (desc, body) = split_command(&text);
        let desc: String = format!("Comando /{stem} do plugin {plugin}. {desc}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('"', "'")
            .chars()
            .take(1000)
            .collect();
        let rel = format!("{COMMANDS_DIR}/{name}");
        std::fs::create_dir_all(root.join(&rel))?;
        std::fs::write(
            root.join(&rel).join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: \"{desc}\"\n---\n\n{}\n\n\
                 (Comando /{stem}: os argumentos são o texto que o usuário escreveu depois de \
                 /{stem}; onde o texto acima diz $ARGUMENTS, use esse texto. Caminhos relativos \
                 como ./HARNESS.md são da pasta do plugin: {root_str})\n",
                body.replace("${CLAUDE_PLUGIN_ROOT}", &root_str).trim_end()
            ),
        )?;
        out.push(rel);
    }
    Ok(out)
}

fn read_json(p: &Path) -> Value {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// `mcpServers` (inline or a file) or `.mcp.json`, with `${CLAUDE_PLUGIN_ROOT}`
/// resolved. `env` is dropped: its values are per-user settings Claude Code
/// asks for at install.
// ponytail: no env passthrough; add `env` to McpEntry when a plugin's server
// needs a variable to start at all.
fn mcp(v: &Value, root: &Path) -> Vec<McpEntry> {
    let servers = match &v["mcpServers"] {
        Value::Object(_) => v["mcpServers"].clone(),
        Value::String(p) => read_json(&root.join(clean(p))),
        _ => read_json(&root.join(".mcp.json")),
    };
    let servers = match servers.get("mcpServers") {
        Some(inner) => inner.clone(),
        None => servers,
    };
    let root_str = root.to_string_lossy().replace('\\', "/");
    let sub = |s: &str| s.replace("${CLAUDE_PLUGIN_ROOT}", &root_str);
    let Value::Object(map) = servers else {
        return Vec::new();
    };
    map.iter()
        .map(|(name, cfg)| McpEntry {
            name: name.clone(),
            command: cfg["command"].as_str().map(sub),
            args: cfg["args"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).map(sub).collect())
                .unwrap_or_default(),
            url: cfg["url"].as_str().map(sub),
        })
        .collect()
}

/// The manifest of a Claude Code plugin already copied to `root` (absolute).
/// `v` is its `plugin.json`, with a marketplace entry's fields on top.
pub(crate) fn manifest(v: &Value, root: &Path) -> Result<Manifest> {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let name = s("name");
    let mut skills = skill_dirs(root, paths(&v["skills"], "skills"));
    skills.extend(commands(&name, root, paths(&v["commands"], "commands"))?);
    let agents = md_files(root, paths(&v["agents"], "agents"))
        .iter()
        .filter_map(|p| rel(root, p))
        .collect();
    let version = s("version");
    Ok(Manifest {
        version: if version.is_empty() {
            "-".into()
        } else {
            version
        },
        description: s("description"),
        license: v["license"].as_str().map(Into::into),
        command: String::new(),
        args: Vec::new(),
        capabilities: Vec::new(),
        skills,
        agents,
        hooks: Vec::new(),
        mcp: mcp(v, root),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_and_descriptions_come_out_valid() {
        assert_eq!(
            skill_name("cli-anything", "cli-anything").unwrap(),
            "cli-anything"
        );
        assert_eq!(
            skill_name("claude-bughunter", "Hunt_All").unwrap(),
            "claude-bughunter-hunt-all"
        );
        let (d, b) =
            split_command("---\r\ndescription: \"Caça bugs\"\r\n---\r\n\r\n# hunt\r\ncorpo");
        assert_eq!((d.as_str(), b.as_str()), ("Caça bugs", "# hunt\ncorpo"));
        let (d, _) = split_command("# list\n\nLista as ferramentas.\n");
        assert_eq!(d, "Lista as ferramentas.");
        assert_eq!(
            paths(&Value::String("./skills/".into()), "x"),
            vec!["skills"]
        );
        assert_eq!(paths(&Value::Null, "commands"), vec!["commands"]);
    }
}
