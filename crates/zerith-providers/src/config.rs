//! Provider configuration. Text-friendly on purpose: the same struct is what
//! the CLI prints and what a future `config.toml` round-trips.

use serde::{Deserialize, Serialize};

/// Which wire protocol an endpoint speaks. Kept explicit so an
/// OpenAI-compatible check never silently accepts an Anthropic endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// `POST {base}/chat/completions`, `Authorization: Bearer`. Covers OpenAI,
    /// DeepSeek, Gemini, OpenRouter, Ollama, LM Studio and every other
    /// OpenAI-compatible server.
    OpenAi,
    /// `POST {base}/messages`, `x-api-key` + `anthropic-version`.
    Anthropic,
    /// `POST {base}/responses`: the Responses API a Codex subscription answers
    /// on. Flat `input`, typed SSE.
    Responses,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::OpenAi => "openai",
            Protocol::Anthropic => "anthropic",
            Protocol::Responses => "responses",
        }
    }
}

/// How a request proves who it is. `ApiKey` is the historical path; `Oauth`
/// swaps the credential header (and, for Anthropic, adds the beta header the
/// subscription endpoint requires).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    #[default]
    ApiKey,
    Oauth,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::ApiKey => "api-key",
            AuthMode::Oauth => "oauth",
        }
    }
}

/// Which CLI a credential belongs to. Endpoints and client ids live in
/// `oauth::provider`, keyed by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthTool {
    ClaudeCode,
    Codex,
    Antigravity,
}

impl OAuthTool {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "anthropic" => Some(Self::ClaudeCode),
            "codex" | "openai" => Some(Self::Codex),
            "antigravity" | "google" => Some(Self::Antigravity),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }
}

/// What the cofre stores. `Debug` is hand-written: a derived one would print the
/// tokens into any `{:?}` of a config or an error.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds, from the server's `expires_in`.
    pub expires_at: i64,
    /// `chatgpt-account-id` (Codex) or the Google project (Antigravity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub tool: OAuthTool,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("access_token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("tool", &self.tool)
            .finish()
    }
}

impl OAuthCredential {
    /// True when the token is worth sending. The skew keeps a token from
    /// expiring mid-stream; it never loops, because `expires_in` below the skew
    /// still yields exactly one refresh per call.
    pub fn needs_refresh(&self, now: i64) -> bool {
        const SKEW: i64 = 300;
        self.expires_at - now < SKEW
    }
}

/// A well-known provider, used only to prefill sane defaults. Any endpoint can
/// be added as `Custom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    DeepSeek,
    Gemini,
    OpenRouter,
    Ollama,
    LmStudio,
    /// A Codex subscription: OAuth only, own base URL and protocol.
    Codex,
    Custom,
}

impl ProviderKind {
    pub const PRESETS: &[ProviderKind] = &[
        ProviderKind::Anthropic,
        ProviderKind::DeepSeek,
        ProviderKind::OpenAi,
        ProviderKind::Gemini,
        ProviderKind::OpenRouter,
        ProviderKind::Ollama,
        ProviderKind::LmStudio,
        ProviderKind::Codex,
    ];

    pub fn all_presets() -> &'static [ProviderKind] {
        Self::PRESETS
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "anthropic" | "claude" => Self::Anthropic,
            "deepseek" => Self::DeepSeek,
            "gemini" | "google" => Self::Gemini,
            "openrouter" => Self::OpenRouter,
            "ollama" => Self::Ollama,
            "lmstudio" | "lm-studio" => Self::LmStudio,
            "codex" => Self::Codex,
            _ => Self::Custom,
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::DeepSeek => "https://api.deepseek.com/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            ProviderKind::Ollama => "http://localhost:11434/v1",
            ProviderKind::LmStudio => "http://localhost:1234/v1",
            ProviderKind::Codex => "https://chatgpt.com/backend-api/codex",
            ProviderKind::Custom => "",
        }
    }

    pub fn default_protocol(self) -> Protocol {
        match self {
            ProviderKind::Anthropic => Protocol::Anthropic,
            ProviderKind::Codex => Protocol::Responses,
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
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::DeepSeek => "deepseek",
            ProviderKind::Gemini => "gemini",
            ProviderKind::OpenRouter => "openrouter",
            ProviderKind::Ollama => "ollama",
            ProviderKind::LmStudio => "lmstudio",
            ProviderKind::Codex => "codex",
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
    /// `ApiKey` (default) or `Oauth`. With `Oauth`, `api_key` stays empty and
    /// `oauth` carries the credential.
    #[serde(default)]
    pub auth: AuthMode,
    /// Never serialized: a `Debug` of the config must not print tokens.
    #[serde(skip_serializing)]
    pub oauth: Option<OAuthCredential>,
    pub default_model: Option<String>,
    pub known_models: Vec<String>,
    pub context_limit: Option<u32>,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: "openai".into(),
            kind: ProviderKind::OpenAi,
            protocol: Protocol::OpenAi,
            base_url: ProviderKind::OpenAi.default_base_url().into(),
            api_key: None,
            auth: AuthMode::ApiKey,
            oauth: None,
            default_model: None,
            known_models: Vec::new(),
            context_limit: None,
            timeout_ms: 60_000,
            account_id: None,
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

    pub fn with_account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
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
    /// `ZERITH_MODEL` / `ZERITH_ACCOUNT_ID` override its defaults. Absent or
    /// empty `ZERITH_PROVIDER` defaults to `OpenAi`, and unknown provider names
    /// resolve to `Custom`.
    pub fn from_env() -> Self {
        let kind = match std::env::var("ZERITH_PROVIDER") {
            Ok(s) if !s.trim().is_empty() => ProviderKind::parse(s.trim()),
            _ => ProviderKind::OpenAi,
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
        if let Ok(account_id) = std::env::var("ZERITH_ACCOUNT_ID") {
            let trimmed = account_id.trim();
            if !trimmed.is_empty() {
                cfg.account_id = Some(trimmed.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn provider_kind_parse_and_presets() {
        assert_eq!(ProviderKind::parse("openai"), ProviderKind::OpenAi);
        assert_eq!(ProviderKind::parse("anthropic"), ProviderKind::Anthropic);
        assert_eq!(ProviderKind::parse("claude"), ProviderKind::Anthropic);
        assert_eq!(ProviderKind::parse("Claude"), ProviderKind::Anthropic);
        assert_eq!(ProviderKind::parse("deepseek"), ProviderKind::DeepSeek);
        assert_eq!(ProviderKind::parse("gemini"), ProviderKind::Gemini);
        assert_eq!(ProviderKind::parse("google"), ProviderKind::Gemini);
        assert_eq!(ProviderKind::parse("openrouter"), ProviderKind::OpenRouter);
        assert_eq!(ProviderKind::parse("ollama"), ProviderKind::Ollama);
        assert_eq!(ProviderKind::parse("lmstudio"), ProviderKind::LmStudio);
        assert_eq!(ProviderKind::parse("lm-studio"), ProviderKind::LmStudio);
        assert_eq!(ProviderKind::parse("codex"), ProviderKind::Codex);
        assert_eq!(ProviderKind::parse("my-proxy"), ProviderKind::Custom);

        assert_eq!(ProviderKind::PRESETS, ProviderKind::all_presets());
        assert_eq!(ProviderKind::PRESETS.len(), 8);
        assert!(!ProviderKind::PRESETS.contains(&ProviderKind::Custom));
    }

    #[test]
    fn default_config_uses_openai() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.kind, ProviderKind::OpenAi);
        assert_eq!(cfg.name, "openai");
        assert_eq!(cfg.protocol, Protocol::OpenAi);
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.account_id, None);
    }

    #[test]
    fn from_env_defaults_to_openai_and_reads_account_id() {
        let _guard = ENV_LOCK.lock().unwrap();
        let keys = [
            "ZERITH_PROVIDER",
            "ZERITH_BASE_URL",
            "ZERITH_API_KEY",
            "ZERITH_MODEL",
            "ZERITH_ACCOUNT_ID",
        ];
        let saved: Vec<(&str, Option<String>)> =
            keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        for k in keys {
            std::env::remove_var(k);
        }

        let def = ProviderConfig::from_env();
        assert_eq!(def.kind, ProviderKind::OpenAi);
        assert_eq!(def.account_id, None);

        std::env::set_var("ZERITH_PROVIDER", "claude");
        std::env::set_var("ZERITH_ACCOUNT_ID", "acc-work");
        let parsed = ProviderConfig::from_env();
        assert_eq!(parsed.kind, ProviderKind::Anthropic);
        assert_eq!(parsed.protocol, Protocol::Anthropic);
        assert_eq!(parsed.account_id.as_deref(), Some("acc-work"));

        std::env::set_var("ZERITH_PROVIDER", "custom-local");
        let custom = ProviderConfig::from_env();
        assert_eq!(custom.kind, ProviderKind::Custom);

        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}
