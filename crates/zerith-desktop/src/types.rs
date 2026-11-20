//! The few plain types the bridge and the window share: no Tauri, no I/O.
//! They are the vocabulary the commands speak.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zerith_providers::media::{KeyInfo, Kind, ModelPage};
use zerith_providers::ProviderConfig;

/// Appearance. `System` follows the OS and is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }
    pub fn parse(s: Option<&str>) -> Theme {
        match s {
            Some("dark") => Theme::Dark,
            Some("light") => Theme::Light,
            _ => Theme::System,
        }
    }
}

/// Execution profile: how much history, how many subagents, which caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Economy,
    Balanced,
    Performance,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Economy => "economy",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
        }
    }
    pub fn parse(s: Option<&str>) -> Profile {
        match s {
            Some("economy") => Profile::Economy,
            Some("performance") => Profile::Performance,
            _ => Profile::Balanced,
        }
    }
}

/// A tool call waiting for the user. The window shows one card per approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub tool: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Once,
    Session,
    Deny,
}

/// Where the OpenRouter key the media screen found comes from. The key itself
/// never reaches the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    Keyring,
    Env,
}

type ModelsFn = Arc<dyn Fn(Kind, &str, usize, usize) -> Result<ModelPage, String> + Send + Sync>;
type ResolveFn = Arc<dyn Fn() -> Result<Option<KeySource>, String> + Send + Sync>;
type SaveFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
type CheckFn = Arc<dyn Fn() -> Result<KeyInfo, String> + Send + Sync>;

/// Everything the media screen asks of the outside world. The real one and a
/// fake have the same shape.
#[derive(Clone)]
pub struct Backend {
    /// `OpenRouter::models(kind, query, offset, limit)`. Needs no key.
    pub models: ModelsFn,
    /// Is there a key, and where is it. Never returns the key itself.
    pub resolve_key: ResolveFn,
    /// Writes the key to the OS keyring and nowhere else.
    pub save_key: SaveFn,
    /// `OpenRouter::check_key` with the stored key.
    pub check_key: CheckFn,
}

/// Price of a model in USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input_per_m: f64,
    pub output_per_m: f64,
}

/// Whether `path` is an absolute path under `dir`, with no `..`, no scheme.
/// Lexical on purpose: a view must not touch the disk.
pub fn is_under(dir: &Path, path: &Path) -> bool {
    fn norm(s: &str) -> String {
        s.replace('\\', "/")
    }
    let s = path.to_string_lossy();
    // `http://x`, `data:image/…`, `file:///x`. A Windows drive (`C:\`) is a
    // single letter before the colon and is not a scheme.
    if let Some((scheme, _)) = s.split_once(':') {
        if scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
    }
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    let (p, d) = (norm(&s), norm(&dir.to_string_lossy()));
    let d = d.trim_end_matches('/');
    let same = |a: &str, b: &str| {
        if cfg!(windows) {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    p.len() > d.len() + 1
        && p.is_char_boundary(d.len())
        && same(&p[..d.len()], d)
        && p.as_bytes()[d.len()] == b'/'
}

/// One provider as the window shows it: never the key, only whether one exists.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderView {
    pub name: String,
    pub kind: String,
    pub protocol: String,
    pub base_url: String,
    pub default_model: Option<String>,
    pub known_models: Vec<String>,
    pub context_limit: Option<u32>,
    pub timeout_ms: u64,
    pub account_id: Option<String>,
    pub has_key: bool,
    pub auth: String,
}

impl ProviderView {
    pub fn of(cfg: &ProviderConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            kind: cfg.kind.as_str().to_string(),
            protocol: cfg.protocol.as_str().to_string(),
            base_url: cfg.base_url.clone(),
            default_model: cfg.default_model.clone(),
            known_models: cfg.known_models.clone(),
            context_limit: cfg.context_limit,
            timeout_ms: cfg.timeout_ms,
            account_id: cfg.account_id.clone(),
            has_key: cfg.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()),
            auth: match cfg.auth {
                zerith_providers::AuthMode::ApiKey => "apikey",
                zerith_providers::AuthMode::Oauth => "oauth",
            }
            .to_string(),
        }
    }
}

/// A message as the transcript draws it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Row id: what `turn_changes` takes for an answer's diff.
    #[serde(default)]
    pub id: i64,
    pub role: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_name: Option<String>,
    /// As stored: `[{id, name, arguments}]` on a step that called tools,
    /// `{tool_call_id}` on a tool result. The transcript pairs them.
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
}

/// A question the model asked, ready for the card that draws it.
#[derive(Clone, Debug, Serialize)]
pub struct QuestionView {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOptionView>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionOptionView {
    pub label: String,
    pub description: String,
}

impl QuestionView {
    pub fn of(q: &zerith_tools::Question) -> Self {
        Self {
            question: q.question.clone(),
            header: q.header.clone(),
            options: q
                .options
                .iter()
                .map(|o| QuestionOptionView {
                    label: o.label.clone(),
                    description: o.description.clone(),
                })
                .collect(),
            multi_select: q.multi_select,
        }
    }
}

/// Path the media screen writes generated files to.
pub type MediaDir = PathBuf;

/// Empty parameter map, the shape the media screen saves.
pub type Params = BTreeMap<String, serde_json::Value>;
