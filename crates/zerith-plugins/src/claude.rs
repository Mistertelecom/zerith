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

use crate::{HookEntry, Manifest, McpEntry, Result};

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

/// Translates a Claude Code hook tool matcher (e.g. `"Edit|Write|MultiEdit"`
/// or `"Bash(git *)"`) into Zerith tool names (`"edit_file|multi_edit|write_file"`
/// or `"shell(git *)"`).
pub fn translate_hook_matcher(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return None;
    }
    fn map_tool(name: &str) -> Vec<&str> {
        match name.trim() {
            "Bash" | "Shell" => vec!["shell"],
            "Read" => vec!["read_file"],
            "Write" => vec!["write_file"],
            "Edit" => vec!["edit_file", "multi_edit"],
            "MultiEdit" => vec!["multi_edit"],
            "Grep" => vec!["grep"],
            "Glob" => vec!["glob"],
            "LS" => vec!["list_dir"],
            "WebFetch" => vec!["web_fetch"],
            "WebSearch" => vec!["web_search"],
            "Task" | "Agent" => vec!["agent"],
            "TodoWrite" => vec!["task_plan", "task_create", "task_update"],
            other => vec![other],
        }
    }
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, ch) in trimmed.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth <= 0 => {
                parts.push(&trimmed[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&trimmed[start..]);

    let mut out = Vec::new();
    for entry in parts {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.split_once('(') {
            Some((name, rest)) => {
                for mapped in map_tool(name) {
                    let item = format!("{mapped}({rest}");
                    if !out.contains(&item) {
                        out.push(item);
                    }
                }
            }
            None => {
                for mapped in map_tool(entry) {
                    let item = mapped.to_string();
                    if !out.contains(&item) {
                        out.push(item);
                    }
                }
            }
        }
    }
    (!out.is_empty()).then(|| out.join("|"))
}

fn valid_hook_event(ev: &str) -> bool {
    matches!(
        ev,
        "SessionStart"
            | "UserPromptSubmit"
            | "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "PermissionRequest"
            | "Stop"
            | "PreCompact"
            | "SessionEnd"
            | "SubagentStart"
            | "SubagentStop"
            | "Notification"
            | "PostCompact"
            | "TaskCompleted"
    )
}

/// Parses Claude Code `hooks` (inline in `plugin.json`, a referenced JSON file,
/// or `hooks/hooks.json`), resolving `${CLAUDE_PLUGIN_ROOT}` and mapping
/// tool matchers to Zerith's tool names.
fn hooks(v: &Value, root: &Path) -> Vec<HookEntry> {
    let raw = match &v["hooks"] {
        Value::Object(_) => v["hooks"].clone(),
        Value::String(p) => read_json(&root.join(clean(p))),
        _ => read_json(&root.join("hooks/hooks.json")),
    };
    let map_val = match raw.get("hooks") {
        Some(inner) if inner.is_object() => inner.clone(),
        _ => raw,
    };
    let Value::Object(events) = map_val else {
        return Vec::new();
    };
    let root_str = root.to_string_lossy().replace('\\', "/");
    let sub = |s: &str| s.replace("${CLAUDE_PLUGIN_ROOT}", &root_str);
    let to_ms = |n: u64| if n <= 600 { n * 1000 } else { n };

    let mut out = Vec::new();
    for (event, list) in events {
        if !valid_hook_event(&event) {
            continue;
        }
        let Some(entries) = list.as_array() else {
            continue;
        };
        for item in entries {
            let matcher = item["matcher"].as_str().and_then(translate_hook_matcher);
            if let Some(nested) = item["hooks"].as_array() {
                for h in nested {
                    let is_cmd = h["type"].as_str().is_none_or(|t| t == "command");
                    if !is_cmd {
                        continue;
                    }
                    let Some(cmd) = h["command"]
                        .as_str()
                        .map(str::trim)
                        .filter(|c| !c.is_empty())
                    else {
                        continue;
                    };
                    let timeout_ms = h["timeout_ms"]
                        .as_u64()
                        .or_else(|| h["timeout"].as_u64().map(to_ms));
                    out.push(HookEntry {
                        event: event.clone(),
                        matcher: matcher.clone(),
                        command: sub(cmd),
                        timeout_ms,
                    });
                }
            } else if let Some(cmd) = item["command"]
                .as_str()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                let timeout_ms = item["timeout_ms"]
                    .as_u64()
                    .or_else(|| item["timeout"].as_u64().map(to_ms));
                out.push(HookEntry {
                    event: event.clone(),
                    matcher,
                    command: sub(cmd),
                    timeout_ms,
                });
            }
        }
    }
    out
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
        hooks: hooks(v, root),
        mcp: mcp(v, root),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn claude_code_hooks_and_matchers_translate_to_zerith_tools() {
        assert_eq!(
            translate_hook_matcher("Edit|Write|MultiEdit").as_deref(),
            Some("edit_file|multi_edit|write_file")
        );
        assert_eq!(
            translate_hook_matcher("Bash(git commit *)|Read").as_deref(),
            Some("shell(git commit *)|read_file")
        );
        assert_eq!(
            translate_hook_matcher("TodoWrite").as_deref(),
            Some("task_plan|task_create|task_update")
        );
        assert_eq!(translate_hook_matcher("*"), None);
        assert_eq!(translate_hook_matcher("  "), None);

        let v = json!({
            "name": "sec",
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Edit|Write",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python3 ${CLAUDE_PLUGIN_ROOT}/hooks/check.py",
                                "timeout": 15
                            }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "command": "${CLAUDE_PLUGIN_ROOT}/hooks/init.sh",
                        "timeout_ms": 2500
                    }
                ]
            }
        });
        let parsed = hooks(&v, Path::new("/plugins/sec"));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].event, "PreToolUse");
        assert_eq!(
            parsed[0].matcher.as_deref(),
            Some("edit_file|multi_edit|write_file")
        );
        assert_eq!(parsed[0].command, "python3 /plugins/sec/hooks/check.py");
        assert_eq!(parsed[0].timeout_ms, Some(15_000));
        assert_eq!(parsed[1].event, "SessionStart");
        assert_eq!(parsed[1].matcher, None);
        assert_eq!(parsed[1].timeout_ms, Some(2500));
    }
}
