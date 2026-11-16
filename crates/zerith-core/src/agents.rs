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
//! The frontmatter is optional and only knows `description` and `model`;
//! anything else is ignored so a file written for another tool still loads.

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
    /// `user` or `project`: which layer the definition came from.
    pub layer: &'static str,
}

/// Every agent visible from `project_dir`, sorted by name. Unreadable files
/// are skipped: one broken definition must not hide the others.
pub fn list(paths: &Paths, project_dir: Option<&Path>) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    let mut layers = vec![(paths.agents(), "user")];
    if let Some(dir) = project_dir {
        layers.push((dir.join(".zerith").join("agents"), "project"));
    }
    for (dir, layer) in layers {
        for def in read_dir(&dir, layer) {
            match out.iter_mut().find(|a| a.name == def.name) {
                Some(slot) => *slot = def,
                None => out.push(def),
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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

fn read_dir(dir: &Path, layer: &'static str) -> Vec<AgentDef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            if name == DEFAULT {
                return None;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            Some(parse(name, &text, path, layer))
        })
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
    let mut description = String::new();
    let mut model = None;
    for line in header.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        match k.trim() {
            "description" => description = v,
            "model" if !v.is_empty() => model = Some(v),
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
    }
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
}
