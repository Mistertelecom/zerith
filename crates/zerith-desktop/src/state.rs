//! The one thing the commands share: where the data root is. Every path the
//! window touches is derived from it, so tests and a scratch profile are the
//! same code with a different root.

use std::path::{Path, PathBuf};

use zerith_core::Paths;

/// Process-wide state handed to every command.
#[derive(Clone)]
pub struct AppState {
    root: PathBuf,
}

impl AppState {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The resolved data root (`ZERITH_HOME`, else the OS user dir).
    pub fn resolve() -> Self {
        Self::new(Paths::resolve().root().to_path_buf())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/zerith.db`: sessions, memory and usage in one file, as the CLI
    /// uses it.
    pub fn db(&self) -> PathBuf {
        Paths::with_root(&self.root).db()
    }

    /// Creates the folder layout on first run; idempotent.
    pub fn ensure(&self) -> std::io::Result<()> {
        Paths::with_root(&self.root).ensure()
    }
}
