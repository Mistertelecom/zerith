//! MCP servers screen (Ajustes → MCP): list, add, remove, switch, approve,
//! test, and the one-way import from Hermes' `config.yaml`.
//!
//! Env values and headers may be secrets: the window gets their *keys* only.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use zerith_core::Paths;
use zerith_importers::hermes_setup::{self, SetupReport};
use zerith_mcp::config::{
    add_server, approve_server, load_servers, remove_server, set_enabled, set_extra,
};
use zerith_mcp::{oauth, Extra, McpRegistry, Origin, ServerConfig};

use crate::state::AppState;

/// How long "Testar" waits for a server before calling it hung.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpView {
    name: String,
    transport: &'static str,
    /// `command args…` or the URL.
    target: String,
    enabled: bool,
    approved: bool,
    project: bool,
    env_keys: Vec<String>,
    header_keys: Vec<String>,
    /// `auth = "oauth"`: the row shows Entrar/Sair.
    oauth: bool,
    /// Tokens in the vault (always false without `oauth`).
    connected: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    name: String,
    description: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesMcpView {
    name: String,
    transport: &'static str,
    target: String,
    /// importavel | nao-encontrado | oauth | existe | invalido
    status: &'static str,
    enabled: bool,
    env_keys: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportView {
    imported: Vec<String>,
    existed: Vec<String>,
    failed: Vec<String>,
}

impl From<SetupReport> for ImportView {
    fn from(r: SetupReport) -> Self {
        Self {
            imported: r.imported,
            existed: r.existed,
            failed: r.failed,
        }
    }
}

fn target(command: &Option<String>, args: &[String], url: &Option<String>) -> String {
    url.clone()
        .unwrap_or_else(|| format!("{} {}", command.clone().unwrap_or_default(), args.join(" ")))
        .trim()
        .to_string()
}

/// The project whose `.zerith/mcp.toml` the screen shows: the folder the
/// latest conversation works in, the same one a new turn starts from.
fn project(paths: &Paths) -> Option<std::path::PathBuf> {
    Some(crate::turn::last_workspace(&paths.db()))
}

fn list(paths: &Paths) -> Vec<McpView> {
    let l = load_servers(paths, project(paths).as_deref());
    let keys = |m: Option<&BTreeMap<String, String>>| {
        m.map(|m| m.keys().cloned().collect()).unwrap_or_default()
    };
    let row = |s: &ServerConfig, enabled: bool, approved: bool| {
        let x = l.extras.get(&s.name);
        McpView {
            name: s.name.clone(),
            transport: if s.is_http() { "http" } else { "stdio" },
            target: target(&s.command, &s.args, &s.url),
            enabled,
            approved,
            project: s.origin == Origin::Project,
            env_keys: keys(x.map(|x| &x.env)),
            header_keys: keys(x.map(|x| &x.headers)),
            oauth: s.is_oauth(),
            connected: s.is_oauth()
                && oauth::status(&zerith_core::secrets::OsKeyring, &s.name).connected,
        }
    };
    let mut out: Vec<McpView> = l.servers.iter().map(|s| row(s, true, true)).collect();
    out.extend(l.disabled.iter().map(|s| row(s, false, true)));
    out.extend(l.unapproved.iter().map(|s| row(s, true, false)));
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `KEY=valor` per line; blank lines and `#` comments skipped.
fn pairs(text: &str) -> Result<BTreeMap<String, String>, String> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
        .map(|(i, l)| {
            // The line itself is not echoed: it may be a pasted secret.
            let (k, v) = l
                .split_once('=')
                .ok_or_else(|| format!("linha {} sem `=` (use CHAVE=valor)", i + 1))?;
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mcp_list(state: State<'_, AppState>) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || Ok(list(&paths))).await
}

/// Adds (or replaces) a user server. `command` is split on whitespace into
/// program and arguments (no shell); `env`/`headers` are `KEY=valor` lines.
#[tauri::command]
pub async fn mcp_add(
    state: State<'_, AppState>,
    name: String,
    command: String,
    url: String,
    env: String,
    headers: String,
) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        let mut words = command.split_whitespace().map(str::to_string);
        let server = ServerConfig {
            name: name.trim().to_string(),
            command: words.next(),
            args: words.collect(),
            url: Some(url.trim().to_string()).filter(|u| !u.is_empty()),
            timeout_ms: None,
            auth: None,
            origin: Origin::User,
        };
        let extra = Extra {
            env: pairs(&env)?,
            headers: pairs(&headers)?,
        };
        let name = server.name.clone();
        add_server(&paths, server)?;
        set_extra(&paths, &name, &extra)?;
        Ok(list(&paths))
    })
    .await
}

#[tauri::command]
pub async fn mcp_remove(state: State<'_, AppState>, name: String) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        if !remove_server(&paths, &name)? {
            return Err(format!(
                "{name} não está em mcp.toml; se vem de settings.toml ou do projeto, edite o arquivo"
            ));
        }
        oauth::forget(paths.root(), &zerith_core::secrets::OsKeyring, &name)?;
        Ok(list(&paths))
    })
    .await
}

#[tauri::command]
pub async fn mcp_toggle(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        set_enabled(&paths, &name, enabled)?;
        Ok(list(&paths))
    })
    .await
}

#[tauri::command]
pub async fn mcp_approve(state: State<'_, AppState>, name: String) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        approve_server(&paths, &name)?;
        Ok(list(&paths))
    })
    .await
}

/// Starts the server, lists its tools, stops it. Disabled and unapproved
/// servers are tested too: the person clicked "Testar" on that row.
#[tauri::command]
pub async fn mcp_test(state: State<'_, AppState>, name: String) -> Result<Vec<McpTool>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        let l = load_servers(&paths, project(&paths).as_deref());
        let mut cfg = l
            .servers
            .into_iter()
            .chain(l.disabled)
            .chain(l.unapproved)
            .find(|s| s.name == name)
            .ok_or_else(|| format!("servidor MCP desconhecido: {name}"))?;
        cfg.timeout_ms = Some(
            cfg.timeout_ms
                .unwrap_or(u64::MAX)
                .min(TEST_TIMEOUT.as_millis() as u64),
        );
        let mut reg = McpRegistry::new(vec![cfg]);
        reg.with_extras(&l.extras);
        let tools = reg.list_tools(&name).map_err(|e| e.to_string())?;
        Ok(tools
            .into_iter()
            .map(|t| McpTool {
                name: t.name,
                description: t.description.unwrap_or_default(),
            })
            .collect())
    })
    .await
}

/// "Entrar": discovery, client registration, browser, code exchange. The URL
/// goes to the window as `oauth:url` (Copiar link) and to the browser, unless
/// `ZERITH_OAUTH_NO_BROWSER=1`.
#[tauri::command]
pub async fn mcp_oauth_login(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<McpView>, String> {
    use tauri::Emitter;
    let paths = Paths::with_root(state.root());
    blocking(move || {
        let l = load_servers(&paths, project(&paths).as_deref());
        let url = l
            .servers
            .iter()
            .chain(&l.disabled)
            .chain(&l.unapproved)
            .find(|s| s.name == name && s.is_oauth())
            .and_then(|s| s.url.clone())
            .ok_or_else(|| format!("{name} não é um servidor MCP com OAuth"))?;
        oauth::login(
            paths.root(),
            &zerith_core::secrets::OsKeyring,
            &name,
            &url,
            &|u| {
                let _ = app.emit("oauth:url", u);
                if std::env::var_os("ZERITH_OAUTH_NO_BROWSER").is_none() {
                    let _ = zerith_providers::oauth::open_browser(u);
                }
            },
        )?;
        Ok(list(&paths))
    })
    .await
}

#[tauri::command]
pub async fn mcp_oauth_status(name: String) -> Result<oauth::Status, String> {
    blocking(move || Ok(oauth::status(&zerith_core::secrets::OsKeyring, &name))).await
}

#[tauri::command]
pub async fn mcp_oauth_logout(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<McpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        oauth::logout(&zerith_core::secrets::OsKeyring, &name)?;
        Ok(list(&paths))
    })
    .await
}

#[tauri::command]
pub async fn mcp_hermes_preview(state: State<'_, AppState>) -> Result<Vec<HermesMcpView>, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        let home = hermes_setup::hermes_home().ok_or("Hermes não encontrado neste computador")?;
        Ok(hermes_setup::hermes_mcp(&home, &paths)?
            .into_iter()
            .map(|(m, st)| HermesMcpView {
                transport: if m.url.is_some() { "http" } else { "stdio" },
                target: target(&m.command, &m.args, &m.url),
                status: st.as_str(),
                enabled: m.enabled,
                env_keys: m.env.keys().chain(m.headers.keys()).cloned().collect(),
                name: m.name,
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn mcp_hermes_import(
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<ImportView, String> {
    let paths = Paths::with_root(state.root());
    blocking(move || {
        let home = hermes_setup::hermes_home().ok_or("Hermes não encontrado neste computador")?;
        hermes_setup::import_hermes_mcp(&home, &paths, &names).map(ImportView::from)
    })
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_lines_parse_and_reject_garbage() {
        let m = super::pairs("# c\nA=1\n\n B = x=y \n").unwrap();
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "x=y");
        assert!(super::pairs("SEM_IGUAL").is_err());
    }
}
