//! The one place the window touches other crates' persistence: settings,
//! secrets, memory pause, session search/archive/export, approvals, agents.
//! Every function takes the data root explicitly (`<root>/zerith.db` lives
//! beside `<root>/settings.toml`), so tests and snapshots stay isolated.

use std::path::{Path, PathBuf};

use zerith_core::settings::{self, ProviderEntry, WireProtocol};
use zerith_core::{Paths, Settings};
use zerith_memory::Scope;
use zerith_providers::{Protocol, ProviderConfig, ProviderKind};
use zerith_runtime::{Approval as RtApproval, ApprovalSender};
use zerith_sessions::Store;

use crate::screens::chat::{Approval, Decision};
use crate::{Profile, Theme};

/// The memory scope of this install; the same one the CLI uses.
pub fn memory_scope() -> Scope {
    zerith_memory::default_scope()
}

fn load(root: &Path) -> Settings {
    let cwd = std::env::current_dir().ok();
    Settings::load(&Paths::with_root(root), cwd.as_deref()).settings
}

fn kind_of(name: &str) -> ProviderKind {
    [
        ProviderKind::NineRouter,
        ProviderKind::OpenAi,
        ProviderKind::Anthropic,
        ProviderKind::DeepSeek,
        ProviderKind::Gemini,
        ProviderKind::OpenRouter,
        ProviderKind::Ollama,
        ProviderKind::LmStudio,
    ]
    .into_iter()
    .find(|k| k.as_str() == name)
    .unwrap_or(ProviderKind::Custom)
}

/// Providers: the environment's one first (what the CLI uses), then every
/// `[[providers]]` entry from the settings layers. Keys come from
/// `ZERITH_API_KEY` or the OS keyring, never from a file.
pub fn configured_providers(root: &Path) -> Vec<ProviderConfig> {
    let st = load(root);
    let mut env = ProviderConfig::from_env();
    if env.default_model.is_none() {
        env.default_model = st.model.clone();
    }
    let mut out = vec![env];
    for e in &st.providers {
        let mut c = ProviderConfig::new(kind_of(&e.name));
        c.name = e.name.clone();
        if let Some(u) = &e.base_url {
            c.base_url = u.clone();
        }
        if let Some(p) = e.protocol {
            c.protocol = match p {
                WireProtocol::OpenAi => Protocol::OpenAi,
                WireProtocol::Anthropic => Protocol::Anthropic,
            };
        }
        c.default_model = e.default_model.clone();
        if let Some(t) = e.timeout_ms {
            c.timeout_ms = t;
        }
        c.context_limit = e.context_limit;
        match out.iter_mut().find(|o| o.name == c.name) {
            Some(slot) => *slot = c,
            None => out.push(c),
        }
    }
    for c in &mut out {
        if c.api_key.is_none() {
            let key_ref = st
                .provider_entry(&c.name)
                .and_then(|p| p.api_key_ref.clone());
            c.api_key = zerith_core::secrets::resolve_api_key(&c.name, key_ref.as_deref())
                .ok()
                .flatten();
        }
    }
    out
}

/// Persists a provider into the user settings layer (never its key).
pub fn save_provider(root: &Path, cfg: &ProviderConfig) -> Result<(), String> {
    let entry = ProviderEntry {
        name: cfg.name.clone(),
        protocol: Some(match cfg.protocol {
            Protocol::OpenAi => WireProtocol::OpenAi,
            Protocol::Anthropic => WireProtocol::Anthropic,
        }),
        base_url: Some(cfg.base_url.clone()),
        default_model: cfg.default_model.clone(),
        timeout_ms: Some(cfg.timeout_ms),
        context_limit: cfg.context_limit,
        api_key_ref: Some(cfg.name.clone()),
    };
    Settings::save_user(&Paths::with_root(root), |s| {
        match s.providers.iter_mut().find(|p| p.name == entry.name) {
            Some(p) => *p = entry,
            None => s.providers.push(entry),
        }
    })
    .map_err(|e| e.to_string())
}

/// Stores an API key in the OS keyring under the provider's name.
pub fn store_key(provider: &str, key: &str) -> Result<(), String> {
    zerith_core::secrets::set_secret(provider, key).map_err(|e| e.to_string())
}

/// Answers a pending tool approval through the running turn's channel. With
/// no turn listening the answer is moot, and the turn fails closed anyway.
pub fn answer_approval(sender: Option<&ApprovalSender>, a: &Approval, d: Decision) {
    let (Some(tx), Ok(id)) = (sender, a.id.parse::<u64>()) else {
        return;
    };
    tx.answer(
        id,
        match d {
            Decision::Once => RtApproval::AllowOnce,
            Decision::Session => RtApproval::AllowSession,
            Decision::Deny => RtApproval::Deny,
        },
    );
}

/// Whether memory is paused: the database switch the runtime checks.
pub fn memory_paused(db: &Path) -> bool {
    zerith_memory::SqliteMemory::open(db)
        .and_then(|m| m.is_paused())
        .unwrap_or(false)
}

/// Pauses or resumes memory. Stored in the database, so the CLI and every
/// turn see the same switch.
pub fn set_memory_paused(db: &Path, paused: bool) -> Result<(), String> {
    zerith_memory::SqliteMemory::open(db)
        .and_then(|m| m.set_paused(paused))
        .map_err(|e| e.to_string())
}

/// Session ids whose title or messages match `query` (FTS5).
pub fn search_sessions(db: &Path, query: &str, limit: usize) -> Result<Vec<String>, String> {
    Store::open(db)
        .and_then(|st| st.search_sessions(query, limit))
        .map(|v| v.into_iter().map(|s| s.id).collect())
        .map_err(|e| e.to_string())
}

/// Archives or restores a session.
pub fn set_archived(db: &Path, id: &str, archived: bool) -> Result<(), String> {
    Store::open(db)
        .and_then(|st| st.set_archived(id, archived))
        .map_err(|e| e.to_string())
}

/// Writes a session as Markdown under `<root>/exports` and returns where.
pub fn export_session(db: &Path, id: &str) -> Result<PathBuf, String> {
    let dir = db.parent().unwrap_or(Path::new(".")).join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.md"));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    Store::open(db)
        .and_then(|st| st.export_session_markdown(id, std::io::BufWriter::new(file)))
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// One agent definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    pub path: PathBuf,
    pub summary: String,
}

/// Agent definitions: `*.md` files in `./.zerith/agents`, first non-empty
/// line as summary.
// TODO(wire): zerith_core::agents::list(paths) -> Result<Vec<AgentDef>> once
// the core owns agent definitions; no crate exposes them yet.
pub fn agents() -> Result<Vec<Agent>, String> {
    let dir = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(".zerith")
        .join("agents");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Agent> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .map(|path| {
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            let summary = body
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && *l != "---")
                .unwrap_or("")
                .trim_start_matches('#')
                .trim()
                .to_string();
            Agent {
                name: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path,
                summary,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// What the window reads from settings at startup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiPrefs {
    pub theme: Theme,
    pub reduce_motion: bool,
    pub profile: Profile,
}

/// Theme, motion and profile from the merged settings layers. A broken
/// settings file is skipped by the loader, so this falls back to defaults.
pub fn load_prefs(root: &Path) -> UiPrefs {
    let st = load(root);
    UiPrefs {
        theme: match st.ui.theme {
            settings::Theme::Dark => Theme::Dark,
            settings::Theme::Light => Theme::Light,
            settings::Theme::System => Theme::System,
        },
        reduce_motion: st.ui.reduced_motion,
        profile: match st.profile {
            settings::Profile::Economy => Profile::Economy,
            settings::Profile::Balanced => Profile::Balanced,
            settings::Profile::Performance => Profile::Performance,
        },
    }
}

/// Writes theme, motion and profile to the user settings layer. Runs off the
/// UI thread unless `inline`: a rename on Windows can wait on antivirus.
pub fn save_prefs(root: &Path, prefs: UiPrefs, inline: bool) {
    let paths = Paths::with_root(root);
    let job = move || {
        let r = Settings::save_user(&paths, |s| {
            s.ui.theme = match prefs.theme {
                Theme::Dark => settings::Theme::Dark,
                Theme::Light => settings::Theme::Light,
                Theme::System => settings::Theme::System,
            };
            s.ui.reduced_motion = prefs.reduce_motion;
            s.profile = match prefs.profile {
                Profile::Economy => settings::Profile::Economy,
                Profile::Balanced => settings::Profile::Balanced,
                Profile::Performance => settings::Profile::Performance,
            };
        });
        if let Err(e) = r {
            eprintln!("zerith-ui: não consegui salvar as configurações: {e}");
        }
    };
    if inline {
        job();
    } else {
        std::thread::spawn(job);
    }
}

/// Opens a folder in the OS file manager.
pub fn reveal(path: &Path) -> Result<(), String> {
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
