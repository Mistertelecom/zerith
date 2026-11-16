//! Per-OS data locations, namespaced `zerith`.
//!
//! Resolution order: `ZERITH_HOME` (tests, portable installs), then the OS
//! convention. Nothing here creates directories; call `Paths::ensure`.

use std::path::{Path, PathBuf};

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

    pub fn skills(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.skills())?;
        std::fs::create_dir_all(self.backups())?;
        Ok(())
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
