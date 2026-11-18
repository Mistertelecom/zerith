//! Per-OS data locations, namespaced `zerith`.
//!
//! Resolution order: `ZERITH_HOME` (tests, portable installs), then the OS
//! convention. Nothing here creates directories; call `Paths::ensure`.

use std::path::{Path, PathBuf};

use crate::diagnose;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        if let Some(home) = std::env::var_os("ZERITH_HOME") {
            return Self {
                root: PathBuf::from(home),
            };
        }
        Self {
            root: default_root(),
        }
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("zerith.db")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// User settings layer (see `settings`). `config()` is kept for callers
    /// that already use it; nothing reads `config.toml`.
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// User-level project instructions, applied to every project.
    pub fn instructions(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }

    /// User-level agent definitions (`*.md`); see `agents`.
    pub fn agents(&self) -> PathBuf {
        self.root.join("agents")
    }

    pub fn skills(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// The ready-made skills that ship with the app: a `bundled-skills` folder
    /// next to the executable, or `ZERITH_BUNDLED_SKILLS` to point elsewhere
    /// (a dev checkout). `None` when there is no such folder.
    pub fn bundled_skills(&self) -> Option<PathBuf> {
        let explicit = std::env::var_os("ZERITH_BUNDLED_SKILLS").map(PathBuf::from);
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("bundled-skills")));
        explicit.into_iter().chain(beside_exe).find(|p| p.is_dir())
    }

    /// Programs that ship with the app and that skills rely on (for example
    /// `agent-desktop`): a `bundled-tools` folder next to the executable, or
    /// `ZERITH_BUNDLED_TOOLS`. `None` when there is none.
    pub fn bundled_tools(&self) -> Option<PathBuf> {
        let explicit = std::env::var_os("ZERITH_BUNDLED_TOOLS").map(PathBuf::from);
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("bundled-tools")));
        explicit.into_iter().chain(beside_exe).find(|p| p.is_dir())
    }

    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// The folders the app writes to, relative to the root.
    pub const FOLDERS: [&'static str; 3] = ["", "skills", "backups"];

    /// Creates the data folders. A failure names the folder and the operation,
    /// because "Acesso negado" alone leaves the user guessing which of several
    /// paths it was; the folder is also probed so the message can say why.
    pub fn ensure(&self) -> std::io::Result<()> {
        for sub in Self::FOLDERS {
            let dir = if sub.is_empty() {
                self.root.clone()
            } else {
                self.root.join(sub)
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "não consegui criar a pasta {}: {e}{}",
                        dir.display(),
                        diagnose::hint(&dir, &e)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Checks that every data folder can be created, written and deleted in,
    /// by really doing it. Never changes permissions.
    pub fn check_writable(&self) -> Vec<diagnose::FolderCheck> {
        Self::FOLDERS
            .iter()
            .map(|sub| {
                let dir = if sub.is_empty() {
                    self.root.clone()
                } else {
                    self.root.join(sub)
                };
                diagnose::probe(&dir)
            })
            .collect()
    }

    pub fn is_inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

fn default_root() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Zerith")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Zerith")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("zerith")
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_sits_under_root_and_inside_check_holds() {
        let p = Paths::with_root("/tmp/z1");
        assert_eq!(p.db(), Path::new("/tmp/z1/zerith.db"));
        assert!(p.is_inside(Path::new("/tmp/z1/zerith.db")));
        assert!(!p.is_inside(Path::new("/tmp/z2/zerith.db")));
    }
}
