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
//!
//! A plugin may also be a *bundle* (the Claude Code plugin idea): `skills`,
//! `agents`, `hooks` and `mcp` entries in the manifest. Nothing in a bundle is
//! active until the plugin is enabled **and** reviewed; [`Library::contributions`]
//! returns only what such plugins declare, and the host decides what to load.
//!
//! Plugins in the Claude Code layout (`.claude-plugin/plugin.json`) install
//! as they are ([`claude`]), and come from marketplaces ([`market`]): the
//! built-in `zerith` one or any `owner/repo` added by the person.

mod claude;
pub mod market;

pub use claude::translate_hook_matcher;

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
    /// May be empty for a bundle that has no process of its own.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// What the plugin says it contributes. Advisory: the host will not show
    /// these until the plugin is launched and answers `tools/list`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Directories (relative to the plugin) that hold a `SKILL.md`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Agent definition files (relative `*.md`), as in `zerith-core::agents`.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Shell hooks the plugin asks for. Run with the user's privileges.
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
    /// MCP servers the plugin ships.
    #[serde(default)]
    pub mcp: Vec<McpEntry>,
}

/// One hook, in the shape of a `[[hooks.<Event>]]` settings entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEntry {
    /// `SessionStart`, `PreToolUse`, ... (validated by the host that loads it).
    pub event: String,
    #[serde(default)]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpEntry {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// What enabled, reviewed plugins add. Paths are absolute, inside the
/// plugin's installed directory.
#[derive(Debug, Default, Clone)]
pub struct Contributions {
    /// `(plugin, skill directory)`
    pub skills: Vec<(String, PathBuf)>,
    /// `(plugin, agent file)`
    pub agents: Vec<(String, PathBuf)>,
    /// `(plugin, hook)`
    pub hooks: Vec<(String, HookEntry)>,
    /// Named `<plugin>-<server>` so two plugins cannot collide.
    pub mcp: Vec<(String, zerith_mcp::ServerConfig)>,
}

impl Contributions {
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        self.skills.iter().map(|(_, p)| p.clone()).collect()
    }

    pub fn agent_defs(&self) -> Vec<zerith_core::agents::AgentDef> {
        self.agents
            .iter()
            .filter_map(|(_, path)| zerith_core::agents::load_file(path, "plugin"))
            .collect()
    }

    pub fn apply_hooks(&self, settings: &mut zerith_core::Settings) {
        for (_, h) in &self.hooks {
            let slot = match h.event.as_str() {
                "SessionStart" => &mut settings.hooks.session_start,
                "UserPromptSubmit" => &mut settings.hooks.user_prompt_submit,
                "PreToolUse" => &mut settings.hooks.pre_tool_use,
                "PostToolUse" => &mut settings.hooks.post_tool_use,
                "PostToolUseFailure" => &mut settings.hooks.post_tool_use_failure,
                "PermissionRequest" => &mut settings.hooks.permission_request,
                "Stop" => &mut settings.hooks.stop,
                "PreCompact" => &mut settings.hooks.pre_compact,
                "SessionEnd" => &mut settings.hooks.session_end,
                "SubagentStart" => &mut settings.hooks.subagent_start,
                "SubagentStop" => &mut settings.hooks.subagent_stop,
                "Notification" => &mut settings.hooks.notification,
                "PostCompact" => &mut settings.hooks.post_compact,
                "TaskCompleted" => &mut settings.hooks.task_completed,
                _ => continue,
            };
            slot.push(zerith_core::settings::HookSpec {
                matcher: h.matcher.clone(),
                command: h.command.clone(),
                timeout_ms: h.timeout_ms.unwrap_or(60_000),
            });
        }
    }
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
    #[error("manifesto inválido: {0}")]
    Invalid(String),
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
    /// distinction is enforced. `from` holds a Zerith `plugin.json` or a Claude
    /// Code `.claude-plugin/plugin.json`.
    pub fn install(&self, from: &Path) -> Result<Plugin> {
        let claude = from.join(".claude-plugin").join(MANIFEST);
        if !claude.is_file() && from.join(MANIFEST).is_file() {
            let m = read_manifest(&from.join(MANIFEST))?;
            // A root `plugin.json` that declares nothing is a Claude Code
            // manifest kept at the root: find its parts by convention.
            let empty = m.command.trim().is_empty()
                && m.skills.is_empty()
                && m.agents.is_empty()
                && m.hooks.is_empty()
                && m.mcp.is_empty();
            if !empty {
                return self.install_as(from, m, None);
            }
            return self.install_claude(from, read_json(&from.join(MANIFEST))?);
        }
        self.install_claude(from, read_json(&claude)?)
    }

    /// A Claude Code plugin: `v` is its manifest (a marketplace entry's fields
    /// already merged in). The parts are found after the copy, so command
    /// skills are written into the installed folder, never the source.
    pub(crate) fn install_claude(&self, from: &Path, v: serde_json::Value) -> Result<Plugin> {
        let name = v["name"].as_str().unwrap_or_default().to_string();
        let shell = Manifest {
            name,
            version: String::new(),
            description: String::new(),
            license: None,
            command: String::new(),
            args: Vec::new(),
            capabilities: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: Vec::new(),
            mcp: Vec::new(),
        };
        self.install_as(from, shell, Some(v))
    }

    fn install_as(
        &self,
        from: &Path,
        mut manifest: Manifest,
        claude: Option<serde_json::Value>,
    ) -> Result<Plugin> {
        validate_name(&manifest.name)?;
        if claude.is_none() {
            validate_bundle(&manifest, from)?;
        }
        let mut plugins = self.plugins()?;
        if plugins.iter().any(|p| p.manifest.name == manifest.name) {
            return Err(PluginError::Exists(manifest.name));
        }
        let dest = self.root.join("installed").join(&manifest.name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        copy_dir(from, &dest)?;
        if let Some(v) = claude {
            let abs = std::path::absolute(&dest)?;
            let built = claude::manifest(&v, &abs).and_then(|m| {
                validate_bundle(&m, &dest)?;
                Ok(m)
            });
            match built {
                Ok(m) => manifest = m,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dest);
                    return Err(e);
                }
            }
        }
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

    /// Reverses [`Library::trust`]: the plugin stays installed but stops
    /// contributing and cannot launch.
    pub fn untrust(&self, name: &str) -> Result<()> {
        let mut plugins = self.plugins()?;
        let p = plugins
            .iter_mut()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))?;
        p.trusted = false;
        self.save(&plugins)
    }

    pub fn get(&self, name: &str) -> Result<Plugin> {
        self.plugins()?
            .into_iter()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| PluginError::Missing(name.into()))
    }

    /// Installs and enables official bundled plugins once on startup.
    ///
    /// - Plugins removed or disabled by the user stay untouched (tracked in
    ///   `bundled-seen.json`).
    /// - If an already-installed enabled bundled plugin changed in a newer app
    ///   release (another version in its manifest, or more skills), its files
    ///   are refreshed in place while preserving its status and trust flag.
    ///   Counting skills alone missed edits to a skill's text (0.3.8 → 0.3.9).
    pub fn seed_enabled(&self, bundle: &Path) -> Result<(Vec<String>, Vec<String>)> {
        let seen_path = self.root.join("bundled-seen.json");
        let mut seen: Vec<String> = std::fs::read(&seen_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut added = Vec::new();
        let mut failed = Vec::new();
        let Ok(entries) = std::fs::read_dir(bundle) else {
            return Ok((added, failed));
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.join(".claude-plugin").join(MANIFEST).is_file() || p.join(MANIFEST).is_file()
            })
            .collect();
        dirs.sort();
        for dir in dirs {
            let folder = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if seen.contains(&folder) {
                // Refresh in place if the user kept it enabled and the bundle changed.
                if let Ok(existing) = self.get(&folder) {
                    let bundle_skills = std::fs::read_dir(dir.join("skills"))
                        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
                        .unwrap_or(0);
                    let bundle_version = bundle_version(&dir);
                    let newer =
                        !bundle_version.is_empty() && bundle_version != existing.manifest.version;
                    if existing.status == Status::Enabled
                        && (newer || bundle_skills > existing.manifest.skills.len())
                    {
                        let was_trusted = existing.trusted;
                        let _ = self.remove(&existing.manifest.name);
                        if let Ok(p) = self.install(&dir) {
                            let _ = self.set_status(&p.manifest.name, Status::Enabled);
                            if was_trusted {
                                let _ = self.trust(&p.manifest.name);
                            }
                        }
                    }
                }
                continue;
            }
            match self.install(&dir) {
                Ok(p) => {
                    let _ = self.set_status(&p.manifest.name, Status::Enabled);
                    added.push(p.manifest.name);
                    seen.push(folder);
                }
                Err(PluginError::Exists(_)) => seen.push(folder),
                Err(e) => failed.push(format!("{folder}: {e}")),
            }
        }
        std::fs::write(&seen_path, serde_json::to_vec_pretty(&seen)?)?;
        Ok((added, failed))
    }

    /// What enabled plugins add. Skills and agents are text the model reads
    /// (a skill's `allowed-tools` grants nothing, and every command still
    /// goes through approval), so they load once the plugin is enabled; hooks
    /// and MCP servers run code and also need the plugin reviewed. Nothing is
    /// read from disk beyond the index and nothing is started: loading what
    /// this returns is the host's decision.
    pub fn contributions(&self) -> Result<Contributions> {
        let mut out = Contributions::default();
        for p in self.plugins()? {
            if p.status != Status::Enabled {
                continue;
            }
            let m = &p.manifest;
            out.skills
                .extend(m.skills.iter().map(|s| (m.name.clone(), p.dir.join(s))));
            out.agents
                .extend(m.agents.iter().map(|a| (m.name.clone(), p.dir.join(a))));
            if !p.trusted {
                continue;
            }
            out.hooks
                .extend(m.hooks.iter().map(|h| (m.name.clone(), h.clone())));
            for e in &m.mcp {
                out.mcp.push((
                    m.name.clone(),
                    zerith_mcp::ServerConfig {
                        name: format!("{}-{}", m.name, e.name),
                        command: e.command.clone(),
                        args: e.args.clone(),
                        url: e.url.clone(),
                        timeout_ms: None,
                        auth: None,
                        origin: zerith_mcp::Origin::User,
                    },
                ));
            }
        }
        Ok(out)
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
        if plugin.manifest.command.trim().is_empty() {
            return Err(PluginError::Invalid(format!(
                "{name} não declara `command`: é só um pacote de skills/agentes/hooks/mcp"
            )));
        }
        Ok(Client::connect(
            &plugin.manifest.name,
            &plugin.manifest.command,
            &plugin.manifest.args,
        )?)
    }
}

/// The `version` a bundled plugin declares (Claude Code layout or Zerith's
/// root `plugin.json`); empty when it declares none.
fn bundle_version(dir: &Path) -> String {
    [
        dir.join(".claude-plugin").join(MANIFEST),
        dir.join(MANIFEST),
    ]
    .iter()
    .find_map(|p| read_json(p).ok())
    .and_then(|v| v["version"].as_str().map(str::to_string))
    .unwrap_or_default()
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

/// Bundle entries must be relative paths that stay inside the plugin and point
/// at something that exists; hooks and MCP entries must be usable.
fn validate_bundle(m: &Manifest, root: &Path) -> Result<()> {
    let bad = |what: &str, why: String| PluginError::Invalid(format!("{what}: {why}"));
    let inside = |rel: &str, want_dir: bool| -> Result<()> {
        let p = Path::new(rel);
        let clean = !rel.is_empty()
            && p.is_relative()
            && p.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !clean {
            return Err(bad(rel, "use um caminho relativo sem `..`".into()));
        }
        let full = root.join(p);
        let ok = if want_dir {
            full.is_dir()
        } else {
            full.is_file()
        };
        if !ok {
            return Err(bad(rel, "não existe no plugin".into()));
        }
        Ok(())
    };
    for s in &m.skills {
        inside(s, true)?;
        inside(&format!("{s}/SKILL.md"), false)?;
    }
    for a in &m.agents {
        inside(a, false)?;
    }
    for h in &m.hooks {
        if h.event.trim().is_empty() || h.command.trim().is_empty() {
            return Err(bad(
                "hooks",
                "cada hook precisa de `event` e `command`".into(),
            ));
        }
    }
    for e in &m.mcp {
        let cfg = zerith_mcp::ServerConfig {
            name: e.name.clone(),
            command: e.command.clone(),
            args: e.args.clone(),
            url: e.url.clone(),
            timeout_ms: None,
            auth: None,
            origin: zerith_mcp::Origin::User,
        };
        cfg.validate().map_err(|why| bad("mcp", why))?;
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        PluginError::Invalid(format!(
            "sem {MANIFEST} nem .claude-plugin/{MANIFEST} em {} ({e})",
            path.parent()
                .and_then(Path::parent)
                .unwrap_or(path)
                .display()
        ))
    })?;
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
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
/// the filesystem into the library through a link. A clone's `.git` stays out.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if ty.is_symlink() || entry.file_name() == ".git" {
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
