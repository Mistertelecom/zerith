//! Agent definitions: Markdown files whose body is the agent's instructions.
//!
//! Two layers, low to high: the user's `<root>/agents/*.md`, then the
//! project's `<project>/.zerith/agents/*.md`. A project file with the same
//! name replaces the user's, as settings layers do. The file name (without
//! `.md`) is the agent's name; a plan task's `agent` field refers to it.
//!
//! ```text
//! ---
//! description: Revisa diffs Rust
//! model: gpt-4o-mini
//! ---
//! Você revisa diffs. Aponte unwrap em caminhos de erro…
//! ```
//!
//! The frontmatter is optional. Fields (Claude Code's names):
//!
//! | Field | Meaning |
//! | --- | --- |
//! | `name` | overrides the file name |
//! | `description` | what the agent is for |
//! | `tools` | allowlist; absent = every tool of the parent |
//! | `disallowedTools` | removed from whatever `tools` leaves |
//! | `model` | preferred model; the host may override |
//! | `permissionMode` | `default`, `acceptEdits`, `plan`, `dontAsk`, `bypassPermissions` |
//! | `maxTurns` | step limit for this agent |
//! | `skills` | skills preloaded for this agent |
//! | `isolation` | `worktree`: run in its own git worktree (not run yet: warns, uses the workspace) |
//! | `background` | run without blocking the parent |
//!
//! Lists are `a, b`, `[a, b]` or YAML `- a` lines. Unknown keys are ignored so
//! a file written for another tool still loads. Claude Code tool names
//! (`Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`) are mapped to Zerith's.
//! Subagent execution is not here: this module only reads definitions.

use crate::Paths;
use std::path::{Path, PathBuf};

/// The agent every turn uses when none is named. Never read from disk.
pub const DEFAULT: &str = "default";

/// Largest instruction body that reaches the prompt, in bytes.
pub const MAX_INSTRUCTIONS: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Model this agent prefers; the host may still override it.
    pub model: Option<String>,
    pub instructions: String,
    pub path: PathBuf,
    /// `plugin`, `user` or `project`: which layer the definition came from.
    pub layer: &'static str,
    /// Allowlist (`tools:`); `None` inherits every tool.
    pub tools: Option<Vec<String>>,
    /// `disallowedTools:`, removed after the allowlist.
    pub disallowed_tools: Vec<String>,
    /// `permissionMode:` as written; parse it with `zerith_security::Mode::parse`.
    pub permission_mode: Option<String>,
    pub max_turns: Option<u32>,
    pub skills: Vec<String>,
    pub isolation: Isolation,
    pub background: bool,
    /// `created-by: ai`: written by the model through `create_agent`. The UI
    /// shows these with an `IA` badge; the AI may edit or delete only these.
    pub created_by_ai: bool,
    /// `disabled: true`: on disk but invisible to [`list`] and [`find`] until
    /// the person enables it ([`set_disabled`]). AI-created agents start so.
    pub disabled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Isolation {
    #[default]
    None,
    /// Run in a temporary git worktree.
    Worktree,
}

impl AgentDef {
    /// The tools this agent may use out of `all` (the parent's tool names):
    /// the `tools` allowlist (absent = all), minus `disallowedTools`. Entries
    /// are exact names, `*` globs (`mcp__github__*`) or Claude Code names;
    /// `Tool(specifier)` entries count as the tool (specifiers are permission
    /// rules, not availability) in an allowlist and are ignored in a denylist.
    /// Order of `all` is kept.
    pub fn effective_tools(&self, all: &[&str]) -> Vec<String> {
        let allow: Option<Vec<String>> = self.tools.as_ref().map(|t| names(t));
        let deny: Vec<String> = self
            .disallowed_tools
            .iter()
            .filter(|t| !t.contains('('))
            .map(|t| canonical(t))
            .collect();
        let hit = |list: &[String], tool: &str| {
            list.iter()
                .any(|p| p == tool || (p.contains('*') && crate::settings::glob_match(p, tool)))
        };
        all.iter()
            .filter(|t| allow.as_ref().is_none_or(|a| hit(a, t)))
            .filter(|t| !hit(&deny, t))
            .map(|t| t.to_string())
            .collect()
    }
}

/// Strips ONE matching pair of quotes. `trim_matches` would eat every quote at
/// the ends, so a value like `disse 'x'` would lose its closing one.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Tool names with Claude Code's spellings mapped to Zerith's and any
/// `(specifier)` dropped. Used to compare what an agent asks for with what a
/// turn has.
pub fn canonical_names(list: &[String]) -> Vec<String> {
    names(list)
}

fn names(list: &[String]) -> Vec<String> {
    list.iter()
        .map(|t| canonical(t.split('(').next().unwrap_or(t).trim()))
        .collect()
}

/// Claude Code tool names to Zerith's; anything else is kept as written.
fn canonical(t: &str) -> String {
    match t.trim() {
        "Read" => "read_file",
        "Write" => "write_file",
        "Edit" | "MultiEdit" => "edit_file",
        "Bash" | "Shell" => "shell",
        "Glob" => "glob",
        "Grep" => "grep",
        "LS" => "list_dir",
        other => other,
    }
    .to_string()
}

/// Every agent visible from `project_dir`, sorted by name. Unreadable files
/// are skipped: one broken definition must not hide the others.
pub fn list(paths: &Paths, project_dir: Option<&Path>) -> Vec<AgentDef> {
    list_checked(paths, project_dir).0
}

/// Like [`list`], but also the agents switched off (`disabled: true`), for the
/// screen where the person reviews and enables them. Nothing else should use
/// this: a disabled agent must not be reachable from a turn or a plan.
pub fn list_all(paths: &Paths, project_dir: Option<&Path>) -> Vec<AgentDef> {
    collect(paths, project_dir, &[], true).0
}

/// [`list`] plus one warning per name clash. Project beats user (the project
/// definition replaces the user's); two files of the *same* layer with the
/// same name (`a.md`, and `name: a` inside `b.md`) keep the first in
/// file-name order and warn, so the outcome never depends on directory order.
pub fn list_checked(paths: &Paths, project_dir: Option<&Path>) -> (Vec<AgentDef>, Vec<String>) {
    collect(paths, project_dir, &[], false)
}

/// Every agent visible from `project_dir`, including plugin-contributed files.
/// Plugin definitions load first (keeping the first on internal clashes); user
/// and project layers then overwrite plugins on name collisions.
pub fn list_with_extra(
    paths: &Paths,
    project_dir: Option<&Path>,
    extra_files: &[(String, PathBuf)],
) -> Vec<AgentDef> {
    collect(paths, project_dir, extra_files, false).0
}

fn collect(
    paths: &Paths,
    project_dir: Option<&Path>,
    extra_files: &[(String, PathBuf)],
    include_disabled: bool,
) -> (Vec<AgentDef>, Vec<String>) {
    let mut out: Vec<AgentDef> = Vec::new();
    let mut warnings = Vec::new();
    for (_, path) in extra_files {
        if let Some(def) = load_file(path, "plugin") {
            if !out.iter().any(|a| a.name == def.name) {
                out.push(def);
            }
        }
    }
    if extra_files.is_empty() {
        let idx = paths.root().join("plugins").join("plugins.json");
        if let Ok(text) = std::fs::read_to_string(idx) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                for p in v["plugins"].as_array().into_iter().flatten() {
                    if p["status"].as_str() != Some("enabled") {
                        continue;
                    }
                    let Some(dir) = p["dir"].as_str().map(PathBuf::from) else {
                        continue;
                    };
                    for rel in p["manifest"]["agents"].as_array().into_iter().flatten() {
                        if let Some(rel_str) = rel.as_str() {
                            if let Some(def) = load_file(&dir.join(rel_str), "plugin") {
                                if !out.iter().any(|a| a.name == def.name) {
                                    out.push(def);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut layers = vec![(paths.agents(), "user")];
    if let Some(dir) = project_dir {
        layers.push((dir.join(".zerith").join("agents"), "project"));
    }
    for (dir, layer) in layers {
        let mut seen: Vec<String> = Vec::new();
        for def in read_dir(&dir, layer) {
            if seen.contains(&def.name) {
                warnings.push(format!(
                    "{}: agente `{}` repetido na camada {layer}; ignorado",
                    def.path.display(),
                    def.name
                ));
                continue;
            }
            seen.push(def.name.clone());
            match out.iter_mut().find(|a| a.name == def.name) {
                Some(slot) => *slot = def,
                None => out.push(def),
            }
        }
    }
    if !include_disabled {
        out.retain(|a| !a.disabled);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    warnings.extend(out.iter().filter_map(worktree_warning));
    (out, warnings)
}

/// `isolation: worktree` is read but nothing creates the worktree yet: say so
/// instead of letting the person believe the agent is isolated.
pub fn worktree_warning(a: &AgentDef) -> Option<String> {
    (a.isolation == Isolation::Worktree).then(|| {
        format!(
            "{}: agente `{}` pede `isolation: worktree`, que ainda não é suportado; ele roda no workspace da conversa",
            a.path.display(),
            a.name
        )
    })
}

/// The named agent, or `None` for `default` and for names nobody defined.
pub fn find(paths: &Paths, project_dir: Option<&Path>, name: &str) -> Option<AgentDef> {
    if name.is_empty() || name == DEFAULT {
        return None;
    }
    list(paths, project_dir)
        .into_iter()
        .find(|a| a.name == name)
}

/// Like [`find`], also consulting plugin-contributed agent files.
pub fn find_with_extra(
    paths: &Paths,
    project_dir: Option<&Path>,
    extra_files: &[(String, PathBuf)],
    name: &str,
) -> Option<AgentDef> {
    if name.is_empty() || name == DEFAULT {
        return None;
    }
    list_with_extra(paths, project_dir, extra_files)
        .into_iter()
        .find(|a| a.name == name)
}

/// Loads one `.md` agent definition, using the file stem as initial name.
pub fn load_file(path: &Path, layer: &'static str) -> Option<AgentDef> {
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(path).ok()?;
    let def = parse(name, &text, path.to_path_buf(), layer);
    (def.name != DEFAULT).then_some(def)
}

fn read_dir(dir: &Path, layer: &'static str) -> Vec<AgentDef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| load_file(&path, layer))
        .collect()
}

/// Splits optional `---` frontmatter from the body.
pub fn parse(name: String, text: &str, path: PathBuf, layer: &'static str) -> AgentDef {
    let text = text.trim_start_matches('\u{feff}');
    let (header, body) = match text.strip_prefix("---") {
        Some(rest) => match rest.find("\n---") {
            Some(end) => {
                let after = &rest[end + 4..];
                (&rest[..end], after.strip_prefix('\r').unwrap_or(after))
            }
            None => ("", text),
        },
        None => ("", text),
    };
    let mut name = name;
    let mut description = String::new();
    let mut model = None;
    let mut def_tools = None;
    let mut disallowed = Vec::new();
    let mut permission_mode = None;
    let mut max_turns = None;
    let mut skills = Vec::new();
    let mut isolation = Isolation::None;
    let mut background = false;
    let mut created_by_ai = false;
    let mut disabled = false;
    let lines: Vec<&str> = header.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        // Top-level `key: value` only; indented lines belong to a list.
        if line.starts_with([' ', '\t', '-']) {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let raw = v.trim();
        let v = unquote(raw);
        // A YAML block list: the `- item` lines that follow an empty value.
        let mut block = Vec::new();
        if raw.is_empty() {
            while i < lines.len() && lines[i].trim_start().starts_with('-') {
                block.push(lines[i].trim_start()[1..].trim().to_string());
                i += 1;
            }
        }
        let list = || {
            let mut items = block.clone();
            items.extend(split_list(raw));
            items.retain(|x| !x.is_empty());
            items
        };
        match k.trim() {
            "name" if is_valid_name(&v) => name = v,
            "description" => description = v,
            "model" if !v.is_empty() && v != "inherit" => model = Some(v),
            "tools" => def_tools = Some(list()).filter(|l| !l.is_empty()),
            "disallowedTools" | "disallowed_tools" => disallowed = list(),
            "permissionMode" | "permission_mode" if !v.is_empty() => permission_mode = Some(v),
            "maxTurns" | "max_turns" => max_turns = v.parse().ok().filter(|n| *n > 0),
            "skills" => skills = list(),
            "isolation" => {
                isolation = if v == "worktree" {
                    Isolation::Worktree
                } else {
                    Isolation::None
                }
            }
            "background" => background = v == "true",
            "created-by" | "created_by" => created_by_ai = v.eq_ignore_ascii_case("ai"),
            "disabled" => disabled = v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    let mut instructions = body.trim().to_string();
    if instructions.len() > MAX_INSTRUCTIONS {
        let mut cut = MAX_INSTRUCTIONS;
        while !instructions.is_char_boundary(cut) {
            cut -= 1;
        }
        instructions.truncate(cut);
        instructions.push_str("\n[truncado]");
    }
    if description.is_empty() {
        // The first line of the body says what the agent is for.
        description = instructions
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .trim_start_matches('#')
            .trim()
            .to_string();
    }
    AgentDef {
        name,
        description,
        model,
        instructions,
        path,
        layer,
        tools: def_tools,
        disallowed_tools: disallowed,
        permission_mode,
        max_turns,
        skills,
        isolation,
        background,
        created_by_ai,
        disabled,
    }
}

/// A name that is safe as a plan `agent:` reference and a file stem.
fn is_valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `a, b`, `[a, b]` or `["a", "b"]`; empty for an empty value (block list).
fn split_list(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|x| unquote(x.trim()))
        .filter(|x| !x.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new().tempdir_in(root).unwrap()
    }

    #[test]
    fn frontmatter_is_optional_and_body_is_the_prompt() {
        let a = parse(
            "rev".into(),
            "---\ndescription: \"Revisa diffs\"\nmodel: m1\ntools: x\n---\nVocê revisa.\n",
            PathBuf::new(),
            "user",
        );
        assert_eq!(a.description, "Revisa diffs");
        assert_eq!(a.model.as_deref(), Some("m1"));
        assert_eq!(a.instructions, "Você revisa.");

        let b = parse(
            "doc".into(),
            "# Escreve docs\n\nCurto.",
            PathBuf::new(),
            "user",
        );
        assert_eq!(b.description, "Escreve docs");
        assert!(b.model.is_none());
        assert!(b.instructions.starts_with("# Escreve docs"));
    }

    #[test]
    fn project_layer_wins_and_default_is_reserved() {
        let tmp = scratch();
        let paths = Paths::with_root(tmp.path().join("home"));
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(paths.agents()).unwrap();
        std::fs::create_dir_all(project.join(".zerith/agents")).unwrap();
        std::fs::write(paths.agents().join("rev.md"), "do usuário").unwrap();
        std::fs::write(paths.agents().join("only-user.md"), "u").unwrap();
        std::fs::write(paths.agents().join("default.md"), "ignorado").unwrap();
        std::fs::write(project.join(".zerith/agents/rev.md"), "do projeto").unwrap();
        std::fs::write(project.join(".zerith/agents/notes.txt"), "não é agente").unwrap();

        let all = list(&paths, Some(&project));
        let names: Vec<&str> = all.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["only-user", "rev"]);
        let rev = find(&paths, Some(&project), "rev").unwrap();
        assert_eq!(rev.instructions, "do projeto");
        assert_eq!(rev.layer, "project");
        assert!(find(&paths, Some(&project), DEFAULT).is_none());
        assert!(find(&paths, Some(&project), "nobody").is_none());
        assert!(list(&paths, None).iter().all(|a| a.layer == "user"));
    }

    fn def(text: &str) -> AgentDef {
        parse("x".into(), text, PathBuf::new(), "user")
    }

    #[test]
    fn claude_code_frontmatter_fields_are_read() {
        let a = def(
            "---\nname: reviewer\ndescription: Revisa\ntools: Read, Grep, Bash(git *)\n\
             disallowedTools: [write_file, \"edit_file\"]\nmodel: inherit\npermissionMode: plan\n\
             maxTurns: 7\nskills:\n  - rust\n  - docs\nisolation: worktree\nbackground: true\n---\ncorpo",
        );
        assert_eq!(a.name, "reviewer");
        assert_eq!(a.tools.as_deref().unwrap(), ["Read", "Grep", "Bash(git *)"]);
        assert_eq!(a.disallowed_tools, ["write_file", "edit_file"]);
        assert_eq!(a.model, None, "`inherit` means no preference");
        assert_eq!(a.permission_mode.as_deref(), Some("plan"));
        assert_eq!(a.max_turns, Some(7));
        assert_eq!(a.skills, ["rust", "docs"]);
        assert_eq!(a.isolation, Isolation::Worktree);
        assert!(a.background);
        assert_eq!(a.instructions, "corpo");

        let plain = def("só corpo");
        assert!(plain.tools.is_none() && plain.skills.is_empty() && !plain.background);
        assert_eq!(plain.isolation, Isolation::None);
        // A bad `name` falls back to the file name.
        assert_eq!(def("---\nname: a b/c\n---\nx").name, "x");
    }

    #[test]
    fn effective_tools_applies_allowlist_then_denylist() {
        let all = [
            "read_file",
            "write_file",
            "edit_file",
            "shell",
            "mcp__gh__search",
            "mcp__gh__push",
        ];
        assert_eq!(def("x").effective_tools(&all).len(), 6);

        let a = def(
            "---\ntools: Read, Bash(git *), mcp__gh__*\ndisallowedTools: mcp__gh__push\n---\nx",
        );
        assert_eq!(
            a.effective_tools(&all),
            ["read_file", "shell", "mcp__gh__search"]
        );

        let d = def("---\ndisallowedTools: Write, Edit, Bash(rm *)\n---\nx");
        assert_eq!(
            d.effective_tools(&all),
            ["read_file", "shell", "mcp__gh__search", "mcp__gh__push"],
            "a specifier in disallowedTools is a rule, not an availability switch"
        );
        assert!(def("---\ntools: nothing\n---\nx")
            .effective_tools(&all)
            .is_empty());
    }

    #[test]
    fn a_same_layer_clash_keeps_the_first_file_and_warns() {
        let tmp = scratch();
        let paths = Paths::with_root(tmp.path().join("home"));
        std::fs::create_dir_all(paths.agents()).unwrap();
        std::fs::write(paths.agents().join("a.md"), "primeiro").unwrap();
        std::fs::write(paths.agents().join("b.md"), "---\nname: a\n---\nsegundo").unwrap();
        let (all, warnings) = list_checked(&paths, None);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].instructions, "primeiro");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("repetido"));
    }

    #[test]
    fn worktree_isolation_warns_that_it_runs_in_the_workspace() {
        let tmp = scratch();
        let paths = Paths::with_root(tmp.path().join("home"));
        std::fs::create_dir_all(paths.agents()).unwrap();
        std::fs::write(
            paths.agents().join("iso.md"),
            "---\nisolation: worktree\n---\nx",
        )
        .unwrap();
        std::fs::write(paths.agents().join("plain.md"), "y").unwrap();
        let (all, warnings) = list_checked(&paths, None);
        assert_eq!(all.len(), 2, "the agent still loads");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("`iso`") && warnings[0].contains("ainda não é suportado"));
        assert!(warnings[0].contains("roda no workspace"));
        assert!(worktree_warning(&all[1]).is_none());
    }

    #[test]
    fn extra_files_load_as_plugin_and_user_wins_on_collision() {
        let tmp = scratch();
        let paths = Paths::with_root(tmp.path().join("home"));
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(paths.agents()).unwrap();
        std::fs::create_dir_all(project.join(".zerith/agents")).unwrap();

        let pdir = tmp.path().join("plugin");
        std::fs::create_dir_all(&pdir).unwrap();
        let p_rev = pdir.join("rev.md");
        let p_dup = pdir.join("dup.md");
        let p_only = pdir.join("only-plugin.md");
        let p_default = pdir.join("default.md");
        std::fs::write(&p_rev, "do plugin").unwrap();
        std::fs::write(&p_dup, "---\nname: only-plugin\n---\nsegundo plugin").unwrap();
        std::fs::write(&p_only, "primeiro plugin").unwrap();
        std::fs::write(&p_default, "ignorado").unwrap();

        std::fs::write(paths.agents().join("rev.md"), "do usuario").unwrap();

        let extra = vec![
            ("p1".to_string(), p_rev),
            ("p1".to_string(), p_only),
            ("p2".to_string(), p_dup),
            ("p2".to_string(), p_default),
        ];

        let all = list_with_extra(&paths, Some(&project), &extra);
        let names: Vec<&str> = all.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["only-plugin", "rev"]);

        let rev = find_with_extra(&paths, Some(&project), &extra, "rev").unwrap();
        assert_eq!(rev.layer, "user");
        assert_eq!(rev.instructions, "do usuario");

        let only = find_with_extra(&paths, Some(&project), &extra, "only-plugin").unwrap();
        assert_eq!(only.layer, "plugin");
        assert_eq!(only.instructions, "primeiro plugin");

        assert!(find_with_extra(&paths, Some(&project), &extra, DEFAULT).is_none());
        assert!(find_with_extra(&paths, Some(&project), &extra, "").is_none());
    }
}
