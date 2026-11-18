//! Provedores, contas, delegação e uso: the four jobs the endpoint screen has.
//!
//! Each command validates the form the window sent, then hands the work to
//! `wire`, which is the only place that touches settings and the keyring.

use serde::{Deserialize, Serialize};
use zerith_core::settings::{AccountEntry, RoutingSettings};
use zerith_providers::{Client, Protocol, ProviderConfig, ProviderKind};
use zerith_sessions::UsageSummary;

use crate::state::AppState;
use crate::types::ProviderView;
use crate::wire;

type Cmd<T> = Result<T, String>;

/// One preset: the defaults that save typing, nothing that changes behavior.
#[derive(Serialize)]
pub struct Preset {
    pub kind: String,
    pub label: String,
    pub base_url: String,
    pub protocol: String,
    pub model_hint: String,
    pub needs_key: bool,
}

fn model_hint(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "ex.: claude-sonnet-4",
        ProviderKind::DeepSeek => "ex.: deepseek-chat",
        ProviderKind::OpenAi => "ex.: gpt-4o",
        ProviderKind::Gemini => "ex.: gemini-2.5-flash",
        ProviderKind::OpenRouter => "ex.: anthropic/claude-sonnet-4",
        ProviderKind::Ollama => "ex.: qwen2.5-coder",
        ProviderKind::LmStudio => "ex.: local-model",
        ProviderKind::Codex => "ex.: gpt-5-codex",
        ProviderKind::Antigravity => "ex.: gemini-4-hydrogen-medium",
        ProviderKind::Custom => "ex.: modelo-padrão",
    }
}

#[tauri::command]
pub async fn presets() -> Cmd<Vec<Preset>> {
    Ok(ProviderKind::all_presets()
        .iter()
        .map(|k| Preset {
            kind: k.as_str().to_string(),
            label: match k {
                ProviderKind::OpenAi => "OpenAI",
                ProviderKind::Anthropic => "Anthropic",
                ProviderKind::DeepSeek => "DeepSeek",
                ProviderKind::Gemini => "Gemini",
                ProviderKind::OpenRouter => "OpenRouter",
                ProviderKind::Ollama => "Ollama",
                ProviderKind::LmStudio => "LM Studio",
                ProviderKind::Codex => "Codex",
                ProviderKind::Antigravity => "Antigravity",
                ProviderKind::Custom => "Outro",
            }
            .to_string(),
            base_url: k.default_base_url().to_string(),
            protocol: k.default_protocol().as_str().to_string(),
            model_hint: model_hint(*k).to_string(),
            needs_key: k.needs_key(),
        })
        .collect())
}

/// What the form holds. The key is only present when the user typed a new one.
#[derive(Deserialize)]
pub struct ProviderForm {
    pub name: String,
    pub base_url: String,
    pub protocol: String,
    pub model: Option<String>,
    pub timeout_ms: u64,
    pub key: Option<String>,
}

impl ProviderForm {
    /// Reads the form back into a config, keeping what the saved endpoint knew
    /// (its model cache and context limit) so a rename does not lose it.
    fn config(&self, root: &std::path::Path) -> Cmd<ProviderConfig> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Dê um nome ao provedor".into());
        }
        let url = self.base_url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("URL precisa começar com http:// ou https://".into());
        }
        if !(1_000..=600_000).contains(&self.timeout_ms) {
            return Err("Timeout entre 1000 e 600000 ms".into());
        }
        let mut cfg = wire::configured_providers(root)
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| ProviderConfig::new(ProviderKind::parse(name)));
        cfg.name = name.to_string();
        cfg.kind = ProviderKind::parse(name);
        cfg.protocol = match self.protocol.as_str() {
            "anthropic" => Protocol::Anthropic,
            "responses" => Protocol::Responses,
            "gemini" => Protocol::Gemini,
            _ => Protocol::OpenAi,
        };
        cfg.base_url = url.to_string();
        cfg.default_model = self
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        cfg.timeout_ms = self.timeout_ms;
        cfg.api_key = self
            .key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string);
        Ok(cfg)
    }
}

#[tauri::command]
pub async fn provider_save(
    state: tauri::State<'_, AppState>,
    form: ProviderForm,
) -> Cmd<ProviderView> {
    let root = state.root().to_path_buf();
    let cfg = form.config(&root)?;
    wire::save_provider(&root, &cfg)?;
    if let Some(key) = cfg.api_key.as_deref() {
        wire::store_key(&cfg.name, key)?;
    }
    Ok(ProviderView::of(&cfg))
}

/// A test connection. It leaves the caller's thread: the endpoint may hang.
#[tauri::command]
pub async fn provider_test(
    state: tauri::State<'_, AppState>,
    form: ProviderForm,
) -> Cmd<ProbeView> {
    let root = state.root().to_path_buf();
    let cfg = form.config(&root)?;
    tauri::async_runtime::spawn_blocking(move || {
        Client::new(cfg)
            .map(|c| ProbeView::of(&c.probe()))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct ProbeView {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub models: Vec<String>,
    pub detail: String,
}

impl ProbeView {
    fn of(p: &zerith_providers::Probe) -> Self {
        Self {
            ok: p.ok,
            status: p.status,
            latency_ms: p.latency_ms,
            models: p.models.clone(),
            detail: p.detail.chars().take(240).collect(),
        }
    }
}

/// One account as the window edits it. `key` is empty unless a new one is typed.
#[derive(Deserialize)]
pub struct AccountForm {
    pub id: String,
    pub provider: String,
    pub priority: i32,
    pub quota_tokens: Option<u64>,
    pub window_hours: u32,
    pub default_model: Option<String>,
    pub enabled: bool,
    pub key: Option<String>,
    /// Catalogue connect: name shown on the card, endpoint and wire protocol
    /// for providers the core has no preset for. Absent = keep the saved one.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
}

fn trimmed(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[tauri::command]
pub async fn accounts(state: tauri::State<'_, AppState>) -> Cmd<Vec<AccountEntry>> {
    let root = state.root().to_path_buf();
    Ok(wire::configured_accounts(&root))
}

#[tauri::command]
pub async fn account_save(
    state: tauri::State<'_, AppState>,
    form: AccountForm,
) -> Cmd<Vec<AccountEntry>> {
    let root = state.root().to_path_buf();
    let id = form.id.trim().to_string();
    if id.is_empty() {
        return Err("Informe o ID da conta".into());
    }
    if form.provider.trim().is_empty() {
        return Err("Informe o provedor da conta".into());
    }
    if form.window_hours == 0 {
        return Err("Janela precisa ter pelo menos 1 hora".into());
    }
    let previous = wire::configured_accounts(&root)
        .into_iter()
        .find(|a| a.id == id)
        .unwrap_or_default();
    let entry = AccountEntry {
        id: id.clone(),
        provider: form.provider.trim().to_string(),
        priority: form.priority,
        quota_tokens: form.quota_tokens.filter(|q| *q > 0),
        window_hours: form.window_hours,
        default_model: form
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string),
        enabled: form.enabled,
        api_key_ref: previous.api_key_ref.clone().or(Some(id.clone())),
        label: trimmed(&form.label).or(previous.label.clone()),
        base_url: match trimmed(&form.base_url) {
            Some(u) if !(u.starts_with("http://") || u.starts_with("https://")) => {
                return Err("URL precisa começar com http:// ou https://".into())
            }
            Some(u) => Some(u),
            None => previous.base_url.clone(),
        },
        protocol: match form.protocol.as_deref().map(str::trim) {
            Some("anthropic") => Some(zerith_core::settings::WireProtocol::Anthropic),
            Some("openai") => Some(zerith_core::settings::WireProtocol::OpenAi),
            _ => previous.protocol,
        },
        ..previous
    };
    wire::save_account(&root, &entry, form.key.as_deref())?;
    Ok(wire::configured_accounts(&root))
}

/// Tries an account before (or after) it is saved: same endpoint, protocol
/// and key the router would use. A key typed in the form wins; otherwise the
/// one already in the cofre. OAuth accounts are checked by their login instead.
#[tauri::command]
pub async fn account_test(state: tauri::State<'_, AppState>, form: AccountForm) -> Cmd<ProbeView> {
    let root = state.root().to_path_buf();
    let provider = form.provider.trim().to_string();
    if provider.is_empty() {
        return Err("Informe o provedor da conta".into());
    }
    let mut cfg = ProviderConfig::new(ProviderKind::parse(&provider));
    cfg.name = provider;
    if let Some(u) = trimmed(&form.base_url) {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err("URL precisa começar com http:// ou https://".into());
        }
        cfg.base_url = u;
    }
    match form.protocol.as_deref().map(str::trim) {
        Some("anthropic") => cfg.protocol = Protocol::Anthropic,
        Some("openai") => cfg.protocol = Protocol::OpenAi,
        _ => {}
    }
    cfg.api_key = trimmed(&form.key).or_else(|| {
        let saved = wire::configured_accounts(&root)
            .into_iter()
            .find(|a| a.id == form.id.trim())?;
        let key_ref = saved.api_key_ref.unwrap_or(saved.id);
        zerith_core::secrets::get_secret(&key_ref)
            .ok()
            .filter(|k| !k.trim().is_empty())
    });
    tauri::async_runtime::spawn_blocking(move || {
        Client::new(cfg)
            .map(|c| ProbeView::of(&c.probe()))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn account_delete(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Cmd<Vec<AccountEntry>> {
    let root = state.root().to_path_buf();
    wire::delete_account(&root, &id)?;
    Ok(wire::configured_accounts(&root))
}

/// Browser login: blocking by design, so it runs off the async runtime.
/// Uses the credential the CLI already has, skipping the browser.
#[tauri::command]
pub async fn oauth_import(
    state: tauri::State<'_, AppState>,
    tool: String,
    account: String,
) -> Cmd<String> {
    let root = state.root().to_path_buf();
    let tool =
        zerith_providers::OAuthTool::parse(&tool).ok_or("Ferramenta de OAuth desconhecida")?;
    tauri::async_runtime::spawn_blocking(move || wire::oauth_import(&root, tool, &account))
        .await
        .map_err(|e| e.to_string())?
}

/// Copies the connection 9router already has for this provider.
#[tauri::command]
pub async fn oauth_import_9router(
    state: tauri::State<'_, AppState>,
    tool: String,
    account: String,
) -> Cmd<String> {
    let root = state.root().to_path_buf();
    let tool =
        zerith_providers::OAuthTool::parse(&tool).ok_or("Ferramenta de OAuth desconhecida")?;
    tauri::async_runtime::spawn_blocking(move || wire::oauth_import_9router(&root, tool, &account))
        .await
        .map_err(|e| e.to_string())?
}

/// Plan quota (5 h / weekly windows) for an OAuth account.
#[tauri::command]
pub async fn account_quota(account: String) -> Cmd<zerith_providers::oauth::Quota> {
    tauri::async_runtime::spawn_blocking(move || wire::account_quota(&account))
        .await
        .map_err(|e| e.to_string())?
}

/// Models an OAuth account offers right now (empty when the provider has no list).
#[tauri::command]
pub async fn account_models(account: String) -> Cmd<Vec<zerith_providers::oauth::ModelInfo>> {
    tauri::async_runtime::spawn_blocking(move || wire::account_models(&account))
        .await
        .map_err(|e| e.to_string())?
}

/// Imports a pasted `.credentials.json` into a new account.
#[tauri::command]
pub async fn oauth_import_pasted(
    state: tauri::State<'_, AppState>,
    tool: String,
    account: String,
    raw: String,
) -> Cmd<String> {
    let root = state.root().to_path_buf();
    let tool =
        zerith_providers::OAuthTool::parse(&tool).ok_or("Ferramenta de OAuth desconhecida")?;
    tauri::async_runtime::spawn_blocking(move || {
        wire::oauth_import_pasted(&root, tool, &account, &raw)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delivers the code the user pasted after consenting in the browser.
#[tauri::command]
pub fn oauth_code(code: String) -> Cmd<bool> {
    Ok(wire::submit_oauth_code(&code))
}

#[tauri::command]
pub async fn oauth_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    tool: String,
    account: String,
) -> Cmd<String> {
    use tauri::Emitter;
    let root = state.root().to_path_buf();
    let tool =
        zerith_providers::OAuthTool::parse(&tool).ok_or("Ferramenta de OAuth desconhecida")?;
    tauri::async_runtime::spawn_blocking(move || {
        wire::oauth_login(&root, tool, &account, &|url| {
            let _ = app.emit("oauth:url", url);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn oauth_status(account: String) -> Cmd<Option<OauthView>> {
    Ok(
        wire::oauth_status(&account).map(|(tool, minutes)| OauthView {
            tool: tool.as_str().to_string(),
            minutes,
        }),
    )
}

#[derive(Serialize)]
pub struct OauthView {
    pub tool: String,
    pub minutes: i64,
}

#[tauri::command]
pub async fn routing(state: tauri::State<'_, AppState>) -> Cmd<RoutingSettings> {
    let root = state.root().to_path_buf();
    Ok(wire::load_routing(&root))
}

#[tauri::command]
pub async fn routing_save(
    state: tauri::State<'_, AppState>,
    routing: RoutingSettings,
) -> Cmd<RoutingSettings> {
    let root = state.root().to_path_buf();
    wire::save_routing(&root, &routing)?;
    Ok(wire::load_routing(&root))
}

#[tauri::command]
pub async fn usage(state: tauri::State<'_, AppState>, hours: Option<u32>) -> Cmd<UsageSummary> {
    let db = state.db();
    wire::usage_summary(&db, hours.unwrap_or(5))
}

#[tauri::command]
pub async fn clear_cooldown(state: tauri::State<'_, AppState>, account_id: String) -> Cmd<()> {
    let db = state.db();
    wire::clear_account_cooldown(&db, &account_id)
}
