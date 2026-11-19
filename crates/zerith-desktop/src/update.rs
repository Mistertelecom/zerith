//! Self-update without a plugin: ask GitHub for the latest release, download
//! the NSIS installer it carries, check its SHA-256 and hand over to it.
//!
//! Blocking `ureq` on a worker thread, like every other network call here.
//! Tests point the check at a local fake with `ZERITH_UPDATE_URL`; only when
//! that override is a `http://127.0.0.1` URL do plain-http loopback downloads
//! pass the host allow-list (see `loopback`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::state::AppState;

const LATEST: &str = "https://api.github.com/repos/Mistertelecom/zerith/releases/latest";
const UA: &str = concat!("zerith-desktop/", env!("CARGO_PKG_VERSION"));
/// Ceiling for the installer download (the current one is ~10 MB).
const CAP: u64 = 200 * 1024 * 1024;
/// The only hosts an installer (or its `.sha256`) may come from, redirects
/// included: release downloads bounce from github.com to one of the others.
const HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// What the window shows: `newer` only when there is an installer to run.
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: Option<String>,
    pub newer: bool,
    pub url: Option<String>,
    /// The release body (markdown), cut to 800 chars.
    pub notes: String,
    /// One pt-BR line saying what the check found, ready for the window.
    pub message: String,
    pub size: Option<u64>,
}

/// `zerith-v0.1.1`, `v0.1.1`, `0.1.1` (and `-beta`/`+build` tails) → numbers.
fn version(tag: &str) -> Option<(u64, u64, u64)> {
    let v = tag.trim();
    let v = v.strip_prefix("zerith-").unwrap_or(v);
    let v = v.strip_prefix('v').unwrap_or(v);
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let t = (
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
        parts.next().unwrap_or(Some(0))?,
    );
    parts.next().is_none().then_some(t)
}

/// Garbage on either side is never newer: a bad tag must not offer an update.
fn newer(tag: &str, current: &str) -> bool {
    matches!((version(tag), version(current)), (Some(a), Some(b)) if a > b)
}

/// The NSIS installer Tauri names `Zerith_<ver>_x64-setup.exe`: url and size.
fn installer(release: &Value) -> Option<(String, u64)> {
    release["assets"].as_array()?.iter().find_map(|a| {
        if !a["name"].as_str()?.ends_with("-setup.exe") {
            return None;
        }
        let url = a["browser_download_url"].as_str()?.to_string();
        Some((url, a["size"].as_u64().unwrap_or(0)))
    })
}

/// https on a GitHub release host, or plain http to 127.0.0.1 when `loopback`.
/// Userinfo (`user@host`) and odd schemes fail closed.
fn allowed(url: &str, loopback: bool) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    match scheme {
        "https" => {
            let host = authority.strip_suffix(":443").unwrap_or(authority);
            HOSTS.contains(&host.to_ascii_lowercase().as_str())
        }
        "http" => loopback && authority.split(':').next() == Some("127.0.0.1"),
        _ => false,
    }
}

/// Test hook: plain-http downloads from 127.0.0.1 are allowed only while the
/// release check itself is pointed at a loopback fake. Never set in real use.
fn loopback() -> bool {
    std::env::var("ZERITH_UPDATE_URL")
        .is_ok_and(|u| u.starts_with("http://127.0.0.1:") || u.starts_with("http://127.0.0.1/"))
}

fn check() -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION");
    let mut info = UpdateInfo {
        current: current.into(),
        ..Default::default()
    };
    let url = std::env::var("ZERITH_UPDATE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| LATEST.into());
    let agent = ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout(Duration::from_secs(20))
        .build();
    let release: Value = match agent
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r
            .into_json()
            .map_err(|e| format!("resposta inválida do GitHub: {e}"))?,
        // No published release yet (drafts do not count): nothing to offer.
        Err(ureq::Error::Status(404, _)) => {
            info.message = "Nenhuma versão publicada ainda.".into();
            return Ok(info);
        }
        Err(ureq::Error::Status(code, _)) => {
            return Err(format!(
                "o GitHub respondeu {code} ao procurar atualizações"
            ))
        }
        Err(e) => return Err(format!("sem conexão com o GitHub: {e}")),
    };
    let tag = release["tag_name"].as_str().unwrap_or_default();
    info.latest = version(tag).map(|(a, b, c)| format!("{a}.{b}.{c}"));
    info.notes = release["body"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(800)
        .collect();
    let latest = info.latest.clone().unwrap_or_else(|| tag.to_string());
    info.message = if !newer(tag, current) {
        "Você está na versão mais recente.".into()
    } else {
        match installer(&release) {
            Some((url, size)) if cfg!(windows) => {
                info.newer = true;
                info.url = Some(url);
                info.size = Some(size);
                format!("Nova versão {latest} disponível.")
            }
            Some(_) => "A atualização automática existe só no Windows.".into(),
            None => format!("A versão {latest} ainda não tem instalador para Windows."),
        }
    };
    Ok(info)
}

/// GET that follows redirects by hand so every hop passes `allowed`.
/// `Ok(None)` = 404.
fn get(url: &str, loopback: bool) -> Result<Option<ureq::Response>, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut url = url.to_string();
    for _ in 0..6 {
        if !allowed(&url, loopback) {
            return Err(format!(
                "download recusado: {url} não é um endereço do GitHub"
            ));
        }
        match agent.get(&url).call() {
            Ok(r) if (300..400).contains(&r.status()) => {
                url = r
                    .header("location")
                    .ok_or("redirecionamento sem destino")?
                    .to_string()
            }
            Ok(r) => return Ok(Some(r)),
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!(
                    "o servidor respondeu {code} ao baixar a atualização"
                ))
            }
            Err(e) => return Err(format!("falha de rede ao baixar a atualização: {e}")),
        }
    }
    Err("redirecionamentos demais ao baixar a atualização".into())
}

/// The digest a `sha256sum` line carries (`<hex>  <name>` or `<hex> *<name>`).
fn sha_line(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?.to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// Downloads the installer to `<root>/updates/<name>`; refuses it when the
/// release's `<asset>.sha256` exists and does not match.
fn download(root: &Path, url: &str) -> Result<PathBuf, String> {
    let loopback = loopback();
    let name = url.rsplit('/').next().unwrap_or_default();
    let clean = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if !allowed(url, loopback) || !name.ends_with("-setup.exe") || !clean {
        return Err(format!("endereço de atualização recusado: {url}"));
    }
    // Only an explicit 404 skips the check; any other failure refuses.
    let expected = match get(&format!("{url}.sha256"), loopback)? {
        Some(r) => {
            let mut text = String::new();
            r.into_reader()
                .take(4096)
                .read_to_string(&mut text)
                .map_err(|e| format!("não consegui ler o .sha256: {e}"))?;
            Some(sha_line(&text).ok_or("o arquivo .sha256 da versão está ilegível")?)
        }
        None => None,
    };
    let resp = get(url, loopback)?.ok_or("instalador não encontrado na versão (404)")?;
    if resp
        .header("content-length")
        .and_then(|l| l.parse::<u64>().ok())
        .is_some_and(|n| n > CAP)
    {
        return Err("instalador maior que 200 MB: recusado".into());
    }
    let dir = root.join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let part = dir.join(format!("{name}.part"));
    let got = (|| -> Result<String, String> {
        let mut file =
            std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        let mut reader = resp.into_reader().take(CAP + 1);
        let (mut buf, mut total) = (vec![0u8; 64 * 1024], 0u64);
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("download interrompido: {e}"))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > CAP {
                return Err("instalador maior que 200 MB: recusado".into());
            }
            ctx.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| format!("{}: {e}", part.display()))?;
        }
        file.flush().map_err(|e| e.to_string())?;
        Ok(ctx
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    })();
    let got = got.inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    if let Some(want) = expected {
        if got != want {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "SHA-256 do instalador não confere (esperado {want}, recebido {got}). O arquivo foi apagado e nada foi instalado."
            ));
        }
    }
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Starts the installer detached. Switches, read from Tauri's generated
/// `installer.nsi` (target/release/nsis/x64):
///   /P       passive: progress bar only, no pages, closes itself; also lets
///            `CheckIfAppIsRunning` close a running Zerith without asking
///   /R       relaunch Zerith when done (honoured only with /P or /S)
///   /UPDATE  skip the reinstall/uninstall page and the WebView2 bootstrap,
///            keep the user's shortcuts as they are
/// /S would hide the progress, so `/P /R /UPDATE` (the same set
/// tauri-plugin-updater's "passive" mode passes): the user sees a bar, then
/// Zerith again.
#[cfg(windows)]
fn launch(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let child = std::process::Command::new(path)
        .args(["/P", "/R", "/UPDATE"])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| format!("não consegui abrir o instalador: {e}"))?;
    eprintln!(
        "zerith-desktop: instalador {} iniciado (pid {}); saindo para ele trocar os arquivos",
        path.display(),
        child.id()
    );
    Ok(())
}

#[cfg(not(windows))]
fn launch(_: &Path) -> Result<(), String> {
    Err("a atualização automática existe só no Windows".into())
}

/// Asks GitHub for the latest release. No release yet = `newer: false`.
#[tauri::command]
pub async fn update_check() -> Result<UpdateInfo, String> {
    tauri::async_runtime::spawn_blocking(check)
        .await
        .map_err(|e| e.to_string())?
}

/// Downloads, verifies and starts the installer, then quits so it can replace
/// the files. Returns only on failure (or just before the exit).
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        launch(&download(&root, &url)?)?;
        // Let the answer reach the window, then step aside.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            app.exit(0);
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_and_garbage_is_never_newer() {
        assert!(newer("zerith-v0.1.1", "0.1.0"));
        assert!(newer("v0.2.0", "0.1.1"));
        assert!(newer("0.10.0", "0.9.9"), "numbers, not strings");
        assert!(newer("zerith-v1", "0.9.0"));
        assert!(!newer("zerith-v0.1.0", "0.1.0"));
        assert!(!newer("v0.0.9", "0.1.0"));
        assert!(!newer("latest", "0.1.0"));
        assert!(!newer("zerith-v0.x.1", "0.1.0"));
        assert!(!newer("v1.2.3.4", "0.1.0"));
        assert!(!newer("", "0.1.0"));
        assert_eq!(version("zerith-v0.2.0-beta.1"), Some((0, 2, 0)));
    }

    #[test]
    fn the_nsis_setup_asset_is_picked() {
        let release = serde_json::json!({ "assets": [
            { "name": "zerith-zerith-v0.2.0-windows-x86_64.zip", "browser_download_url": "https://github.com/a/zip", "size": 1 },
            { "name": "Zerith_0.2.0_x64-setup.exe.sha256", "browser_download_url": "https://github.com/a/sha", "size": 2 },
            { "name": "Zerith_0.2.0_x64-setup.exe", "browser_download_url": "https://github.com/a/setup", "size": 3 },
        ]});
        assert_eq!(
            installer(&release),
            Some(("https://github.com/a/setup".into(), 3))
        );
        assert_eq!(installer(&serde_json::json!({ "assets": [] })), None);
        assert_eq!(installer(&serde_json::json!({})), None);
    }

    #[test]
    fn only_github_release_hosts_pass() {
        let gh = "https://github.com/Mistertelecom/zerith/releases/download/zerith-v0.2.0/Zerith_0.2.0_x64-setup.exe";
        assert!(allowed(gh, false));
        assert!(allowed("https://objects.githubusercontent.com/x", false));
        assert!(allowed(
            "https://release-assets.githubusercontent.com:443/x",
            false
        ));
        assert!(
            !allowed(&gh.replace("https", "http"), false),
            "no plain http"
        );
        assert!(!allowed("https://github.com.evil.io/x", false));
        assert!(!allowed("https://evil.io/github.com/x", false));
        assert!(!allowed("https://github.com@evil.io/x", false));
        assert!(!allowed("file:///C:/x-setup.exe", false));
        assert!(
            !allowed("http://127.0.0.1:8000/x", false),
            "loopback only in tests"
        );
        assert!(allowed("http://127.0.0.1:8000/x", true));
        assert!(!allowed("http://localhost:8000/x", true));
        assert!(!allowed("http://10.0.0.1/x", true));
    }

    #[test]
    fn sha256_lines_parse() {
        let hex = "ab".repeat(32);
        assert_eq!(
            sha_line(&format!("{hex}  Zerith_0.2.0_x64-setup.exe\n")),
            Some(hex.clone())
        );
        assert_eq!(sha_line(&format!("{} *x", hex.to_uppercase())), Some(hex));
        assert_eq!(sha_line("not a hash"), None);
    }
}
