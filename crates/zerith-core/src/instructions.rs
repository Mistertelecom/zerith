//! Project instructions: `AGENTS.md` (or `CLAUDE.md` where there is no
//! `AGENTS.md`) from the project directory up to the git root, plus the
//! user-level `<root>/AGENTS.md`.
//!
//! Order is general to specific — user file, then the outermost directory
//! down to the project — so the closest file has the last word, as a reader
//! would expect.

use crate::Paths;
use std::path::{Path, PathBuf};

/// Total characters of instructions that may reach the prompt.
pub const INSTRUCTIONS_BUDGET_BYTES: usize = 20 * 1024;

const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Every instruction file that applies to `project_dir`, in prompt order,
/// trimmed to `INSTRUCTIONS_BUDGET_BYTES` in total.
pub fn load_instructions(paths: &Paths, project_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut dirs = Vec::new();
    let mut dir = Some(project_dir);
    while let Some(d) = dir {
        dirs.push(d.to_path_buf());
        // The git root is the last directory that belongs to the project.
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    dirs.reverse();

    let mut files = Vec::new();
    if let Ok(text) = std::fs::read_to_string(paths.instructions()) {
        files.push((paths.instructions(), text));
    }
    for d in dirs {
        if let Some(found) = NAMES.iter().map(|n| d.join(n)).find(|p| p.is_file()) {
            if let Ok(text) = std::fs::read_to_string(&found) {
                files.push((found, text));
            }
        }
    }
    cap(files, INSTRUCTIONS_BUDGET_BYTES)
}

/// Keeps the files closest to the project whole and truncates the rest: the
/// project's own file is the one most likely to matter.
fn cap(files: Vec<(PathBuf, String)>, budget: usize) -> Vec<(PathBuf, String)> {
    let mut left = budget;
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for (path, text) in files.into_iter().rev() {
        if left == 0 {
            out.push((path, "[omitido: limite de instruções atingido]".into()));
            continue;
        }
        if text.len() <= left {
            left -= text.len();
            out.push((path, text));
        } else {
            let mut cut = left;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let note = format!(
                "{}\n[truncado: {} de {} bytes]",
                &text[..cut],
                cut,
                text.len()
            );
            left = 0;
            out.push((path, note));
        }
    }
    out.reverse();
    out
}

/// `system_prompt(title)` followed by the instruction files. The instructions
/// come after the fixed text so the provider's prefix cache still covers it.
pub fn system_prompt_with_instructions(title: &str, instructions: &[(PathBuf, String)]) -> String {
    let mut p = crate::system_prompt(title);
    for (path, text) in instructions {
        p.push_str(&format!(
            "\n\n# Instructions from {}\n\n{}",
            path.display(),
            text.trim_end()
        ));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_to_the_git_root_prefers_agents_and_caps() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&root).unwrap();
        let tmp = tempfile::Builder::new().tempdir_in(root).unwrap();
        let outer = tmp.path().join("outer");
        let repo = outer.join("repo");
        let sub = repo.join("sub");
        std::fs::create_dir_all(sub.clone()).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(outer.join("AGENTS.md"), "fora do repo").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "raiz").unwrap();
        std::fs::write(repo.join("CLAUDE.md"), "ignorado").unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "sub").unwrap();
        let paths = Paths::with_root(tmp.path().join("data"));
        std::fs::create_dir_all(paths.root()).unwrap();
        std::fs::write(paths.instructions(), "usuário").unwrap();

        let got = load_instructions(&paths, &sub);
        let texts: Vec<&str> = got.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["usuário", "raiz", "sub"]);

        let prompt = system_prompt_with_instructions("t", &got);
        assert!(prompt.starts_with(&crate::system_prompt("t")));
        assert!(prompt.ends_with("sub"));

        let capped = cap(
            vec![
                ("a".into(), "x".repeat(10)),
                ("b".into(), "y".repeat(10)),
                ("c".into(), "z".repeat(10)),
            ],
            15,
        );
        assert!(capped[0].1.starts_with("[omitido"));
        assert!(capped[1].1.contains("[truncado: 5 de 10 bytes]"));
        assert_eq!(capped[2].1, "z".repeat(10));
    }
}
