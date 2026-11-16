//! Provider configuration. Text-friendly on purpose: the same struct is what
//! the CLI prints and what a future `config.toml` round-trips.

use serde::{Deserialize, Serialize};

/// Which wire protocol an endpoint speaks. Kept explicit so an
/// OpenAI-compatible check never silently accepts an Anthropic endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `POST {base}/chat/completions`, `Authorization: Bearer`. Covers OpenAI,
    /// 9Router, DeepSeek, OpenRouter, Ollama, LM Studio and every other
    /// OpenAI-compatible server.
    OpenAi,
    /// `POST {base}/messages`, `x-api-key` + `anthropic-version`.
    Anthropic,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::OpenAi => "openai",
            Protocol::Anthropic => "anthropic",
        }
    }
}

/// A well-known provider, used only to prefill sane defaults. Any endpoint can
/// be added as `Custom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    NineRouter,
    OpenAi,
    Anthropic,
    DeepSeek,
    Gemini,
    OpenRouter,
    Ollama,
    LmStudio,
    Custom,
}

impl ProviderKind {
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::NineRouter => "http://localhost:20128/v1",
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::DeepSeek => "https://api.deepseek.com/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            ProviderKind::Ollama => "http://localhost:11434/v1",
            ProviderKind::LmStudio => "http://localhost:1234/v1",
            ProviderKind::Custom => "",
        }
    }

    pub fn default_protocol(self) -> Protocol {
        match self {
            ProviderKind::Anthropic => Protocol::Anthropic,
            _ => Protocol::OpenAi,
        }
    }

    /// Whether the provider usually needs a key. Local servers do not, and
    /// requiring one would block the most privacy-preserving setup.
    pub fn needs_key(self) -> bool {
        !matches!(self, ProviderKind::Ollama | ProviderKind::LmStudio)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::NineRouter => "9router",
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::DeepSeek => "deepseek",
            ProviderKind::Gemini => "gemini",
            ProviderKind::OpenRouter => "openrouter",
            ProviderKind::Ollama => "ollama",
            ProviderKind::LmStudio => "lmstudio",
            ProviderKind::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub protocol: Protocol,
    pub base_url: String,
    /// Never logged, never serialized into an error, never printed by `show`.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    pub known_models: Vec<String>,
    pub context_limit: Option<u32>,
    pub timeout_ms: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: "9router".into(),
            kind: ProviderKind::NineRouter,
            protocol: Protocol::OpenAi,
            base_url: ProviderKind::NineRouter.default_base_url().into(),
            api_key: None,
            default_model: None,
            known_models: Vec::new(),
            context_limit: None,
            timeout_ms: 60_000,
        }
    }
}

impl ProviderConfig {
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            name: kind.as_str().to_string(),
            kind,
            protocol: kind.default_protocol(),
            base_url: kind.default_base_url().to_string(),
            ..Default::default()
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    /// What is safe to show: the URL, never the key.
    pub fn redacted(&self) -> String {
        format!(
            "{} ({}) {} — key {}",
            self.name,
            self.kind.as_str(),
            self.base_url,
            if self.api_key.is_some() {
                "set"
            } else {
                "missing"
            }
        )
    }

    /// The provider this process was told to use, from the environment.
    ///
    /// One reader for CLI, window and any future host: two copies of this
    /// resolution would drift, and the user would get a different model
    /// depending on which binary they launched.
    ///
    /// `ZERITH_PROVIDER` picks the kind, `ZERITH_BASE_URL` / `ZERITH_API_KEY` /
    /// `ZERITH_MODEL` override its defaults. Unknown values fall back to the
    /// 9Router, which is the local-first default.
    pub fn from_env() -> Self {
        let kind = match std::env::var("ZERITH_PROVIDER").as_deref() {
            Ok("openai") => ProviderKind::OpenAi,
            Ok("anthropic") => ProviderKind::Anthropic,
            Ok("deepseek") => ProviderKind::DeepSeek,
            Ok("gemini") => ProviderKind::Gemini,
            Ok("openrouter") => ProviderKind::OpenRouter,
            Ok("ollama") => ProviderKind::Ollama,
            Ok("lmstudio") => ProviderKind::LmStudio,
            Ok("custom") => ProviderKind::Custom,
            _ => ProviderKind::NineRouter,
        };
        let mut cfg = Self::new(kind);
        if let Ok(url) = std::env::var("ZERITH_BASE_URL") {
            if !url.trim().is_empty() {
                cfg.base_url = url;
            }
        }
        if let Ok(key) = std::env::var("ZERITH_API_KEY") {
            if !key.trim().is_empty() {
                cfg.api_key = Some(key);
            }
        }
        if let Ok(model) = std::env::var("ZERITH_MODEL") {
            if !model.trim().is_empty() {
                cfg.default_model = Some(model);
            }
        }
        cfg
    }

    pub fn validate(&self) -> crate::Result<()> {
        if self.base_url.trim().is_empty() {
            return Err(crate::ProviderError::NoEndpoint);
        }
        if self.kind.needs_key() && self.api_key.as_deref().unwrap_or("").is_empty() {
            return Err(crate::ProviderError::NoKey);
        }
        Ok(())
    }
}
