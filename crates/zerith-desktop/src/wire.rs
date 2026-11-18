//! The one place the shell touches other crates' persistence: settings,
//! secrets, memory pause, session search/archive/export, approvals, agents.
//! Every function takes the data root explicitly (`<root>/zerith.db` lives
//! beside `<root>/settings.toml`), so tests and snapshots stay isolated.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use zerith_core::settings::{self, AuthMode as AuthModeSettings, ProviderEntry, WireProtocol};
use zerith_core::{Paths, Settings};
use zerith_memory::Scope;
use zerith_providers::{
    infer_provider, AuthMode, ModelSpec, OAuthTool, Protocol, ProviderConfig, ProviderKind,
};
use zerith_runtime::{Approval as RtApproval, ApprovalSender};
use zerith_sessions::{Store, UsageSummary};

use crate::types::{Approval, Decision};
use crate::types::{Profile, Theme};

/// The memory scope of this install; the same one the CLI uses.
pub fn memory_scope() -> Scope {
    zerith_memory::default_scope()
}

fn load(root: &Path) -> Settings {
    let cwd = std::env::current_dir().ok();
    Settings::load(&Paths::with_root(root), cwd.as_deref()).settings
}

fn kind_of(name: &str) -> ProviderKind {
    ProviderKind::parse(name)
}

fn wire_to_protocol(p: WireProtocol) -> Protocol {
    match p {
        WireProtocol::OpenAi => Protocol::OpenAi,
        WireProtocol::Anthropic => Protocol::Anthropic,
    }
}

fn canonical_provider(raw: &str) -> String {
    let kind = ProviderKind::parse(raw);
    if kind == ProviderKind::Custom {
        raw.trim().to_string()
    } else {
        kind.as_str().to_string()
    }
}

fn dedup_accounts(accounts: &[settings::AccountEntry]) -> Vec<settings::AccountEntry> {
    let mut seen = HashSet::new();
    let mut rev = Vec::new();
    for acc in accounts.iter().rev() {
        if !acc.id.is_empty() && seen.insert(acc.id.clone()) {
            rev.push(acc.clone());
        }
    }
    rev.reverse();
    rev
}

fn apply_account_defaults(
    c: &mut ProviderConfig,
    acc: &settings::AccountEntry,
    allow_url_override: bool,
) {
    if let Some(p) = acc.protocol {
        c.protocol = wire_to_protocol(p);
    }
    if allow_url_override {
        if let Some(u) = acc
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            c.base_url = u.to_string();
        }
    }
    if c.default_model.is_none() {
        c.default_model = acc
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    if c.account_id.is_none() {
        c.account_id = Some(acc.id.clone());
    }
}

/// Providers: the environment's one first (what the CLI uses), then every
/// `[[providers]]` entry from the settings layers and providers referenced by
/// `[[accounts]]` / `[routing] main_model`. When `ZERITH_PROVIDER` is unset
/// and `routing.main_model` or `accounts` names a primary provider, that
/// provider is promoted first so the shell targets it by default. Keys come from
/// `ZERITH_API_KEY` or the OS keyring, never from a file.
pub fn configured_providers(root: &Path) -> Vec<ProviderConfig> {
    let st = load(root);
    let env_provider_set = std::env::var("ZERITH_PROVIDER").is_ok_and(|v| !v.trim().is_empty());
    let env_url_set = std::env::var("ZERITH_BASE_URL").is_ok_and(|v| !v.trim().is_empty());
    let mut env = ProviderConfig::from_env();
    if env.default_model.is_none() {
        env.default_model = st.model.clone();
    }
    let env_name = env.name.clone();
    let mut out = vec![env];
    for e in &st.providers {
        let mut c = ProviderConfig::new(kind_of(&e.name));
        c.name = e.name.clone();
        if let Some(u) = &e.base_url {
            c.base_url = u.clone();
        }
        if let Some(p) = e.protocol {
            c.protocol = wire_to_protocol(p);
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

    let accounts = dedup_accounts(&st.accounts);
    for acc in &accounts {
        let prov_name = canonical_provider(&acc.provider);
        if prov_name.is_empty() {
            continue;
        }
        if let Some(slot) = out
            .iter_mut()
            .find(|o| o.name.eq_ignore_ascii_case(&prov_name))
        {
            if st.provider_entry(&slot.name).is_none() {
                let is_env_slot = slot.name == env_name && (env_provider_set || env_url_set);
                apply_account_defaults(slot, acc, !is_env_slot);
            } else if slot.default_model.is_none() {
                slot.default_model = acc.default_model.clone();
            }
        } else {
            let mut c = ProviderConfig::new(kind_of(&prov_name));
            c.name = prov_name;
            apply_account_defaults(&mut c, acc, true);
            out.push(c);
        }
    }

    let account_ids: Vec<&str> = accounts.iter().map(|a| a.id.as_str()).collect();
    let main_from_routing: Option<(String, Option<String>)> = st
        .routing
        .main_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|raw| {
            let spec = ModelSpec::parse_with_accounts(raw, &account_ids);
            let model_opt = (!spec.model.trim().is_empty()).then(|| spec.model.trim().to_string());
            if let Some(aid) = &spec.account_id {
                accounts
                    .iter()
                    .find(|a| a.id == *aid)
                    .map(|acc| (canonical_provider(&acc.provider), model_opt.clone()))
            } else if let Some(p) = &spec.provider {
                Some((canonical_provider(p), model_opt))
            } else {
                infer_provider(&spec.model).map(|k| (k.as_str().to_string(), model_opt))
            }
        });

    let primary = main_from_routing.or_else(|| {
        accounts
            .iter()
            .filter(|a| a.enabled)
            .min_by_key(|a| a.priority)
            .or_else(|| accounts.first())
            .map(|acc| (canonical_provider(&acc.provider), acc.default_model.clone()))
            .filter(|(p, _)| !p.is_empty())
    });

    if let Some((primary_name, primary_model)) = primary {
        let pos = if let Some(idx) = out
            .iter()
            .position(|o| o.name.eq_ignore_ascii_case(&primary_name))
        {
            if out[idx].default_model.is_none() {
                out[idx].default_model = primary_model;
            }
            idx
        } else {
            let mut c = ProviderConfig::new(kind_of(&primary_name));
            c.name = primary_name;
            c.default_model = primary_model;
            out.push(c);
            out.len() - 1
        };
        if !env_provider_set && pos != 0 {
            let p = out.remove(pos);
            out.insert(0, p);
        }
    }

    for c in &mut out {
        if c.api_key.is_none() {
            let key_ref = st
                .provider_entry(&c.name)
                .and_then(|p| p.api_key_ref.clone())
                .or_else(|| {
                    accounts
                        .iter()
                        .filter(|a| a.enabled && canonical_provider(&a.provider) == c.name)
                        .min_by_key(|a| a.priority)
                        .or_else(|| {
                            accounts
                                .iter()
                                .find(|a| canonical_provider(&a.provider) == c.name)
                        })
                        .map(|a| a.api_key_ref.clone().unwrap_or_else(|| a.id.clone()))
                });
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
        protocol: match cfg.protocol {
            Protocol::OpenAi => Some(WireProtocol::OpenAi),
            Protocol::Anthropic => Some(WireProtocol::Anthropic),
            // Responses (codex) and Gemini (antigravity) are implied by the
            // provider kind; the settings field only knows the two historical
            // protocols.
            Protocol::Responses | Protocol::Gemini => None,
        },
        auth: match cfg.auth {
            AuthMode::ApiKey => AuthModeSettings::ApiKey,
            AuthMode::Oauth => AuthModeSettings::Oauth,
        },
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

/// Configured gateway accounts across settings layers (last entry wins per id).
pub fn configured_accounts(root: &Path) -> Vec<settings::AccountEntry> {
    dedup_accounts(&load(root).accounts)
}

/// Persists an account entry in `settings.toml` (`[[accounts]]`) and stores a
/// non-empty API key in the OS keyring under `api_key_ref` (or `acc.id`).
pub fn save_account(
    root: &Path,
    acc: &settings::AccountEntry,
    api_key: Option<&str>,
) -> Result<(), String> {
    if acc.id.trim().is_empty() {
        return Err("Informe o ID da conta".into());
    }
    let entry = acc.clone();
    Settings::save_user(&Paths::with_root(root), |s| {
        match s.accounts.iter_mut().find(|a| a.id == entry.id) {
            Some(slot) => *slot = entry,
            None => s.accounts.push(entry),
        }
    })
    .map_err(|e| e.to_string())?;
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        let key_ref = acc
            .api_key_ref
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or(&acc.id);
        zerith_core::secrets::set_secret(key_ref, key).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Removes a gateway account by id from the user settings layer.
/// Provider an OAuth tool speaks: Codex has its own preset, Claude Code rides
/// the Anthropic one, Antigravity the Google one.
pub fn oauth_provider(tool: OAuthTool) -> &'static str {
    match tool {
        OAuthTool::ClaudeCode => "anthropic",
        OAuthTool::Codex => "codex",
        OAuthTool::Antigravity => "antigravity",
    }
}

/// Full browser login: PKCE, loopback, exchange, cofre, settings.
///
/// Blocking by design — it waits for the human in the browser, so the caller
/// runs it on the worker thread, never on the UI thread.
/// Where a pasted login code is delivered. One login runs at a time, so a
/// single slot is enough.
static OAUTH_CODE: std::sync::Mutex<Option<std::sync::mpsc::Sender<String>>> =
    std::sync::Mutex::new(None);

/// Hands the code the user pasted to the login that is waiting for it.
pub fn submit_oauth_code(code: &str) -> bool {
    let tx = OAUTH_CODE.lock().ok().and_then(|g| g.clone());
    match tx {
        Some(tx) => tx.send(code.to_string()).is_ok(),
        None => false,
    }
}

pub fn oauth_login(
    root: &Path,
    tool: OAuthTool,
    account: &str,
    on_url: &dyn Fn(&str),
) -> Result<String, String> {
    use zerith_providers::oauth;

    let account = account.trim();
    if account.is_empty() {
        return Err("Informe o ID da conta antes de entrar".into());
    }
    oauth::ready(tool)?;
    let pkce = oauth::Pkce::generate();
    let state = oauth::random_state();
    // Providers with their own hosted callback (Claude) end with a pasted code;
    // the loopback listener is only bound for the others. A busy fixed port
    // fails here, before consent.
    let manual = oauth::needs_pasted_code(tool);
    let (listener, port) = if manual {
        (None, 0)
    } else {
        let (l, p) = oauth::bind(tool).map_err(|e| e.to_string())?;
        (Some(l), p)
    };
    let url = oauth::authorize_url(tool, &pkce, &state, port);
    // The window shows the link too (like 9router's fallback): if the browser
    // did not come up, the user copies it; the callback still lands here.
    on_url(&url);
    if let Err(e) = oauth::open_browser(&url) {
        eprintln!("zerith-desktop: navegador não abriu ({e}); link exibido na janela");
    }
    let code = match listener {
        Some(listener) => oauth::wait_for_code(listener, oauth::redirect_path(tool), &state)
            .map_err(|e| e.to_string())?,
        None => {
            let (tx, rx) = std::sync::mpsc::channel();
            if let Ok(mut slot) = OAUTH_CODE.lock() {
                *slot = Some(tx);
            }
            let pasted = rx
                .recv_timeout(oauth::LOGIN_WINDOW)
                .map_err(|_| "o código não chegou em 5 min; comece de novo".to_string())?;
            if let Ok(mut slot) = OAUTH_CODE.lock() {
                *slot = None;
            }
            let (code, pasted_state) = oauth::pasted_code(&pasted);
            if let Some(pasted_state) = pasted_state {
                if pasted_state != state {
                    return Err("o código é de outra tentativa (state não confere)".into());
                }
            }
            code.to_string()
        }
    };
    let cred = oauth::exchange(
        tool,
        &code,
        &pkce.verifier,
        &oauth::redirect_uri(tool, port),
    )
    .map_err(|e| e.to_string())?;

    connect_account(root, tool, account, &cred)
}

/// Takes the credential the CLI already wrote (`~/.claude/.credentials.json`,
/// `~/.codex/auth.json`) instead of running the browser flow. This is what
/// 9router does for Claude: no OAuth round trip, just the CLI's token.
pub fn oauth_import(root: &Path, tool: OAuthTool, account: &str) -> Result<String, String> {
    let account = account.trim();
    if account.is_empty() {
        return Err("Informe o ID da conta antes de entrar".into());
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .ok_or("não sei a pasta do usuário")?;
    let cred = zerith_providers::oauth::import_local(tool, &home).ok_or_else(|| {
        format!(
            "o CLI do {} não tem login salvo; rode o CLI uma vez, ou use Conectar",
            tool.as_str()
        )
    })?;
    connect_account(root, tool, account, &cred)
}

fn connect_account(
    root: &Path,
    tool: OAuthTool,
    account: &str,
    cred: &zerith_providers::OAuthCredential,
) -> Result<String, String> {
    zerith_core::secrets::save_oauth(&zerith_core::secrets::OsKeyring, account, cred)
        .map_err(|e| e.to_string())?;
    let acc = settings::AccountEntry {
        id: account.to_string(),
        provider: oauth_provider(tool).to_string(),
        auth: AuthModeSettings::Oauth,
        oauth_tool: Some(tool.as_str().to_string()),
        ..Default::default()
    };
    save_account(root, &acc, None)?;
    let minutes = (cred.expires_at - zerith_providers::oauth::now_secs()).max(0) / 60;
    Ok(format!(
        "conta {account} conectada via {} (expira em {minutes} min)",
        tool.as_str()
    ))
}

/// A second subscription of the same CLI: the user logs in elsewhere (another
/// machine, another `CLAUDE_CONFIG_DIR`) and pastes the `.credentials.json`.
pub fn oauth_import_pasted(
    root: &Path,
    tool: OAuthTool,
    account: &str,
    raw: &str,
) -> Result<String, String> {
    let account = account.trim();
    if account.is_empty() {
        return Err("Informe o ID da conta antes de entrar".into());
    }
    let cred = match tool {
        OAuthTool::ClaudeCode => zerith_providers::oauth::parse_claude_credentials(raw)
            .ok_or("não achei claudeAiOauth.accessToken nesse JSON")?,
        OAuthTool::Codex => zerith_providers::oauth::parse_codex_credentials(raw)
            .ok_or("não achei tokens.access_token nesse JSON")?,
        OAuthTool::Antigravity => {
            zerith_providers::oauth::parse_9router_credentials(OAuthTool::Antigravity, raw)
                .ok_or("não achei accessToken nesse JSON (formato do 9router)")?
        }
    };
    connect_account(root, tool, account, &cred)
}

/// The subscription 9router already stores, copied straight from its sqlite.
/// Antigravity has no other way in: its own credential is DPAPI-sealed.
pub fn oauth_import_9router(root: &Path, tool: OAuthTool, account: &str) -> Result<String, String> {
    let account = account.trim();
    if account.is_empty() {
        return Err("Informe o ID da conta antes de entrar".into());
    }
    let db = zerith_providers::oauth::ninerouter_db()
        .ok_or("não achei o banco do 9router (AppData/Roaming/9router)")?;
    let cred = zerith_providers::oauth::import_from_9router(tool, &db)
        .ok_or("o 9router não tem conexão ativa deste provedor")?;
    connect_account(root, tool, account, &cred)
}

/// Plan quota for an OAuth account, refreshing the token when it went stale.
pub fn account_quota(account: &str) -> Result<zerith_providers::oauth::Quota, String> {
    let cred = fresh_oauth(account)?;
    zerith_providers::oauth::fetch_quota(cred.tool, &cred.access_token).map_err(|e| e.to_string())
}

/// The live model list of an OAuth account (Antigravity publishes one).
pub fn account_models(account: &str) -> Result<Vec<zerith_providers::oauth::ModelInfo>, String> {
    let cred = fresh_oauth(account)?;
    zerith_providers::oauth::fetch_models(cred.tool, &cred.access_token).map_err(|e| e.to_string())
}

/// The account's credential, renewed when it went stale.
fn fresh_oauth(account: &str) -> Result<zerith_providers::OAuthCredential, String> {
    let mut cred = zerith_core::secrets::load_oauth(&zerith_core::secrets::OsKeyring, account)
        .map_err(|e| e.to_string())?
        .ok_or("sem credencial no cofre para esta conta")?;
    // Codex is never refreshed here: its refresh token rotates on use, so
    // renewing it from the app would log the user's `codex` CLI out.
    if cred.tool != OAuthTool::Codex && cred.expires_at <= zerith_providers::oauth::now_secs() {
        cred = zerith_providers::oauth::refresh(cred.tool, &cred).map_err(|e| e.to_string())?;
        zerith_core::secrets::save_oauth(&zerith_core::secrets::OsKeyring, account, &cred)
            .map_err(|e| e.to_string())?;
    }
    Ok(cred)
}

/// Where an account's credential stands, for the pill: which tool and how many
/// minutes are left. `None` = no credential in the cofre.
pub fn oauth_status(account: &str) -> Option<(OAuthTool, i64)> {
    let cred = zerith_core::secrets::load_oauth(&zerith_core::secrets::OsKeyring, account)
        .ok()
        .flatten()?;
    let minutes = (cred.expires_at - zerith_providers::oauth::now_secs()).max(0) / 60;
    Some((cred.tool, minutes))
}

pub fn delete_account(root: &Path, id: &str) -> Result<(), String> {
    Settings::save_user(&Paths::with_root(root), |s| {
        s.accounts.retain(|a| a.id != id);
    })
    .map_err(|e| e.to_string())
}

/// Loads multi-model routing & delegation settings from the merged layers.
pub fn load_routing(root: &Path) -> settings::RoutingSettings {
    load(root).routing
}

/// Persists multi-model routing & delegation settings in the user settings layer.
pub fn save_routing(root: &Path, routing: &settings::RoutingSettings) -> Result<(), String> {
    let routing = routing.clone();
    Settings::save_user(&Paths::with_root(root), |s| {
        s.routing = routing;
    })
    .map_err(|e| e.to_string())
}

/// Loads Token Saver configuration from the merged layers.
pub fn load_token_saver(root: &Path) -> settings::TokenSaverSettings {
    load(root).token_saver
}

/// Persists Token Saver configuration in the user settings layer.
pub fn save_token_saver(root: &Path, ts: &settings::TokenSaverSettings) -> Result<(), String> {
    let ts = ts.clone();
    Settings::save_user(&Paths::with_root(root), |s| {
        s.token_saver = ts;
    })
    .map_err(|e| e.to_string())
}

/// Summarizes rolling-window usage, quota consumption, and Token Saver savings.
pub fn usage_summary(db: &Path, window_hours: u32) -> Result<UsageSummary, String> {
    Store::open(db)
        .and_then(|st| st.usage_summary(window_hours))
        .map_err(|e| e.to_string())
}

/// Clears any active failover cooldown for `account_id`.
pub fn clear_account_cooldown(db: &Path, account_id: &str) -> Result<(), String> {
    Store::open(db)
        .and_then(|st| st.clear_account_cooldown(account_id))
        .map_err(|e| e.to_string())
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

/// The numbers behind an execution profile (history, subagents, caches).
pub fn profile_limits(p: Profile) -> settings::ProfileLimits {
    match p {
        Profile::Economy => settings::Profile::Economy,
        Profile::Balanced => settings::Profile::Balanced,
        Profile::Performance => settings::Profile::Performance,
    }
    .limits()
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

// ------------------------------------------------------------------ media

use zerith_core::MediaSettings;
use zerith_providers::media::{Kind as MediaKind, OpenRouter};

use crate::types::{Backend, KeySource};

/// The saved `[media]` section (model and fixed parameters per kind).
pub fn media_load(root: &Path) -> MediaSettings {
    load(root).media
}

/// Writes one kind's model and parameters to the user settings layer.
pub fn media_save(
    root: &Path,
    kind: MediaKind,
    model: &str,
    params: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    let (model, params) = (model.to_string(), params.clone());
    Settings::save_user(&Paths::with_root(root), |s| {
        let slot = match kind {
            MediaKind::Image => &mut s.media.image,
            MediaKind::Video => &mut s.media.video,
        };
        slot.model = model;
        slot.params = params;
    })
    .map_err(|e| e.to_string())
}

/// The folder generated files go to (`media.output_dir`, else `<root>/media`).
pub fn media_dir(root: &Path) -> PathBuf {
    let paths = Paths::with_root(root);
    load(root).media.resolve_output_dir(&paths)
}

/// Opens a generated file with the OS default app. Refuses anything that is
/// not a file under `dir`: no URL, no relative path, no `..`.
pub fn open_media(dir: &Path, path: &Path) -> Result<(), String> {
    if !crate::types::is_under(dir, path) {
        return Err("Só abro arquivos da pasta de mídia.".into());
    }
    // Native separators: `explorer C:/a/b.png` does not find the file.
    let native: PathBuf = path.components().collect();
    if !native.is_file() {
        return Err(format!("O arquivo não existe mais: {}", native.display()));
    }
    reveal(&native)
}

/// The real media backend: OpenRouter for the catalogue and the key check, the
/// OS keyring (`openrouter` entry) for the key. The key is read at the moment
/// of a request and never stored in the window's state.
pub fn media_backend(base_url: String) -> Backend {
    use std::sync::Arc;
    let url = move || Some(base_url.clone()).filter(|u| !u.trim().is_empty());
    let (u1, u2) = (url.clone(), url);
    Backend {
        // Listing models needs no key.
        models: Arc::new(move |kind, q, off, lim| {
            OpenRouter::new(None, u1())
                .models(kind, q, off, lim)
                .map_err(|e| e.to_string())
        }),
        resolve_key: Arc::new(|| {
            if std::env::var(zerith_core::secrets::OPENROUTER_ENV)
                .is_ok_and(|v| !v.trim().is_empty())
            {
                return Ok(Some(KeySource::Env));
            }
            match zerith_core::secrets::resolve_openrouter_key() {
                Ok(Some(_)) => Ok(Some(KeySource::Keyring)),
                Ok(None) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }),
        save_key: Arc::new(|key| {
            zerith_core::secrets::set_secret(zerith_core::secrets::OPENROUTER_ENTRY, key)
                .map_err(|e| e.to_string())
        }),
        check_key: Arc::new(move || {
            let key = zerith_core::secrets::resolve_openrouter_key()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Nenhuma chave salva.".to_string())?;
            OpenRouter::new(Some(key), u2())
                .check_key()
                .map_err(|e| e.to_string())
        }),
    }
}

// ------------------------------------------------------------ agent work

/// Whether the store can rewind a turn: the checkpoints module exists in
/// `zerith_sessions`, so the "Rebobinar" affordance is real, not a placeholder.
pub fn checkpoints_supported() -> bool {
    true
}

/// How many checkpoints the session has (what Rebobinar can reach).
pub fn checkpoint_turns(db: &Path, session: &str) -> Vec<i64> {
    let root = db.parent().unwrap_or(Path::new("."));
    Store::open(db)
        .ok()
        .and_then(|st| zerith_sessions::checkpoints::list(&st, root, session).ok())
        .map(|v| v.into_iter().map(|c| c.turn).collect())
        .unwrap_or_default()
}

/// Restores the newest checkpoint: files and conversation. Returns a one-line
/// report for the status text.
pub fn rewind_latest(db: &Path, session: &str) -> Result<String, String> {
    use zerith_sessions::checkpoints::{self, Mode};
    let root = db.parent().unwrap_or(Path::new("."));
    let st = Store::open(db).map_err(|e| e.to_string())?;
    let turn = *checkpoint_turns(db, session)
        .last()
        .ok_or("nenhum ponto de restauração")?;
    let r =
        checkpoints::restore(&st, root, session, turn, Mode::Both).map_err(|e| e.to_string())?;
    Ok(format!(
        "Rebobinado: {} arquivo(s) restaurados, {} removidos, {} mensagem(ns) arquivadas",
        r.files_restored, r.files_removed, r.messages_archived
    ))
}

/// Hands the answers of an `ask_user` card to the runtime.
///
/// TODO(wire): ask_user. Expected: `zerith_runtime::QuestionSender::answer(id,
/// Vec<Vec<String>>)`, created with the turn beside `approval_channel()`; an
/// empty vector means the user declined.
pub fn answer_question(
    sender: Option<&zerith_runtime::QuestionSender>,
    id: &str,
    answers: &[Vec<String>],
) {
    let (Some(tx), Ok(id)) = (sender, id.parse::<u64>()) else {
        return;
    };
    // The runtime takes one flat list: every pick of every question, in order.
    tx.answer(id, answers.iter().flatten().cloned().collect());
}

/// Compacts the open conversation now, off the UI thread. Returns the number
/// of messages summarized, or why nothing happened.
pub fn compact_now(
    db: &Path,
    cfg: ProviderConfig,
    session: &str,
    model: &str,
) -> Result<usize, String> {
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let client =
        zerith_providers::Client::new(cfg).map_err(|e| zerith_providers::explain::headline(&e))?;
    zerith_runtime::compact_now(
        &store,
        &client,
        session,
        model,
        &zerith_providers::Cancel::default(),
        None,
    )
    .map(|r| r.map_or(0, |r| r.summarized))
    .map_err(|e| match e {
        zerith_runtime::RunError::Provider(p) => zerith_providers::explain::headline(&p),
        other => other.to_string(),
    })
}

/// Price of a model in USD per million tokens.
///
/// TODO(wire): pricing. Expected: `ProviderConfig::price(model) -> Option<(f64, f64)>`
/// (input, output). Unknown prices return `None` and the UI shows no cost.
pub fn model_price(_cfg: &ProviderConfig, _model: &str) -> Option<crate::types::Price> {
    None
}

// ------------------------------------------------------------ hermes import

/// Detects Hermes `state.db` installations and previews each valid database
/// without writing to the source.
pub fn detect_hermes() -> Vec<zerith_importers::SourcePreview> {
    zerith_importers::detect()
        .into_iter()
        .filter(|l| l.sessions_table && l.messages_table)
        .filter_map(|l| zerith_importers::preview(&l.path).ok())
        .collect()
}

/// Previews a user-supplied Hermes source (`.db` or `.jsonl`) without writing
/// to the source.
pub fn preview_hermes(path: &Path) -> Result<zerith_importers::SourcePreview, String> {
    if !path.is_file() {
        return Err(format!("arquivo não encontrado: {}", path.display()));
    }
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
    {
        return preview_hermes_jsonl(path);
    }
    zerith_importers::preview(path).map_err(|e| e.to_string())
}

fn preview_hermes_jsonl(path: &Path) -> Result<zerith_importers::SourcePreview, String> {
    use std::io::BufRead;

    use serde_json::{Map, Value};
    use zerith_importers::{SessionPreview, SourceLayout, SourcePreview};

    #[derive(Default)]
    struct Pending {
        meta: Map<String, Value>,
        message_count: usize,
    }

    let str_field = |m: &Map<String, Value>, keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| match m.get(*k)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let truthy = |v: &Value| -> bool {
        match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
            _ => false,
        }
    };
    let time_ms = |m: &Map<String, Value>, keys: &[&str]| -> i64 {
        keys.iter()
            .find_map(|k| match m.get(*k)? {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.parse().ok(),
                _ => None,
            })
            .filter(|&secs| secs > 0.0)
            .map_or(0, |secs| (secs * 1000.0) as i64)
    };

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = std::io::BufReader::new(file);
    let mut order: Vec<(String, Pending)> = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let is_message = obj.contains_key("role") && !obj.contains_key("messages");
        let sid = if is_message {
            str_field(&obj, &["session_id"])
        } else {
            str_field(&obj, &["id", "session_id"])
        };
        let Some(sid) = sid else {
            continue;
        };
        let idx = match order.iter().position(|(k, _)| *k == sid) {
            Some(i) => i,
            None => {
                order.push((sid, Pending::default()));
                order.len() - 1
            }
        };
        let p = &mut order[idx].1;
        if is_message {
            if obj.get("active").is_none_or(|v| v.is_null() || truthy(v)) {
                p.message_count += 1;
            }
        } else {
            if let Some(Value::Array(ms)) = obj.remove("messages") {
                p.message_count += ms
                    .into_iter()
                    .filter(|m| {
                        m.as_object().is_some_and(|mo| {
                            mo.get("active").is_none_or(|v| v.is_null() || truthy(v))
                        })
                    })
                    .count();
            }
            p.meta = obj;
        }
    }

    let mut sessions = Vec::with_capacity(order.len());
    let mut total_messages = 0;
    let mut min_date: Option<i64> = None;
    let mut max_date: Option<i64> = None;

    for (sid, p) in order {
        let m = &p.meta;
        let created_at = time_ms(m, &["started_at", "created_at"]);
        let updated = time_ms(m, &["ended_at", "updated_at"]);
        let updated_at = if updated == 0 { created_at } else { updated };
        total_messages += p.message_count;

        let min_cand = if created_at > 0 {
            Some(created_at)
        } else if updated_at > 0 {
            Some(updated_at)
        } else {
            None
        };
        if let Some(d) = min_cand {
            min_date = Some(min_date.map_or(d, |cur| cur.min(d)));
        }
        let max_cand = if updated_at > 0 {
            Some(updated_at)
        } else if created_at > 0 {
            Some(created_at)
        } else {
            None
        };
        if let Some(d) = max_cand {
            max_date = Some(max_date.map_or(d, |cur| cur.max(d)));
        }

        sessions.push(SessionPreview {
            title: str_field(m, &["title"]).unwrap_or_else(|| sid.clone()),
            id: sid,
            model: str_field(m, &["model"]),
            created_at,
            updated_at,
            message_count: p.message_count,
            archived: m.get("archived").is_some_and(truthy),
        });
    }

    Ok(SourcePreview {
        layout: SourceLayout {
            path: path.to_path_buf(),
            profile: "jsonl".to_string(),
            sessions_table: true,
            messages_table: true,
        },
        sessions,
        total_messages,
        min_date,
        max_date,
    })
}

/// Imports selected Hermes sessions (`.db`) or an exported `.jsonl` file into
/// the Zerith store at `db`.
pub fn run_hermes_import(
    db: &Path,
    source_path: &Path,
    profile: &str,
    include_archived: bool,
    only_ids: Option<Vec<String>>,
) -> Result<zerith_importers::ImportReport, String> {
    let store = Store::open(db).map_err(|e| e.to_string())?;
    if source_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
    {
        zerith_importers::import_jsonl(source_path, profile, &store).map_err(|e| e.to_string())
    } else {
        zerith_importers::import_selected(
            source_path,
            profile,
            &store,
            include_archived,
            only_ids.as_deref(),
        )
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account's provider string must resolve to a real preset, otherwise
    /// the runtime treats it as `Custom` with no endpoint and the login is dead
    /// weight.
    #[test]
    fn oauth_provider_resolves_to_a_known_preset() {
        assert_eq!(oauth_provider(OAuthTool::ClaudeCode), "anthropic");
        assert_eq!(oauth_provider(OAuthTool::Codex), "codex");
        for tool in [OAuthTool::ClaudeCode, OAuthTool::Codex] {
            assert_ne!(
                ProviderKind::parse(oauth_provider(tool)),
                ProviderKind::Custom,
                "{} não vira preset",
                tool.as_str()
            );
        }
    }
}
