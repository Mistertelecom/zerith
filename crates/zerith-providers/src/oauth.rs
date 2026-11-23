//! OAuth for the CLIs the user already pays for: Claude Code, Codex, Antigravity.
//!
//! Two ways in: import the credential a CLI already wrote to disk, or run the
//! loopback PKCE flow in the browser. Endpoints and client ids are the ones
//! those CLIs themselves use — no invented client, no proxy in the middle.
//!
//! Security: this module never logs nor stores a token. It returns
//! `OAuthCredential` and the caller puts it in the OS cofre.

use crate::config::{OAuthCredential, OAuthTool};
use base64::Engine as _;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("o provedor recusou: {0}")]
    Denied(String),
    #[error("state não confere")]
    State(String),
    #[error("porta {0} em uso; feche o outro processo e tente de novo")]
    Port(u16),
    #[error("http {0}: {1}")]
    Http(u16, String),
    #[error("resposta inesperada: {0}")]
    Parse(String),
    #[error("credencial sem refresh_token; rode `zerith login` de novo")]
    NoRefreshToken,
    /// The provider no longer accepts the refresh token (revoked, rotated by
    /// another login, or expired): only a new login brings the account back.
    #[error(
        "o login desta conta expirou ou foi revogado no provedor; entre de novo (Contas › Entrar)"
    )]
    LoginLost,
    #[error("io: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, OAuthError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Json,
    Form,
}

pub struct Provider {
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    /// `Some(porta)` only where the CLI fixes it (Codex 1455).
    pub fixed_port: Option<u16>,
    pub refresh_encoding: Encoding,
    /// Sent on the authorize URL only, verbatim.
    pub extra_authorize: &'static [(&'static str, &'static str)],
    /// The provider only accepts its own hosted callback and shows the code for
    /// the user to paste (Claude Code does this). `None` = loopback callback.
    pub manual_redirect: Option<&'static str>,
}

// Mirrors Claude Code 2.1.x (`an()` config in the CLI bundle): the hosts moved
// to platform.claude.com and the old console.anthropic.com callback now fails
// at consent with "Invalid request format".
const CLAUDE: Provider = Provider {
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    // The CLI bundle names claude.com/cai, but at runtime it opens claude.ai
    // directly; the redirect hop is where validation started failing.
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://platform.claude.com/v1/oauth/token",
    scopes: &[
        "org:create_api_key",
        "user:profile",
        "user:inference",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload",
    ],
    fixed_port: None,
    refresh_encoding: Encoding::Json,
    extra_authorize: &[("code", "true")],
    manual_redirect: Some(CLAUDE_MANUAL_REDIRECT),
};

const CODEX: Provider = Provider {
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scopes: &["openid", "profile", "email", "offline_access"],
    fixed_port: Some(1455),
    refresh_encoding: Encoding::Form,
    extra_authorize: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "codex_cli_rs"),
    ],
    manual_redirect: None,
};

const ANTIGRAVITY: Provider = Provider {
    // ponytail: o Google exige um client cadastrado e o id do CLI do Antigravity
    // não é público. O transporte e o refresh já funcionam com um token colado
    // no cofre; o login completo entra quando houver client próprio.
    client_id: "",
    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    scopes: &[
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ],
    fixed_port: None,
    refresh_encoding: Encoding::Form,
    extra_authorize: &[("access_type", "offline"), ("prompt", "consent")],
    manual_redirect: None,
};

/// The client id actually used. Antigravity has no public one, so it comes from
/// `ZERITH_GOOGLE_CLIENT_ID` — the flow refuses to start without it instead of
/// sending an empty client to Google.
pub fn client_id(tool: OAuthTool) -> String {
    if matches!(tool, OAuthTool::Antigravity) {
        return std::env::var("ZERITH_GOOGLE_CLIENT_ID")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
    }
    provider(tool).client_id.to_string()
}

/// Whether the tool has everything a browser login needs. Says what is missing
/// so the CLI can print it verbatim.
pub fn ready(tool: OAuthTool) -> std::result::Result<(), String> {
    if client_id(tool).is_empty() {
        return Err(format!(
            "{} não tem client OAuth público: defina ZERITH_GOOGLE_CLIENT_ID com o client id do seu projeto Google antes de entrar",
            tool.as_str()
        ));
    }
    Ok(())
}

pub fn provider(tool: OAuthTool) -> &'static Provider {
    match tool {
        OAuthTool::ClaudeCode => &CLAUDE,
        OAuthTool::Codex => &CODEX,
        OAuthTool::Antigravity => &ANTIGRAVITY,
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut raw = [0u8; N];
    SystemRandom::new()
        .fill(&mut raw)
        .expect("SystemRandom não falha nas plataformas suportadas");
    raw
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn from_verifier(v: &str) -> Self {
        let challenge = b64url(digest(&SHA256, v.as_bytes()).as_ref());
        Self {
            verifier: v.to_string(),
            challenge,
        }
    }

    pub fn generate() -> Self {
        Self::from_verifier(&b64url(&random_bytes::<32>()))
    }
}

/// 32 bytes (43 chars), like Claude Code: claude.ai answers "Invalid request
/// format" after consent to a shorter state (16 bytes was rejected).
pub fn random_state() -> String {
    b64url(&random_bytes::<32>())
}

pub fn redirect_path(tool: OAuthTool) -> &'static str {
    match tool {
        OAuthTool::Codex => "/auth/callback",
        _ => "/callback",
    }
}

/// Claude's registered callback. A loopback port is not accepted by the
/// client: the page shows a code (`code#state`) for the user to paste.
pub const CLAUDE_MANUAL_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";

pub fn redirect_uri(tool: OAuthTool, port: u16) -> String {
    if let Some(uri) = provider(tool).manual_redirect {
        return uri.to_string();
    }
    format!("http://127.0.0.1:{port}{}", redirect_path(tool))
}

/// Whether the login ends with a pasted code instead of a loopback callback.
pub fn needs_pasted_code(tool: OAuthTool) -> bool {
    provider(tool).manual_redirect.is_some()
}

/// Splits what Claude shows after consent: `code` or `code#state`.
pub fn pasted_code(raw: &str) -> (&str, Option<&str>) {
    let raw = raw.trim();
    match raw.split_once('#') {
        Some((code, state)) => (code.trim(), Some(state.trim())),
        None => (raw, None),
    }
}

/// Binds the loopback socket. Call it *before* opening the browser: a busy
/// fixed port must fail loudly, not after the user already consented.
pub fn bind(tool: OAuthTool) -> Result<(TcpListener, u16)> {
    let listener = match provider(tool).fixed_port {
        Some(port) => TcpListener::bind(("127.0.0.1", port)).map_err(|_| OAuthError::Port(port))?,
        None => TcpListener::bind("127.0.0.1:0").map_err(|e| OAuthError::Io(e.to_string()))?,
    };
    let port = listener
        .local_addr()
        .map_err(|e| OAuthError::Io(e.to_string()))?
        .port();
    Ok((listener, port))
}

pub fn authorize_url(tool: OAuthTool, pkce: &Pkce, state: &str, port: u16) -> String {
    let p = provider(tool);
    let mut url = format!(
        "{}?client_id={}&response_type=code&code_challenge={}&code_challenge_method=S256&state={}&redirect_uri={}&scope={}",
        p.authorize_url,
        urlencode(&client_id(tool)),
        urlencode(&pkce.challenge),
        urlencode(state),
        urlencode(&redirect_uri(tool, port)),
        urlencode(&p.scopes.join(" ")),
    );
    for (k, v) in p.extra_authorize {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencode(v));
    }
    url
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| urldecode(v))
    })
}

/// Blocks until the browser hits the callback. A wrong `state` or a stray
/// request gets a 400 and the loop keeps waiting; only a valid callback or an
/// explicit `error=` ends it.
pub fn wait_for_code(listener: TcpListener, path: &str, expect_state: &str) -> Result<String> {
    wait_for_code_within(listener, path, expect_state, LOGIN_WINDOW)
}

/// How long a login may take in the browser. Past it the port is released,
/// so a closed browser tab does not leave the next attempt stuck.
pub const LOGIN_WINDOW: Duration = Duration::from_secs(5 * 60);

fn wait_for_code_within(
    listener: TcpListener,
    path: &str,
    expect_state: &str,
    window: Duration,
) -> Result<String> {
    let deadline = std::time::Instant::now() + window;
    listener
        .set_nonblocking(true)
        .map_err(|e| OAuthError::Io(e.to_string()))?;
    loop {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            // WouldBlock, or one bad accept: neither ends the wait, but both
            // honour the deadline and back off instead of spinning.
            Err(_) => {
                if std::time::Instant::now() >= deadline {
                    return Err(OAuthError::Io(format!(
                        "o login não voltou em {} min; tente de novo",
                        window.as_secs() / 60
                    )));
                }
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
        }
        let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let (route, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        if route != path {
            respond(&mut stream, 404, "not here");
            continue;
        }
        if let Some(err) = query_param(query, "error") {
            respond(&mut stream, 400, "login recusado; pode fechar esta aba");
            return Err(OAuthError::Denied(err));
        }
        match query_param(query, "state") {
            Some(s) if constant_eq(&s, expect_state) => {}
            _ => {
                respond(&mut stream, 400, "state inválido");
                continue;
            }
        }
        let Some(code) = query_param(query, "code") else {
            respond(&mut stream, 400, "sem code");
            continue;
        };
        respond(&mut stream, 200, "login concluído; pode fechar esta aba");
        return Ok(code);
    }
}

/// Compares without an early exit, so the comparison time does not leak how
/// many characters of a guessed state were right.
fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn agent() -> ureq::Agent {
    crate::shared_http_agent(30_000)
}

fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<String> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    read_body(
        agent()
            .post(url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("Accept", "application/json")
            .send_string(&body),
    )
}

fn post_json(url: &str, body: serde_json::Value) -> Result<String> {
    read_body(
        agent()
            .post(url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(&body.to_string()),
    )
}

fn read_body(resp: std::result::Result<ureq::Response, ureq::Error>) -> Result<String> {
    match resp {
        Ok(r) => r.into_string().map_err(|e| OAuthError::Io(e.to_string())),
        Err(ureq::Error::Status(code, r)) => {
            Err(OAuthError::Http(code, r.into_string().unwrap_or_default()))
        }
        Err(e) => Err(OAuthError::Io(e.to_string())),
    }
}

pub fn exchange(
    tool: OAuthTool,
    code: &str,
    verifier: &str,
    redirect: &str,
    state: &str,
) -> Result<OAuthCredential> {
    let p = provider(tool);
    let body = match p.refresh_encoding {
        // Claude's token endpoint answers 400 "Invalid request format" when
        // `state` is missing from the body (checked live).
        Encoding::Json => post_json(
            p.token_url,
            serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "code_verifier": verifier,
                "redirect_uri": redirect,
                "client_id": client_id(tool),
                "state": state,
            }),
        )?,
        Encoding::Form => post_form(
            p.token_url,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("client_id", &client_id(tool)),
            ],
        )?,
    };
    credential_from_body(&body, p.refresh_encoding, tool, now_secs())
}

/// `%APPDATA%\9router\db\data.sqlite`, when 9router is installed.
pub fn ninerouter_db() -> Option<std::path::PathBuf> {
    let db = std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("9router/db/data.sqlite");
    db.exists().then_some(db)
}

pub fn refresh(tool: OAuthTool, cred: &OAuthCredential) -> Result<OAuthCredential> {
    // Antigravity's client secret is not public, so we cannot renew it
    // ourselves; 9router keeps its copy fresh, so re-read that one.
    if tool == OAuthTool::Antigravity {
        return ninerouter_db()
            .and_then(|db| import_from_9router(tool, &db))
            .filter(|c| c.expires_at > now_secs())
            .ok_or(OAuthError::NoRefreshToken);
    }
    let p = provider(tool);
    let refresh_token = cred
        .refresh_token
        .as_deref()
        .ok_or(OAuthError::NoRefreshToken)?;
    let body = match p.refresh_encoding {
        Encoding::Json => post_json(
            p.token_url,
            serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": client_id(tool),
            }),
        ),
        Encoding::Form => post_form(
            p.token_url,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", &client_id(tool)),
            ],
        ),
    }
    .map_err(login_lost)?;
    credential_from_body_keeping(&body, p.refresh_encoding, cred, now_secs())
}

/// OAuth's answer to a dead refresh token (RFC 6749 §5.2): `400` with
/// `invalid_grant`. Retrying cannot fix it, and the raw JSON says nothing to
/// the person: name the way out instead.
fn login_lost(e: OAuthError) -> OAuthError {
    match e {
        OAuthError::Http(400 | 401, body) if body.contains("invalid_grant") => {
            OAuthError::LoginLost
        }
        other => other,
    }
}

pub fn credential_from_body(
    body: &str,
    enc: Encoding,
    tool: OAuthTool,
    now: i64,
) -> Result<OAuthCredential> {
    let mut out = parse_token(body, enc)?;
    out.tool = tool;
    out.expires_at += now;
    Ok(out)
}

/// Google and Anthropic only send `refresh_token` on the first
/// authorization: a refresh response without one must keep the previous token.
fn credential_from_body_keeping(
    body: &str,
    enc: Encoding,
    old: &OAuthCredential,
    now: i64,
) -> Result<OAuthCredential> {
    let mut out = parse_token(body, enc)?;
    if out.refresh_token.is_none() {
        out.refresh_token = old.refresh_token.clone();
    }
    if out.account_id.is_none() {
        out.account_id = old.account_id.clone();
    }
    out.tool = old.tool;
    out.expires_at += now;
    Ok(out)
}

/// `expires_at` carries `expires_in` until the caller adds `now`.
/// RFC 6749 token endpoints return JSON even when the request body was
/// `application/x-www-form-urlencoded`, so JSON is always tried first and form
/// decoding is used only as a fallback. Error messages never echo raw token
/// bodies.
fn parse_token(body: &str, enc: Encoding) -> Result<OAuthCredential> {
    let parsed_json = serde_json::from_str::<serde_json::Value>(body).ok();
    let (access, refresh, expires_in) = if let Some(v) = &parsed_json {
        (
            v["access_token"].as_str().unwrap_or_default().to_string(),
            v["refresh_token"].as_str().map(str::to_string),
            v["expires_in"].as_i64().unwrap_or(3600),
        )
    } else if enc == Encoding::Form {
        (
            query_param(body, "access_token").unwrap_or_default(),
            query_param(body, "refresh_token"),
            query_param(body, "expires_in")
                .and_then(|s| s.parse().ok())
                .unwrap_or(3600),
        )
    } else {
        return Err(OAuthError::Parse(
            "resposta OAuth inválida (JSON esperado)".into(),
        ));
    };
    if access.is_empty() {
        // Extract only safe OAuth error fields; never echo raw response bodies
        // that could contain credentials or session material.
        let safe_msg = parsed_json
            .as_ref()
            .and_then(|v| {
                let err = v["error"].as_str();
                let desc = v["error_description"].as_str();
                match (err, desc) {
                    (Some(e), Some(d)) => Some(format!("{e}: {d}")),
                    (Some(e), None) => Some(e.to_string()),
                    (None, Some(d)) => Some(d.to_string()),
                    (None, None) => None,
                }
            })
            .or_else(|| {
                let err = query_param(body, "error");
                let desc = query_param(body, "error_description");
                match (err, desc) {
                    (Some(e), Some(d)) => Some(format!("{e}: {d}")),
                    (Some(e), None) => Some(e),
                    (None, Some(d)) => Some(d),
                    (None, None) => None,
                }
            })
            .unwrap_or_else(|| "resposta OAuth sem access_token".into());
        return Err(OAuthError::Parse(safe_msg));
    }
    Ok(OAuthCredential {
        access_token: access,
        refresh_token: refresh,
        expires_at: expires_in,
        account_id: parsed_json.as_ref().and_then(chatgpt_account_id),
        tool: OAuthTool::ClaudeCode,
        imported: Some(false),
    })
}

/// The ChatGPT account a Codex token response's `id_token` names. The JWT is
/// only decoded, not verified: it came straight from the token endpoint over
/// TLS, and the claim is only routing info for the `chatgpt-account-id` header.
fn chatgpt_account_id(v: &serde_json::Value) -> Option<String> {
    let payload = v["id_token"].as_str()?.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(|a| a.as_str())
        .map(str::to_string)
}

/// For a credential imported from a CLI's file: that file again, when the CLI
/// has since renewed it. Zerith never refreshes such a credential itself
/// (that would rotate the CLI's refresh token and log the CLI out).
pub fn reread(cred: &OAuthCredential) -> Option<OAuthCredential> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    reimport(cred, std::path::Path::new(&home))
}

/// Renews any credential: re-reads the CLI's file for an imported one,
/// refreshes Zerith's own.
pub fn renew(cred: &OAuthCredential) -> Result<OAuthCredential> {
    if cred.is_imported() {
        reread(cred).ok_or(OAuthError::NoRefreshToken)
    } else {
        refresh(cred.tool, cred)
    }
}

/// The CLI's file, when it holds a different, not older token for the same
/// account (Codex files carry no expiry, so the token itself must differ).
fn reimport(cred: &OAuthCredential, home: &std::path::Path) -> Option<OAuthCredential> {
    import_local(cred.tool, home).filter(|c| {
        c.access_token != cred.access_token
            && c.expires_at >= cred.expires_at
            && c.account_id == cred.account_id
    })
}

/// Validates that `url` uses an allowed web/mailto scheme with no control
/// characters before handing it to the OS handler.
fn validate_browser_url(url: &str) -> std::io::Result<()> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let ok_scheme = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:");
    let has_ctrl = u.chars().any(char::is_control);
    let has_ws = !lower.starts_with("mailto:") && u.chars().any(char::is_whitespace);
    if !ok_scheme || has_ctrl || has_ws {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("endereço recusado para abrir no navegador: {u}"),
        ));
    }
    Ok(())
}

/// Opens the URL in the user's default browser.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    validate_browser_url(url)?;
    browser_command(url.trim()).spawn().map(|_| ())
}

/// On Windows, no `cmd`: it splits on `&` (every OAuth URL has one) and
/// expands `%`, and std escapes embedded quotes as `\"`, which `cmd` does not
/// understand (`start` then got `\https://…` and Windows reported it missing).
/// `rundll32 url.dll,FileProtocolHandler` hands the URL straight to the shell's
/// protocol handler; the URL is percent-encoded, so it is one plain argument.
fn browser_command(url: &str) -> std::process::Command {
    let (program, lead): (&str, &[&str]) = if cfg!(target_os = "windows") {
        ("rundll32", &["url.dll,FileProtocolHandler"])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let mut c = std::process::Command::new(program);
    c.args(lead).arg(url);
    c
}

/// Antigravity: `groups[].buckets[]`, each with a `window` (`5h` / `weekly`),
/// a `remainingFraction` (1 = untouched) and an ISO `resetTime`.
fn parse_antigravity_quota(body: &str) -> Quota {
    let mut q = Quota::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return q;
    };
    let groups = v
        .get("groups")
        .or_else(|| v.get("quotaSummary").and_then(|s| s.get("groups")))
        .and_then(|g| g.as_array());
    let Some(groups) = groups else { return q };
    for group in groups {
        if let Some(name) = group["displayName"].as_str() {
            q.plan.get_or_insert_with(|| name.to_string());
        }
        let Some(buckets) = group["buckets"].as_array() else {
            continue;
        };
        for b in buckets {
            let window = b["window"].as_str().unwrap_or("").to_ascii_lowercase();
            let remaining = b["remainingFraction"].as_f64().unwrap_or(1.0);
            let entry = QuotaWindow {
                utilization: ((1.0 - remaining) * 100.0).max(0.0),
                resets_at: b["resetTime"].as_str().map(str::to_string),
                resets_at_epoch: None,
            };
            // the group ("Claude and GPT models") is the section title; the
            // bar itself only says which window, so it never truncates
            let group_name = group["displayName"].as_str().unwrap_or("Plano");
            let label = match window.as_str() {
                "5h" => "Sessão (5 h)".to_string(),
                "weekly" => "Semanal".to_string(),
                _ => b["displayName"].as_str().unwrap_or("Limite").to_string(),
            };
            q.buckets.push(bucket(group_name, label, &entry));
            match window.as_str() {
                "5h" => q.five_hour.get_or_insert(entry),
                "weekly" => q.seven_day.get_or_insert(entry),
                _ => continue,
            };
        }
    }
    // session before week, as Claude and Codex show them (sort is stable)
    q.buckets.sort_by_key(|b| b.label != "Sessão (5 h)");
    q
}

/// `fetchAvailableModels`: `models.<id>.quotaInfo.{remainingFraction, resetTime}`.
/// Internal/unnamed entries (`chat_20706`, `tab_*`) are not something a user
/// picks, so they are left out, as 9router does.
fn parse_antigravity_models(body: &str) -> Vec<QuotaBucket> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(models) = v.get("models").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<QuotaBucket> = models
        .iter()
        .filter(|(id, m)| !m["isInternal"].as_bool().unwrap_or(false) && !id.contains("disabled"))
        .filter_map(|(_, m)| {
            let name = m["displayName"].as_str()?;
            let qi = m.get("quotaInfo")?;
            let remaining = qi["remainingFraction"].as_f64().unwrap_or(1.0);
            Some(QuotaBucket {
                group: "Modelos".into(),
                label: name.to_string(), // id stays out: it only truncated the bar
                window: QuotaWindow {
                    utilization: ((1.0 - remaining) * 100.0).max(0.0),
                    resets_at: qi["resetTime"].as_str().map(str::to_string),
                    resets_at_epoch: None,
                },
            })
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    // `-tiered-high` carries the same name (and limit) as `-high`
    out.dedup_by(|a, b| a.label == b.label);
    out
}

/// One `v1internal` call. The `daily` host is the one that lists Gemini 4
/// (Hydrogen, Argon), and without an Antigravity user agent it answers 403
/// "no valid license".
fn antigravity_post(access_token: &str, method: &str) -> Result<String> {
    read_body(
        agent()
            .post(&format!(
                "{}/v1internal:{method}",
                crate::antigravity::BASE_URL
            ))
            .set("Authorization", &format!("Bearer {access_token}"))
            .set("Content-Type", "application/json")
            .set("User-Agent", crate::antigravity::USER_AGENT)
            .send_string("{}"),
    )
}

/// A model the account can chat with: the id the API takes, the name it shows.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
}

/// The models a subscription offers right now. Only Antigravity publishes a
/// live list; the others answer empty and the window keeps its catalogue.
pub fn fetch_models(tool: OAuthTool, access_token: &str) -> Result<Vec<ModelInfo>> {
    match tool {
        OAuthTool::Antigravity => Ok(parse_antigravity_model_list(&antigravity_post(
            access_token,
            "fetchAvailableModels",
        )?)),
        _ => Ok(Vec::new()),
    }
}

/// The app's own "Recommended" order first (Gemini 4 Hydrogen/Argon, Claude,
/// GPT-OSS), then every other named chat model. Internal, disabled and image
/// models stay out, and one name is listed once (`-tiered-high` repeats
/// `-high`).
fn parse_antigravity_model_list(body: &str) -> Vec<ModelInfo> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(models) = v["models"].as_object() else {
        return Vec::new();
    };
    let image: Vec<&str> = v["imageGenerationModelIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str())
        .collect();
    let recommended = v["agentModelSorts"][0]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["modelIds"].as_array().into_iter().flatten())
        .filter_map(|x| x.as_str());
    let mut ids: Vec<&str> = recommended.collect();
    let mut rest: Vec<&str> = models.keys().map(String::as_str).collect();
    rest.sort();
    ids.extend(rest);
    let mut out: Vec<ModelInfo> = Vec::new();
    for id in ids {
        let Some(m) = models.get(id) else { continue };
        let Some(name) = m["displayName"].as_str().map(str::trim) else {
            continue;
        };
        if m["isInternal"].as_bool().unwrap_or(false)
            || id.contains("disabled")
            || image.contains(&id)
            || out.iter().any(|o| o.id == id || o.name == name)
        {
            continue;
        }
        out.push(ModelInfo {
            id: id.to_string(),
            name: name.to_string(),
        });
    }
    out
}

/// `2026-11-17T23:34:03.322Z` → unix seconds. No chrono in this crate; days
/// since the epoch are enough for an expiry stamp.
pub fn iso_to_epoch(iso: &str) -> Option<i64> {
    let (date, rest) = iso.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = rest.trim_end_matches('Z').split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: f64 = t.next()?.parse().ok()?;
    // Howard Hinnant's days_from_civil
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss as i64)
}

/// The connection 9router already stores for this provider, read from its own
/// sqlite (`%APPDATA%/9router/db/data.sqlite`). Antigravity has no other way in:
/// its credential is DPAPI-sealed and its client secret is not public.
pub fn import_from_9router(tool: OAuthTool, db: &std::path::Path) -> Option<OAuthCredential> {
    let provider = match tool {
        OAuthTool::ClaudeCode => "claude",
        OAuthTool::Codex => "codex",
        OAuthTool::Antigravity => "antigravity",
    };
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let raw: String = conn
        .query_row(
            "select data from providerConnections where provider = ?1 and isActive = 1
             order by priority limit 1",
            [provider],
            |r| r.get(0),
        )
        .ok()?;
    parse_9router_credentials(tool, &raw)
}

/// A row copied from 9router's `providerConnections.data` (or hand-pasted):
/// `{accessToken, refreshToken, expiresAt, projectId?}` for every provider it
/// stores. That is the "copiar de lá" path.
pub fn parse_9router_credentials(tool: OAuthTool, raw: &str) -> Option<OAuthCredential> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let access = v
        .get("accessToken")
        .or_else(|| v.get("access_token"))
        .and_then(|t| t.as_str())?
        .to_string();
    let expires_at = match v.get("expiresAt").or_else(|| v.get("expires_at")) {
        Some(serde_json::Value::Number(n)) => n.as_i64().map(|ms| ms / 1000).unwrap_or(0),
        Some(serde_json::Value::String(s)) => iso_to_epoch(s).unwrap_or(0),
        _ => 0,
    };
    Some(OAuthCredential {
        access_token: access,
        refresh_token: v
            .get("refreshToken")
            .or_else(|| v.get("refresh_token"))
            .and_then(|t| t.as_str())
            .map(str::to_string),
        expires_at,
        // Antigravity bills against a project; every other provider has none
        account_id: v
            .get("projectId")
            .or_else(|| v.get("account_id"))
            .and_then(|t| t.as_str())
            .map(str::to_string),
        tool,
        imported: Some(false),
    })
}

/// One subscription window (5 h, weekly…), as the plan reports it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct QuotaWindow {
    /// 0..100, already a percentage.
    pub utilization: f64,
    /// RFC 3339, straight from the API.
    pub resets_at: Option<String>,
    /// Unix seconds, for the APIs that send it that way (Codex).
    pub resets_at_epoch: Option<i64>,
}

/// Who the token belongs to and how much of the plan is gone. This is what the
/// 9router usage page shows, from the same two endpoints.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Quota {
    pub email: Option<String>,
    pub plan: Option<String>,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    /// Every limit the plan reports, one bar each on the Cotas screen
    /// (Antigravity has one per model family, Claude splits Opus/Sonnet weekly).
    pub buckets: Vec<QuotaBucket>,
}

/// One named limit: "Semanal · Opus", "Claude Sonnet 5.5 (High)", …
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuotaBucket {
    /// Section the bar sits under ("Janelas", "Modelos").
    pub group: String,
    pub label: String,
    pub window: QuotaWindow,
}

fn bucket(group: &str, label: impl Into<String>, window: &QuotaWindow) -> QuotaBucket {
    QuotaBucket {
        group: group.to_string(),
        label: label.into(),
        window: window.clone(),
    }
}

/// Reads the plan quota for an OAuth credential: Claude and Codex usage
/// endpoints, Antigravity's quota summary plus one bar per model.
pub fn fetch_quota(tool: OAuthTool, access_token: &str) -> Result<Quota> {
    let get = |url: &str| -> Result<String> {
        read_body(
            agent()
                .get(url)
                .set("Authorization", &format!("Bearer {access_token}"))
                .set("anthropic-beta", "oauth-2025-04-20")
                .set("Accept", "application/json")
                .call(),
        )
    };
    match tool {
        OAuthTool::ClaudeCode => {
            let usage = get("https://api.anthropic.com/api/oauth/usage")?;
            // the profile is only a label; a failure there must not lose the numbers
            let profile = get("https://api.anthropic.com/api/oauth/profile").ok();
            Ok(parse_claude_quota(&usage, profile.as_deref()))
        }
        OAuthTool::Codex => Ok(parse_codex_quota(&get(
            "https://chatgpt.com/backend-api/wham/usage",
        )?)),
        OAuthTool::Antigravity => {
            let post = |method: &str| antigravity_post(access_token, method);
            let mut q = parse_antigravity_quota(&post("retrieveUserQuotaSummary")?);
            // per-model limits (9router's long list); a failure keeps the summary
            if let Ok(models) = post("fetchAvailableModels") {
                q.buckets.extend(parse_antigravity_models(&models));
            }
            Ok(q)
        }
    }
}

/// Codex: `primary_window` is the session, `secondary_window` the weekly one,
/// each with `percent_used` (already 0..100) and a reset timestamp.
fn parse_codex_quota(body: &str) -> Quota {
    let mut q = Quota::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return q;
    };
    // real body: the windows live under `rate_limit`
    let root = v.get("rate_limit").unwrap_or(&v);
    let window = |v: &serde_json::Value| -> Option<QuotaWindow> {
        let percent = v
            .get("used_percent")
            .and_then(|p| p.as_f64())
            .or_else(|| v.get("percent_used").and_then(|p| p.as_f64()))?;
        let reset = v.get("reset_at").or_else(|| v.get("resets_at"));
        Some(QuotaWindow {
            utilization: percent,
            resets_at: reset.and_then(|r| r.as_str()).map(str::to_string),
            resets_at_epoch: reset.and_then(|r| r.as_i64()),
        })
    };
    q.five_hour = root.get("primary_window").and_then(window);
    q.seven_day = root.get("secondary_window").and_then(window);
    if let Some(w) = &q.five_hour {
        q.buckets.push(bucket("Janelas", "Sessão (5 h)", w));
    }
    if let Some(w) = &q.seven_day {
        q.buckets.push(bucket("Janelas", "Semanal", w));
    }
    if let Some(w) = v
        .get("code_review_rate_limit")
        .and_then(|c| c.get("primary_window"))
        .and_then(window)
    {
        q.buckets.push(bucket("Janelas", "Code review", &w));
    }
    q.email = v["email"].as_str().map(str::to_string);
    q.plan = v["plan_type"].as_str().map(str::to_string);
    q
}

fn parse_claude_quota(usage: &str, profile: Option<&str>) -> Quota {
    let mut q = Quota::default();
    let window = |v: &serde_json::Value| -> Option<QuotaWindow> {
        Some(QuotaWindow {
            utilization: v.get("utilization")?.as_f64()?,
            resets_at: v
                .get("resets_at")
                .and_then(|r| r.as_str())
                .map(str::to_string),
            resets_at_epoch: None,
        })
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(usage) {
        q.five_hour = v.get("five_hour").and_then(window);
        q.seven_day = v.get("seven_day").and_then(window);
        for (key, label) in [
            ("five_hour", "Sessão (5 h)"),
            ("seven_day", "Semanal"),
            ("seven_day_opus", "Semanal · Opus"),
            ("seven_day_sonnet", "Semanal · Sonnet"),
            ("seven_day_oauth_apps", "Semanal · apps OAuth"),
        ] {
            if let Some(w) = v.get(key).and_then(window) {
                q.buckets.push(bucket("Janelas", label, &w));
            }
        }
    }
    if let Some(p) = profile.and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok()) {
        q.email = p["account"]["email"].as_str().map(str::to_string);
        q.plan = p["organization"]["organization_type"]
            .as_str()
            .map(str::to_string);
    }
    q
}

/// Credential a CLI already wrote to disk, if any. Read-only: never rewrites
/// another tool's file.
pub fn import_local(tool: OAuthTool, home: &std::path::Path) -> Option<OAuthCredential> {
    match tool {
        OAuthTool::Codex => import_codex(home),
        OAuthTool::ClaudeCode => import_claude(home),
        OAuthTool::Antigravity => None, // DPAPI; only the own flow can read it
    }
}

fn import_codex(home: &std::path::Path) -> Option<OAuthCredential> {
    let raw = std::fs::read_to_string(home.join(".codex/auth.json")).ok()?;
    parse_codex_credentials(&raw)
}

/// The same `auth.json`, pasted: another ChatGPT subscription without logging
/// the CLI out of the first one.
pub fn parse_codex_credentials(raw: &str) -> Option<OAuthCredential> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let tokens = v.get("tokens")?;
    let access = tokens.get("access_token")?.as_str()?.to_string();
    Some(OAuthCredential {
        access_token: access,
        refresh_token: tokens
            .get("refresh_token")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        // the file has no `expires_in`: treat as expired so the refresh decides
        expires_at: 0,
        account_id: tokens
            .get("account_id")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        tool: OAuthTool::Codex,
        // the `codex` CLI owns this rotating refresh token, file or pasted
        imported: Some(true),
    })
}

fn import_claude(home: &std::path::Path) -> Option<OAuthCredential> {
    let raw = std::fs::read_to_string(home.join(".claude/.credentials.json")).ok()?;
    // Only the file import is marked: a pasted credential has no file to
    // re-read, so it keeps being refreshed by Zerith.
    parse_claude_credentials(&raw).map(|c| OAuthCredential {
        imported: Some(true),
        ..c
    })
}

/// The same file, pasted by the user: that is how a second subscription gets
/// in without logging the CLI out of the first one.
pub fn parse_claude_credentials(raw: &str) -> Option<OAuthCredential> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    // a file with only `mcpOAuth` means no subscription login was ever made
    let oauth = v.get("claudeAiOauth")?;
    let access = oauth
        .get("accessToken")
        .and_then(|t| t.as_str())?
        .to_string();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(|t| t.as_i64())
        .map(|ms| ms / 1000)
        .unwrap_or(0);
    Some(OAuthCredential {
        access_token: access,
        refresh_token: oauth
            .get("refreshToken")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        expires_at,
        account_id: None,
        tool: OAuthTool::ClaudeCode,
        imported: Some(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writable scratch dir. `tempfile` is not used because the sandbox
    /// denies writes to `%LOCALAPPDATA%\Temp`; `target/` always works.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("oauth-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// RFC 7636, Appendix B.
    #[test]
    fn pkce_matches_the_rfc_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_url_safe_and_long_enough() {
        let p = Pkce::generate();
        assert!((43..=128).contains(&p.verifier.len()));
        assert!(p
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(p.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn authorize_url_carries_pkce_state_scopes_and_redirect() {
        let p = Pkce::from_verifier("v");
        // Codex still ends on the loopback callback; Claude moved to the
        // hosted one (see claude_uses_its_own_callback_and_asks_for_the_code).
        let url = authorize_url(OAuthTool::Codex, &p, "st-1", 4242);
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st-1"));
        assert!(url.contains("offline_access"));
        assert!(url.contains("127.0.0.1%3A4242%2Fauth%2Fcallback"));
        assert!(!url.contains("code_verifier="));
    }

    #[test]
    fn codex_url_adds_the_cli_flow_params() {
        let p = Pkce::from_verifier("v");
        let url = authorize_url(OAuthTool::Codex, &p, "s", 1455);
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("127.0.0.1%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn bind_gives_a_loopback_port_and_codex_uses_1455() {
        let (_l, port) = bind(OAuthTool::ClaudeCode).unwrap();
        assert_ne!(port, 0);
        // 1455 may be taken on a dev box; the contract is the error, not success.
        match bind(OAuthTool::Codex) {
            Ok((_l, p)) => assert_eq!(p, 1455),
            Err(OAuthError::Port(1455)) => {}
            other => panic!("esperava 1455 ou Port(1455), veio {other:?}"),
        }
    }

    #[test]
    fn listener_ignores_a_wrong_state_then_returns_the_code() {
        let (listener, port) = bind(OAuthTool::ClaudeCode).unwrap();
        let handle = std::thread::spawn(move || wait_for_code(listener, "/callback", "st-1"));
        let _ = ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?code=bad&state=nope"
        ))
        .timeout(Duration::from_secs(5))
        .call();
        assert!(ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?code=good&state=st-1"
        ))
        .call()
        .is_ok());
        assert_eq!(handle.join().unwrap().unwrap(), "good");
    }

    #[test]
    fn a_denied_callback_is_an_error_not_a_code() {
        let (listener, port) = bind(OAuthTool::ClaudeCode).unwrap();
        let handle = std::thread::spawn(move || wait_for_code(listener, "/callback", "st-2"));
        let _ = ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?error=access_denied&state=st-2"
        ))
        .call();
        assert!(matches!(handle.join().unwrap(), Err(OAuthError::Denied(_))));
    }

    #[test]
    fn an_abandoned_login_gives_the_port_back() {
        let (listener, _) = bind(OAuthTool::ClaudeCode).unwrap();
        let e = wait_for_code_within(listener, "/callback", "st", Duration::from_millis(300))
            .unwrap_err();
        assert!(e.to_string().contains("não voltou"), "{e}");
    }

    #[test]
    fn antigravity_quota_turns_remaining_fraction_into_used_percent() {
        let body = r#"{"groups":[{"displayName":"Claude and GPT models","buckets":[
            {"bucketId":"3p-weekly","window":"weekly","resetTime":"2026-11-22T20:03:15Z","remainingFraction":0.997662},
            {"bucketId":"3p-5h","window":"5h","resetTime":"2026-11-18T03:53:46Z","remainingFraction":1}]}]}"#;
        let q = parse_antigravity_quota(body);
        assert_eq!(q.plan.as_deref(), Some("Claude and GPT models"));
        assert!((q.seven_day.clone().unwrap().utilization - 0.2338).abs() < 0.001);
        assert_eq!(q.five_hour.unwrap().utilization, 0.0);
        assert_eq!(
            q.seven_day.unwrap().resets_at.as_deref(),
            Some("2026-11-22T20:03:15Z")
        );
    }

    #[test]
    fn antigravity_models_become_named_bars_and_internal_ones_are_skipped() {
        let body = r#"{"models":{
            "claude-sonnet-5-5-high":{"displayName":"Claude Sonnet 5.5 (High)","quotaInfo":{"remainingFraction":0.25,"resetTime":"2026-11-18T04:13:22Z"}},
            "chat_20706":{"quotaInfo":{"remainingFraction":1}},
            "x":{"displayName":"Interno","isInternal":true,"quotaInfo":{"remainingFraction":1}}}}"#;
        let b = parse_antigravity_models(body);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].group, "Modelos");
        assert_eq!(b[0].window.utilization, 75.0);
        assert!(b[0].label.starts_with("Claude Sonnet 5.5 (High)"));
    }

    #[test]
    fn antigravity_model_list_keeps_the_recommended_order_and_drops_noise() {
        let body = r#"{"models":{
            "gemini-4-hydrogen-high":{"displayName":"Gemini 4 Hydrogen (High)"},
            "gemini-4-hydrogen-tiered-high":{"displayName":"Gemini 4 Hydrogen (High)"},
            "gemini-4-carbon-disabled":{"displayName":"Gemini 4 Carbon"},
            "gemini-3.1-flash-image":{"displayName":"Gemini 3.1 Flash Image"},
            "chat_20706":{"isInternal":true},
            "a-model":{"displayName":"A"},
            "gpt-oss-120b-medium":{"displayName":"GPT-OSS 120B (Medium) "}},
            "imageGenerationModelIds":["gemini-3.1-flash-image"],
            "agentModelSorts":[{"groups":[{"modelIds":["gemini-4-hydrogen-high","gpt-oss-120b-medium","gemini-4-carbon-disabled"]}]}]}"#;
        let ids: Vec<String> = parse_antigravity_model_list(body)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            ids,
            ["gemini-4-hydrogen-high", "gpt-oss-120b-medium", "a-model"]
        );
    }

    #[test]
    fn iso_and_9router_rows_parse() {
        assert_eq!(iso_to_epoch("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(iso_to_epoch("2026-11-17T23:34:03.322Z"), Some(1794958443));
        let row = r#"{"accessToken":"ya29.x","refreshToken":"1//0hg","expiresAt":"2026-11-17T23:34:03Z","projectId":"aicode-consumers"}"#;
        let c = parse_9router_credentials(OAuthTool::Antigravity, row).unwrap();
        assert_eq!(c.tool, OAuthTool::Antigravity);
        assert_eq!(c.account_id.as_deref(), Some("aicode-consumers"));
        assert_eq!(c.expires_at, 1794958443);
    }

    #[test]
    fn codex_quota_reads_percent_used_and_the_reset() {
        // shape taken from the live `wham/usage` body
        let body = r#"{"email":"a@b.c","plan_type":"plus","rate_limit":{
            "primary_window":{"used_percent":0,"reset_at":1794966836},
            "secondary_window":{"used_percent":100,"reset_at":1794974842}}}"#;
        let q = parse_codex_quota(body);
        assert_eq!(q.email.as_deref(), Some("a@b.c"));
        assert_eq!(q.plan.as_deref(), Some("plus"));
        assert_eq!(q.five_hour.unwrap().utilization, 0.0);
        let week = q.seven_day.unwrap();
        assert_eq!(week.utilization, 100.0);
        assert_eq!(week.resets_at_epoch, Some(1794974842));
        // an empty body is not an error, it is "no window"
        assert_eq!(parse_codex_quota("{}"), Quota::default());
    }

    #[test]
    fn quota_reads_the_two_windows_and_the_plan_label() {
        let usage = r#"{"five_hour":{"utilization":0.0,"resets_at":null},
            "seven_day":{"utilization":81.0,"resets_at":"2026-11-22T07:59:59+00:00"}}"#;
        let profile = r#"{"account":{"email":"a@b.c"},
            "organization":{"organization_type":"claude_pro"}}"#;
        let q = parse_claude_quota(usage, Some(profile));
        assert_eq!(q.email.as_deref(), Some("a@b.c"));
        assert_eq!(q.plan.as_deref(), Some("claude_pro"));
        assert_eq!(
            q.five_hour,
            Some(QuotaWindow {
                utilization: 0.0,
                resets_at: None,
                resets_at_epoch: None
            })
        );
        assert_eq!(q.seven_day.unwrap().utilization, 81.0);
        // a broken profile still leaves the numbers
        assert_eq!(
            parse_claude_quota(usage, Some("lixo"))
                .seven_day
                .unwrap()
                .utilization,
            81.0
        );
    }

    #[test]
    fn claude_uses_its_own_callback_and_asks_for_the_code() {
        let pkce = Pkce::generate();
        let url = authorize_url(OAuthTool::ClaudeCode, &pkce, "st", 45999);
        assert!(url.contains("code=true"), "{url}");
        assert!(
            url.contains(
                "redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback"
            ),
            "{url}"
        );
        assert!(!url.contains("127.0.0.1"), "{url}");
        assert!(needs_pasted_code(OAuthTool::ClaudeCode));
        assert!(!needs_pasted_code(OAuthTool::Codex));
    }

    #[test]
    fn the_state_is_as_long_as_claude_codes() {
        assert_eq!(random_state().len(), 43);
    }

    #[test]
    fn a_pasted_code_may_carry_the_state() {
        assert_eq!(pasted_code("abc#st1"), ("abc", Some("st1")));
        assert_eq!(pasted_code("  abc  "), ("abc", None));
    }

    #[test]
    fn a_wrong_path_does_not_satisfy_the_wait() {
        let (listener, port) = bind(OAuthTool::ClaudeCode).unwrap();
        let handle = std::thread::spawn(move || wait_for_code(listener, "/callback", "st-3"));
        let _ = ureq::get(&format!("http://127.0.0.1:{port}/favicon.ico")).call();
        assert!(ureq::get(&format!(
            "http://127.0.0.1:{port}/callback?code=c3&state=st-3"
        ))
        .call()
        .is_ok());
        assert_eq!(handle.join().unwrap().unwrap(), "c3");
    }

    #[test]
    fn token_response_parses_json_and_form_encodings() {
        let now = 1_700_000_000;
        let json = r#"{"access_token":"A","refresh_token":"R","expires_in":3600}"#;
        let c = credential_from_body(json, Encoding::Json, OAuthTool::ClaudeCode, now).unwrap();
        assert_eq!(c.access_token, "A");
        assert_eq!(c.expires_at, now + 3600);
        assert_eq!(c.refresh_token.as_deref(), Some("R"));

        let form = "access_token=A&refresh_token=R&expires_in=60";
        let c = credential_from_body(form, Encoding::Form, OAuthTool::Codex, now).unwrap();
        assert_eq!(c.tool, OAuthTool::Codex);
        assert_eq!(c.expires_at, now + 60);
    }

    /// The quota screen showed `http 400: {"error": "invalid_grant", ...}` raw
    /// and kept asking: a dead refresh token is a login to redo, said plainly.
    #[test]
    fn a_dead_refresh_token_reads_as_login_lost() {
        let raw = r#"{"error": "invalid_grant", "error_description": "Refresh token not found or invalid"}"#;
        let e = login_lost(OAuthError::Http(400, raw.into()));
        assert!(matches!(e, OAuthError::LoginLost), "{e}");
        let msg = e.to_string();
        assert!(msg.contains("entre de novo"), "{msg}");
        assert!(!msg.contains("invalid_grant"), "{msg}");
        // other failures keep their own message (a network error is not a lost login)
        assert!(matches!(
            login_lost(OAuthError::Http(500, "x".into())),
            OAuthError::Http(500, _)
        ));
        assert!(matches!(
            login_lost(OAuthError::Io("timeout".into())),
            OAuthError::Io(_)
        ));
    }

    #[test]
    fn an_error_body_is_not_mistaken_for_a_token() {
        let e = credential_from_body(
            r#"{"error":"invalid_grant"}"#,
            Encoding::Json,
            OAuthTool::ClaudeCode,
            0,
        );
        assert!(matches!(e, Err(OAuthError::Parse(_))));
    }

    #[test]
    fn a_refresh_without_refresh_token_keeps_the_previous_one() {
        let old = OAuthCredential {
            access_token: "old".into(),
            refresh_token: Some("keep".into()),
            expires_at: 10,
            account_id: Some("acc".into()),
            tool: OAuthTool::Antigravity,
            imported: Some(false),
        };
        let body = r#"{"access_token":"new","expires_in":3600}"#;
        let c = credential_from_body_keeping(body, Encoding::Json, &old, 0).unwrap();
        assert_eq!(c.access_token, "new");
        assert_eq!(c.refresh_token.as_deref(), Some("keep"));
        assert_eq!(c.account_id.as_deref(), Some("acc"));
        assert_eq!(c.tool, OAuthTool::Antigravity);
    }

    #[test]
    fn importing_codex_credentials_reads_the_local_file() {
        let dir = scratch("codex");
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(
            dir.join(".codex/auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"AT","refresh_token":"RT","account_id":"acc_1"},"last_refresh":"2026-11-08T00:00:00Z"}"#,
        )
        .unwrap();
        let c = import_local(OAuthTool::Codex, &dir).expect("deve importar");
        assert_eq!(c.access_token, "AT");
        assert_eq!(c.refresh_token.as_deref(), Some("RT"));
        assert_eq!(c.account_id.as_deref(), Some("acc_1"));
        assert_eq!(c.tool, OAuthTool::Codex);
        assert!(c.is_imported());
    }

    #[test]
    fn a_codex_browser_login_reads_the_account_from_the_id_token() {
        let claims = r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc_jwt"}}"#;
        let jwt = format!("h.{}.sig", b64url(claims.as_bytes()));
        let body = format!(r#"{{"access_token":"AT","refresh_token":"RT","id_token":"{jwt}"}}"#);
        let c = credential_from_body(&body, Encoding::Form, OAuthTool::Codex, 0).unwrap();
        assert_eq!(c.account_id.as_deref(), Some("acc_jwt"));
        assert!(
            !c.is_imported(),
            "a browser login is Zerith's own: refreshable"
        );
    }

    #[test]
    fn an_imported_claude_credential_is_re_read_not_refreshed() {
        let dir = scratch("claude-reread");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let write = |ms: i64| {
            std::fs::write(
                dir.join(".claude/.credentials.json"),
                format!(r#"{{"claudeAiOauth":{{"accessToken":"T{ms}","refreshToken":"R","expiresAt":{ms}}}}}"#),
            )
            .unwrap()
        };
        write(1_000_000);
        let old = import_local(OAuthTool::ClaudeCode, &dir).unwrap();
        assert!(old.is_imported());
        assert!(
            parse_claude_credentials(r#"{"claudeAiOauth":{"accessToken":"x"}}"#)
                .is_some_and(|c| !c.is_imported())
        );
        assert!(reimport(&old, &dir).is_none(), "same token is not newer");
        write(2_000_000);
        assert_eq!(reimport(&old, &dir).unwrap().access_token, "T2000000");
    }

    #[test]
    fn importing_claude_ignores_a_file_with_only_mcp_tokens() {
        let dir = scratch("claude-mcp");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/.credentials.json"),
            r#"{"mcpOAuth":{"x":{"accessToken":"mcp"}}}"#,
        )
        .unwrap();
        assert!(import_local(OAuthTool::ClaudeCode, &dir).is_none());
    }

    #[test]
    fn importing_claude_reads_the_subscription_block_and_scales_ms_to_seconds() {
        let dir = scratch("claude-sub");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/.credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"CA","refreshToken":"CR","expiresAt":1767225600000}}"#,
        )
        .unwrap();
        let c = import_local(OAuthTool::ClaudeCode, &dir).unwrap();
        assert_eq!(c.access_token, "CA");
        assert_eq!(c.expires_at, 1_767_225_600);
    }

    #[test]
    fn antigravity_has_no_local_import() {
        let dir = scratch("antig");
        assert!(import_local(OAuthTool::Antigravity, &dir).is_none());
    }

    /// Antigravity has no public client: the flow must refuse with the variable
    /// name instead of sending an empty client to Google.
    #[test]
    fn antigravity_refuses_to_start_without_a_client_id() {
        if std::env::var("ZERITH_GOOGLE_CLIENT_ID").is_ok() {
            return; // a dev box with the variable set exercises the other branch
        }
        let e = ready(OAuthTool::Antigravity).unwrap_err();
        assert!(e.contains("ZERITH_GOOGLE_CLIENT_ID"), "{e}");
        assert!(ready(OAuthTool::ClaudeCode).is_ok());
        assert!(ready(OAuthTool::Codex).is_ok());
    }

    /// The authorize URL reaches the opener as one untouched argument: no
    /// quotes added (std would escape them as `\"`), no shell in between.
    #[test]
    fn the_browser_gets_the_url_verbatim() {
        let url = authorize_url(OAuthTool::ClaudeCode, &Pkce::from_verifier("v"), "s", 4242);
        assert!(
            url.contains('&') && !url.contains(['"', ' ', '\\']),
            "{url}"
        );
        let c = browser_command(&url);
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args.last().unwrap().to_str(), Some(url.as_str()));
        if cfg!(target_os = "windows") {
            assert_eq!(c.get_program(), "rundll32");
        }
    }

    #[test]
    fn form_encoding_endpoints_parse_json_responses_and_errors_never_leak_tokens() {
        let json_resp =
            r#"{"access_token":"tok_live_123","refresh_token":"ref_456","expires_in":7200}"#;
        let cred = credential_from_body(json_resp, Encoding::Form, OAuthTool::Codex, 1000)
            .expect("Codex/Antigravity JSON responses must parse even when request used Form");
        assert_eq!(cred.access_token, "tok_live_123");
        assert_eq!(cred.refresh_token.as_deref(), Some("ref_456"));
        assert_eq!(cred.expires_at, 8200);

        let err_resp = r#"{"error":"invalid_grant","error_description":"expired","secret":"super_secret_should_not_leak"}"#;
        let err = credential_from_body(err_resp, Encoding::Form, OAuthTool::Codex, 1000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid_grant"), "{err}");
        assert!(!err.contains("super_secret_should_not_leak"), "{err}");

        assert!(validate_browser_url("https://example.com/auth").is_ok());
        assert!(validate_browser_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_browser_url("ms-settings:privacy").is_err());
        assert!(validate_browser_url("https://example.com/\r\nbad").is_err());
    }
}
