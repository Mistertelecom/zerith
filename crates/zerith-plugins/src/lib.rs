//! Plugins: metadata now, code later, and never code before review.
//!
//! A plugin is a directory with a `plugin.json` manifest. The host reads the
//! manifest and nothing else on discovery, so listing plugins cannot execute
//! one. Launching it spawns the declared command in its own process, exactly
//! like an MCP server: a plugin that crashes takes down its process, not the
//! application.
//!
//! That is the whole isolation story, and it is reported as such rather than
//! claimed as sandboxing: a plugin runs with the user's privileges.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zerith_mcp::Client;

pub const MANIFEST: &str = "plugin.json";

/// The manifest is the contract. Everything else is the plugin's business.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    /// The program to spawn. No shell, so the string is argv, not a command
    /// line a reviewer has to parse.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// What the plugin says it contributes. Advisory: the host will not show
    /// these until the plugin is launched and answers `tools/list`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Enabled,
    Disabled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Enabled => "enabled",
            Status::Disabled => "disabled",
        }
    }
}

/// What the host knows without starting anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub manifest: Manifest,
    pub status: Status,
    pub dir: PathBuf,
    pub installed_at: u64,
    /// Whether a human reviewed it. Loading metadata never sets this.
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {0} already installed")]
    Exists(String),
    #[error("no such plugin: {0}")]
    Missing(String),
    #[error("plugin {0} is disabled")]
    Disabled(String),
    #[error("plugin {0} was not reviewed: run `zerith plugins trust {0}` first")]
    NotTrusted(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Mcp(#[from] zerith_mcp::McpError),
}

pub type Result<T> = std::result::Result<T, PluginError>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    plugins: Vec<Plugin>,
}

/// The installed set. `open` reads the index; it never runs a plugin.
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("plugins.json")
    }

    pub fn plugins(&self) -> Result<Vec<Plugin>> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(path)?;
        let index: Index = serde_json::from_str(&text)?;
        Ok(index.plugins)
    }

    fn save(&self, plugins: &[Plugin]) -> Result<()> {
        let text = serde_json::to_string_pretty(&Index {
            plugins: plugins.to_vec(),
        })?;
        std::fs::write(self.index_path(), text)?;
        Ok(())
    }

    /// Copies a plugin directory in and records it as *disabled and
    /// untrusted*. Installing is not trusting, and this is the only place that
    /// distinction is enforced.
    pub fn install(&self, from: &Path) -> Result<Plugin> {
        let manifest = read_manifest(&from.join(MANIFEST))?;
        validate_name(&manifest.name)?;
        let mut plugins = self.plugins()?;
        if plugins.iter().any(|p| p.manifest.name == manifest.name) {
            return Err(PluginError::Exists(manifest.name));
        }
        let dest = self.root.join("installed").join(&manifest.name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        copy_dir(from, &dest)?;
        let plugin = Plugin {
            manifest,
            status: Status::Disabled,
            dir: dest,
            installed_at: now_ms(),
            trusted: false,
        };
        plugins.push(plugin.clone());
        self.save(&plugins)?;
        Ok(plugin)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let mut plugins = self.plugins()?;
        let at = plugins
            .iter()
            .position(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))?;
        let plugin = plugins.remove(at);
        if plugin.dir.starts_with(&self.root) && plugin.dir.exists() {
            std::fs::remove_dir_all(&plugin.dir)?;
        }
        self.save(&plugins)
    }

    pub fn set_status(&self, name: &str, status: Status) -> Result<()> {
        let mut plugins = self.plugins()?;
        let p = plugins
            .iter_mut()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))?;
        p.status = status;
        self.save(&plugins)
    }

    /// Marks a plugin as reviewed. Kept separate from `install` so no install
    /// path can silently grant execution.
    pub fn trust(&self, name: &str) -> Result<()> {
        let mut plugins = self.plugins()?;
        let p = plugins
            .iter_mut()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))?;
        p.trusted = true;
        self.save(&plugins)
    }

    /// Starts the plugin on first use. Nothing is spawned for a plugin that is
    /// disabled, untrusted or absent, and the error says which.
    pub fn launch(&self, name: &str) -> Result<Client> {
        let plugin = self
            .plugins()?
            .into_iter()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))?;
        if plugin.status == Status::Disabled {
            return Err(PluginError::Disabled(name.into()));
        }
        if !plugin.trusted {
            return Err(PluginError::NotTrusted(name.into()));
        }
        Ok(Client::connect(
            &plugin.manifest.name,
            &plugin.manifest.command,
            &plugin.manifest.args,
        )?)
    }
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PluginError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no {MANIFEST} in {}", path.display()),
            ))
        } else {
            PluginError::Io(e)
        }
    })?;
    Ok(serde_json::from_str(&text)?)
}

fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !name.starts_with('-')
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(PluginError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid plugin name: {name:?}"),
        )))
    }
}

/// Recursive copy without following symlinks: a plugin cannot pull the rest of
/// the filesystem into the library through a link.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
