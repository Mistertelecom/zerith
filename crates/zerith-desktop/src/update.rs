//! Self-update without a plugin: ask GitHub for the latest release, download
//! the NSIS installer it carries, verify both its SHA-256 digest and its
//! Ed25519 signature against the embedded public key, and hand over to it.
//!
//! Security properties:
//! - `update_install` never accepts a download URL from the WebView; it
//!   re-queries GitHub's latest release endpoint directly.
//! - The initial download URL must belong to `Mistertelecom/zerith` releases
//!   (redirects may hop only to GitHub's asset hosts).
//! - Both `<asset>.sha256` and `<asset>.sig` are mandatory (404 fails closed).
//! - The Ed25519 signature binds domain (`zerith-update-v1`), normalized
//!   version, file basename, and SHA-256 digest, preventing tampering, asset
//!   swaps, and downgrade/replay attacks.
//! - The loopback test override (`ZERITH_UPDATE_URL`) is compiled out of
//!   release builds (`#[cfg(any(test, debug_assertions))]`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::state::AppState;

const LATEST: &str = "https://api.github.com/repos/Mistertelecom/zerith/releases/latest";
const REPO_DOWNLOAD_PREFIX: &str = "https://github.com/Mistertelecom/zerith/releases/download/";
const UA: &str = concat!("zerith-desktop/", env!("CARGO_PKG_VERSION"));
/// Ceiling for the installer download (the current one is ~10 MB).
const CAP: u64 = 200 * 1024 * 1024;
/// Hosts an installer (or its `.sha256`/`.sig`) may redirect to: GitHub
/// release downloads bounce from `github.com` to one of the asset CDN hosts.
const ASSET_HOSTS: [&str; 2] = [
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const EMBEDDED_PUBKEY_FILE: &str = include_str!("../update-pubkey.txt");

/// What the window shows: `newer` only when there is a newer installer to run.
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: Option<String>,
    pub newer: bool,
    /// The release body (markdown), cut to 800 chars.
    pub notes: String,
    /// One pt-BR line saying what the check found, ready for the window.
    pub message: String,
    pub size: Option<u64>,
}

/// Internal verified release target returned by [`fetch_latest`].
struct LatestRelease {
    info: UpdateInfo,
    installer_url: Option<String>,
}

/// `zerith-v0.1.1`, `v0.1.1`, `0.1.1` → `(major, minor, patch, is_prerelease)`.
fn parse_version(tag: &str) -> Option<(u64, u64, u64, bool)> {
    let v = tag.trim();
    let v = v.strip_prefix("zerith-").unwrap_or(v);
    let v = v.strip_prefix('v').unwrap_or(v);
    let without_build = v.split('+').next()?;
    let (core, pre) = match without_build.split_once('-') {
        Some((c, _)) => (c, true),
        None => (without_build, false),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let t = (
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
        parts.next().unwrap_or(Some(0))?,
        pre,
    );
    parts.next().is_none().then_some(t)
}

/// `zerith-v0.1.1`, `v0.1.1`, `0.1.1` → numeric core `(major, minor, patch)`.
fn version(tag: &str) -> Option<(u64, u64, u64)> {
    parse_version(tag).map(|(a, b, c, _)| (a, b, c))
}

/// Garbage or pre-release tags are never offered as stable updates.
fn newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some((_, _, _, true)), _) => false,
        (Some((a1, b1, c1, false)), Some((a2, b2, c2, cur_pre))) => {
            (a1, b1, c1) > (a2, b2, c2) || ((a1, b1, c1) == (a2, b2, c2) && cur_pre)
        }
        _ => false,
    }
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

/// Parses scheme and authority, failing closed on userinfo (`user@host`) or
/// traversal segments (`..`).
fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let (authority, path_and_query) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let path = path_and_query.split(['?', '#']).next().unwrap_or("");
    if path.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some((scheme, authority, path))
}

fn is_loopback_http(url: &str) -> bool {
    let Some((scheme, authority, _)) = split_url(url) else {
        return false;
    };
    if scheme != "http" {
        return false;
    }
    match authority.split_once(':') {
        Some((host, port)) => host == "127.0.0.1" && port.parse::<u16>().is_ok(),
        None => authority == "127.0.0.1",
    }
}

/// Initial download URL must point directly to this repository's GitHub
/// Releases (or loopback HTTP in test/debug mode).
fn allowed_initial(url: &str, loopback: bool) -> bool {
    if loopback && is_loopback_http(url) {
        return true;
    }
    let Some((scheme, authority, _)) = split_url(url) else {
        return false;
    };
    if scheme != "https" {
        return false;
    }
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    host.eq_ignore_ascii_case("github.com") && url.starts_with(REPO_DOWNLOAD_PREFIX)
}

/// Redirect hops may stay on this repo's GitHub Releases path or bounce to
/// GitHub's release asset CDN hosts (or loopback HTTP in test/debug mode).
fn allowed_hop(url: &str, loopback: bool) -> bool {
    if allowed_initial(url, loopback) {
        return true;
    }
    let Some((scheme, authority, _)) = split_url(url) else {
        return false;
    };
    if scheme != "https" {
        return false;
    }
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    ASSET_HOSTS.contains(&host.to_ascii_lowercase().as_str())
}

/// Test/debug hook only: plain-http downloads from 127.0.0.1 are allowed only
/// while `ZERITH_UPDATE_URL` points at a loopback fake. Compiled out in release
/// builds.
#[cfg(any(test, debug_assertions))]
fn update_endpoint() -> String {
    std::env::var("ZERITH_UPDATE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| LATEST.into())
}

#[cfg(not(any(test, debug_assertions)))]
fn update_endpoint() -> String {
    LATEST.into()
}

#[cfg(any(test, debug_assertions))]
fn loopback() -> bool {
    std::env::var("ZERITH_UPDATE_URL").is_ok_and(|u| is_loopback_http(&u))
}

#[cfg(not(any(test, debug_assertions)))]
fn loopback() -> bool {
    false
}

/// Canonical domain-separated message signed by `scripts/sign-update.mjs`.
fn signing_payload(version: &str, file_name: &str, sha256_hex: &str) -> Vec<u8> {
    format!("zerith-update-v1\nversion={version}\nfile={file_name}\nsha256={sha256_hex}\n")
        .into_bytes()
}

/// Strips `#` comment lines and whitespace, then decodes a 32-byte Ed25519
/// public key from base64.
fn parse_pubkey_text(text: &str) -> Result<Option<[u8; 32]>, String> {
    let body: String = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if body.is_empty() {
        return Ok(None);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|e| format!("chave pública de atualização inválida (base64): {e}"))?;
    let key: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
        format!(
            "chave pública de atualização inválida: esperado 32 bytes, recebido {}",
            v.len()
        )
    })?;
    Ok(Some(key))
}

/// Returns the Ed25519 public key embedded at build time (from
/// `ZERITH_UPDATE_PUBKEY` env var or `crates/zerith-desktop/update-pubkey.txt`).
/// Fails closed if no key is configured.
fn embedded_pubkey() -> Result<[u8; 32], String> {
    let raw = option_env!("ZERITH_UPDATE_PUBKEY")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(EMBEDDED_PUBKEY_FILE);
    parse_pubkey_text(raw)?.ok_or_else(|| {
        "este build não possui chave pública de atualização configurada; baixe a nova versão manualmente no GitHub Releases.".into()
    })
}

/// Verifies the base64 Ed25519 signature from `<asset>.sig` against the
/// canonical update payload.
fn verify_signature(
    pubkey: &[u8; 32],
    version: &str,
    file_name: &str,
    sha256_hex: &str,
    sig_text: &str,
) -> Result<(), String> {
    let cleaned: String = sig_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let sig = base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|_| "o arquivo .sig da versão não é base64 válido".to_string())?;
    if sig.len() != 64 {
        return Err(format!(
            "assinatura .sig com tamanho inválido ({} bytes; esperado 64)",
            sig.len()
        ));
    }
    let payload = signing_payload(version, file_name, sha256_hex);
    UnparsedPublicKey::new(&ED25519, pubkey)
        .verify(&payload, &sig)
        .map_err(|_| {
            "assinatura Ed25519 do instalador é inválida. O arquivo foi apagado e nada foi instalado."
                .into()
        })
}

fn fetch_latest() -> Result<LatestRelease, String> {
    let current = env!("CARGO_PKG_VERSION");
    let mut info = UpdateInfo {
        current: current.into(),
        ..Default::default()
    };
    let url = update_endpoint();
    let agent = zerith_providers::shared_http_agent(20_000);
    let release: Value = match agent
        .get(&url)
        .set("User-Agent", UA)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r
            .into_json()
            .map_err(|e| format!("resposta inválida do GitHub: {e}"))?,
        // No published release yet (drafts do not count): nothing to offer.
        Err(ureq::Error::Status(404, _)) => {
            info.message = "Nenhuma versão publicada ainda.".into();
            return Ok(LatestRelease {
                info,
                installer_url: None,
            });
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
    let mut installer_url = None;
    info.message = if !newer(tag, current) {
        "Você está na versão mais recente.".into()
    } else {
        match installer(&release) {
            Some((url, size)) if cfg!(windows) => {
                info.newer = true;
                info.size = Some(size);
                installer_url = Some(url);
                format!("Nova versão {latest} disponível.")
            }
            Some(_) => "A atualização automática existe só no Windows.".into(),
            None => format!("A versão {latest} ainda não tem instalador para Windows."),
        }
    };
    Ok(LatestRelease {
        info,
        installer_url,
    })
}

fn check() -> Result<UpdateInfo, String> {
    Ok(fetch_latest()?.info)
}

/// GET that follows redirects by hand so the initial URL passes
/// [`allowed_initial`] and every redirect hop passes [`allowed_hop`].
/// `Ok(None)` = 404.
fn get(url: &str, loopback: bool) -> Result<Option<ureq::Response>, String> {
    if !allowed_initial(url, loopback) {
        return Err(format!(
            "download recusado: {url} não pertence às releases oficiais do Zerith"
        ));
    }
    let agent = ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut current = url.to_string();
    for hop in 0..6 {
        if hop > 0 && !allowed_hop(&current, loopback) {
            return Err(format!(
                "download recusado: redirecionamento para {current} não é um endereço do GitHub"
            ));
        }
        match agent.get(&current).call() {
            Ok(r) if (300..400).contains(&r.status()) => {
                current = r
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

/// Downloads the installer to `<root>/updates/<name>`, requiring and verifying
/// both `<asset>.sha256` and `<asset>.sig` (Ed25519) before renaming into place.
fn download_verified(
    root: &Path,
    url: &str,
    expected_version: &str,
    pubkey: &[u8; 32],
    loopback: bool,
) -> Result<PathBuf, String> {
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let clean = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if !allowed_initial(url, loopback) || !name.ends_with("-setup.exe") || !clean {
        return Err(format!("endereço de atualização recusado: {url}"));
    }

    // Both `.sha256` and `.sig` are mandatory: a 404 fails closed.
    let expected_sha = {
        let r = get(&format!("{url}.sha256"), loopback)?
            .ok_or("a versão não traz o arquivo .sha256; atualização recusada")?;
        let mut text = String::new();
        r.into_reader()
            .take(4096)
            .read_to_string(&mut text)
            .map_err(|e| format!("não consegui ler o .sha256: {e}"))?;
        sha_line(&text).ok_or("o arquivo .sha256 da versão está ilegível")?
    };

    let sig_text = {
        let r = get(&format!("{url}.sig"), loopback)?
            .ok_or("a versão não traz assinatura Ed25519 (.sig); atualização recusada")?;
        let mut text = String::new();
        r.into_reader()
            .take(4096)
            .read_to_string(&mut text)
            .map_err(|e| format!("não consegui ler a assinatura .sig: {e}"))?;
        text
    };

    // Fail fast before downloading ~10-200 MB if the signature does not even
    // match the declared version + filename + SHA-256.
    verify_signature(pubkey, expected_version, name, &expected_sha, &sig_text)?;

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

    if got != expected_sha {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "SHA-256 do instalador não confere (esperado {expected_sha}, recebido {got}). O arquivo foi apagado e nada foi instalado."
        ));
    }

    // Re-verify the signature over the actual computed digest of the bytes on disk.
    if let Err(e) = verify_signature(pubkey, expected_version, name, &got, &sig_text) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
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

/// Re-checks the latest release directly from GitHub, downloads and verifies
/// both SHA-256 and the Ed25519 signature, starts the installer, and quits.
/// Accepts no URL from the WebView.
#[tauri::command]
pub async fn update_install(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let pubkey = embedded_pubkey()?;
        let latest = fetch_latest()?;
        if !latest.info.newer {
            return Err(latest.info.message);
        }
        let url = latest
            .installer_url
            .ok_or("a versão mais recente não traz instalador para Windows")?;
        let ver = latest
            .info
            .latest
            .ok_or("versão da release inválida para verificação de assinatura")?;
        let path = download_verified(&root, &url, &ver, &pubkey, loopback())?;
        launch(&path)?;
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
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn scratch() -> tempfile::TempDir {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch")
            });
        let _ = std::fs::create_dir_all(&base);
        tempfile::Builder::new()
            .prefix("upd-")
            .tempdir_in(&base)
            .expect("tempdir in target")
    }

    fn test_keypair() -> (Ed25519KeyPair, [u8; 32]) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from_pkcs8");
        let pubkey: [u8; 32] = kp.public_key().as_ref().try_into().expect("32 bytes");
        (kp, pubkey)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn sign_for(kp: &Ed25519KeyPair, version: &str, name: &str, sha_hex: &str) -> String {
        let sig = kp.sign(&signing_payload(version, name, sha_hex));
        base64::engine::general_purpose::STANDARD.encode(sig.as_ref())
    }

    fn serve_once_map(routes: HashMap<String, (u16, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let count = routes.len().max(4);
        std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let (status, body) = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, b"not found".to_vec()));
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Error",
                };
                let header = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn versions_compare_numerically_and_reject_prereleases() {
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
        assert!(
            !newer("zerith-v0.2.0-beta.1", "0.1.0"),
            "prereleases are never offered as stable updates"
        );
        assert!(
            newer("zerith-v0.2.0", "0.2.0-beta.1"),
            "stable replaces its own prerelease"
        );
    }

    #[test]
    fn the_nsis_setup_asset_is_picked() {
        let release = serde_json::json!({ "assets": [
            { "name": "zerith-zerith-v0.2.0-windows-x86_64.zip", "browser_download_url": "https://github.com/a/zip", "size": 1 },
            { "name": "Zerith_0.2.0_x64-setup.exe.sha256", "browser_download_url": "https://github.com/a/sha", "size": 2 },
            { "name": "Zerith_0.2.0_x64-setup.exe.sig", "browser_download_url": "https://github.com/a/sig", "size": 2 },
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
    fn initial_url_is_pinned_to_official_repo_and_hops_to_github_assets() {
        let gh = "https://github.com/Mistertelecom/zerith/releases/download/zerith-v0.2.0/Zerith_0.2.0_x64-setup.exe";
        assert!(allowed_initial(gh, false));
        assert!(allowed_hop(gh, false));
        assert!(allowed_hop(
            "https://objects.githubusercontent.com/x",
            false
        ));
        assert!(allowed_hop(
            "https://release-assets.githubusercontent.com:443/x",
            false
        ));
        assert!(
            !allowed_initial(
                "https://github.com/attacker/evil/releases/download/v1/x-setup.exe",
                false
            ),
            "another GitHub repository must be rejected"
        );
        assert!(
            !allowed_initial("https://objects.githubusercontent.com/x-setup.exe", false),
            "initial URL must start at Mistertelecom/zerith releases"
        );
        assert!(
            !allowed_initial(
                "https://github.com/Mistertelecom/zerith/releases/download/../../evil/x-setup.exe",
                false
            ),
            "path traversal segments must be rejected"
        );
        assert!(
            !allowed_initial(&gh.replace("https", "http"), false),
            "no plain http"
        );
        assert!(!allowed_hop("https://github.com.evil.io/x", false));
        assert!(!allowed_hop("https://evil.io/github.com/x", false));
        assert!(!allowed_hop("https://github.com@evil.io/x", false));
        assert!(!allowed_hop("file:///C:/x-setup.exe", false));
        assert!(
            !allowed_initial("http://127.0.0.1:8000/x-setup.exe", false),
            "loopback only in test/debug mode"
        );
        assert!(allowed_initial("http://127.0.0.1:8000/x-setup.exe", true));
        assert!(!allowed_initial("http://localhost:8000/x-setup.exe", true));
        assert!(!allowed_initial("http://10.0.0.1/x-setup.exe", true));
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

    #[test]
    fn pubkey_text_ignores_comments_and_rejects_bad_lengths() {
        assert_eq!(parse_pubkey_text("# comment only\n\n").unwrap(), None);
        let (_, key) = test_keypair();
        let b64 = base64::engine::general_purpose::STANDARD.encode(key);
        let text = format!("# header\n{b64}\n");
        assert_eq!(parse_pubkey_text(&text).unwrap(), Some(key));
        assert!(parse_pubkey_text("AAAA").is_err(), "too short");
        assert!(parse_pubkey_text("not!base64").is_err());
    }

    #[test]
    fn verifies_signature_generated_by_node_script() {
        // Vector produced by Node `node:crypto` (`scripts/sign-update.mjs` format).
        let pubkey = parse_pubkey_text("bO3hCha5b7UgafFJFJW3vEJMzWS3c6M9g1727sJPOWA=")
            .expect("valid base64")
            .expect("present");
        let sha = "ef015083900b498ddd4219852d5d41aa4ef04983aa5b4ceb6e7793a7bc874612";
        let sig = "4vrpwtzN0JWIairiNWDU7s/xzNTwv3oAxGNkXBt8+QbEXYXjPFMc1nrUQbVBtUuyDEexSrxbWzwtOH0Az5xaCw==";
        verify_signature(&pubkey, "0.2.0", "Zerith_0.2.0_x64-setup.exe", sha, sig)
            .expect("Node Ed25519 signature must verify in ring");
        assert!(
            verify_signature(&pubkey, "0.2.1", "Zerith_0.2.0_x64-setup.exe", sha, sig).is_err(),
            "changing version must invalidate signature"
        );
    }

    #[test]
    fn download_verified_accepts_valid_signed_installer() {
        let (kp, pubkey) = test_keypair();
        let name = "Zerith_0.2.0_x64-setup.exe";
        let payload = b"fake-nsis-installer-bytes-v0.2.0".to_vec();
        let hex = sha256_hex(&payload);
        let sig = sign_for(&kp, "0.2.0", name, &hex);

        let mut routes = HashMap::new();
        routes.insert(format!("/{name}"), (200, payload.clone()));
        routes.insert(
            format!("/{name}.sha256"),
            (200, format!("{hex}  {name}\n").into_bytes()),
        );
        routes.insert(
            format!("/{name}.sig"),
            (200, format!("{sig}\n").into_bytes()),
        );
        let base = serve_once_map(routes);

        let dir = scratch();
        let saved = download_verified(
            dir.path(),
            &format!("{base}/{name}"),
            "0.2.0",
            &pubkey,
            true,
        )
        .expect("valid signed update should succeed");
        assert_eq!(std::fs::read(&saved).expect("read"), payload);
    }

    #[test]
    fn download_verified_rejects_missing_sig_missing_sha_wrong_key_replay_and_tamper() {
        let (kp, pubkey) = test_keypair();
        let (other_kp, _) = test_keypair();
        let name = "Zerith_0.2.0_x64-setup.exe";
        let good = b"good-installer".to_vec();
        let good_hex = sha256_hex(&good);
        let good_sig = sign_for(&kp, "0.2.0", name, &good_hex);

        // 1) Missing `.sig` (404) fails closed
        let mut r1 = HashMap::new();
        r1.insert(format!("/{name}"), (200, good.clone()));
        r1.insert(
            format!("/{name}.sha256"),
            (200, format!("{good_hex}  {name}\n").into_bytes()),
        );
        let b1 = serve_once_map(r1);
        let d1 = scratch();
        let err = download_verified(d1.path(), &format!("{b1}/{name}"), "0.2.0", &pubkey, true)
            .unwrap_err();
        assert!(err.contains(".sig"), "unexpected error: {err}");

        // 2) Missing `.sha256` (404) fails closed
        let mut r2 = HashMap::new();
        r2.insert(format!("/{name}"), (200, good.clone()));
        r2.insert(format!("/{name}.sig"), (200, good_sig.as_bytes().to_vec()));
        let b2 = serve_once_map(r2);
        let d2 = scratch();
        let err = download_verified(d2.path(), &format!("{b2}/{name}"), "0.2.0", &pubkey, true)
            .unwrap_err();
        assert!(err.contains(".sha256"), "unexpected error: {err}");

        // 3) Signed with wrong private key
        let wrong_sig = sign_for(&other_kp, "0.2.0", name, &good_hex);
        let mut r3 = HashMap::new();
        r3.insert(format!("/{name}"), (200, good.clone()));
        r3.insert(
            format!("/{name}.sha256"),
            (200, format!("{good_hex}  {name}\n").into_bytes()),
        );
        r3.insert(format!("/{name}.sig"), (200, wrong_sig.into_bytes()));
        let b3 = serve_once_map(r3);
        let d3 = scratch();
        let err = download_verified(d3.path(), &format!("{b3}/{name}"), "0.2.0", &pubkey, true)
            .unwrap_err();
        assert!(err.contains("Ed25519"), "unexpected error: {err}");

        // 4) Version mismatch (replay/downgrade: signed for 0.1.9, served as 0.2.0)
        let old_sig = sign_for(&kp, "0.1.9", name, &good_hex);
        let mut r4 = HashMap::new();
        r4.insert(format!("/{name}"), (200, good.clone()));
        r4.insert(
            format!("/{name}.sha256"),
            (200, format!("{good_hex}  {name}\n").into_bytes()),
        );
        r4.insert(format!("/{name}.sig"), (200, old_sig.into_bytes()));
        let b4 = serve_once_map(r4);
        let d4 = scratch();
        let err = download_verified(d4.path(), &format!("{b4}/{name}"), "0.2.0", &pubkey, true)
            .unwrap_err();
        assert!(err.contains("Ed25519"), "unexpected error: {err}");

        // 5) Tampered installer bytes (signature matches .sha256, but downloaded bytes differ)
        let mut r5 = HashMap::new();
        r5.insert(format!("/{name}"), (200, b"tampered-installer".to_vec()));
        r5.insert(
            format!("/{name}.sha256"),
            (200, format!("{good_hex}  {name}\n").into_bytes()),
        );
        r5.insert(format!("/{name}.sig"), (200, good_sig.into_bytes()));
        let b5 = serve_once_map(r5);
        let d5 = scratch();
        let err = download_verified(d5.path(), &format!("{b5}/{name}"), "0.2.0", &pubkey, true)
            .unwrap_err();
        assert!(err.contains("SHA-256"), "unexpected error: {err}");
        assert!(
            !d5.path()
                .join("updates")
                .join(format!("{name}.part"))
                .exists(),
            "tampered .part file must be deleted"
        );
    }
}
