//! OAuth 2.1 for remote (HTTP) MCP servers, as the MCP authorization spec asks.
//!
//! Discovery: the server's 401 names its metadata (`WWW-Authenticate:
//! resource_metadata=`, RFC 9728), else the well-known path; that document
//! names the authorization server, whose metadata comes from RFC 8414 (or
//! OIDC). No metadata at all = an older server: its origin is the AS with the
//! default `/authorize`, `/token`, `/register`.
//!
//! Then Dynamic Client Registration (RFC 7591), authorization code + PKCE S256
//! + `state` + `resource` (RFC 8707) on a loopback redirect, token exchange.
//!
//! Secrets (tokens, a client secret) live only in the OS vault, chunked
//! (Windows caps one credential at ~1280 chars). `<root>/mcp-oauth/<name>.json`
//! keeps the non-secret client and AS metadata so the next login can reuse the
//! registration. The token entry also carries what a refresh needs (endpoint,
//! client id), so the runtime reads the vault only. No error string here ever
//! carries a token, a code or a secret.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zerith_core::secrets::{OsKeyring, SecretError, SecretStore};
use zerith_providers::oauth::{now_secs, random_state, wait_for_code, Pkce};

pub const EXPIRED: &str = "Sessão expirada — clique em Entrar";
pub const NOT_CONNECTED: &str = "Não conectado — clique em Entrar";
/// Refresh this long before the server's expiry.
const SKEW: i64 = 60;
const CALLBACK: &str = "/callback";
const CHUNK: usize = 1024;

type Res<T> = std::result::Result<T, String>;

// ------------------------------------------------------------------ vault

fn token_key(name: &str) -> String {
    format!("mcp.{name}.oauth")
}

fn client_key(name: &str) -> String {
    format!("mcp.{name}.client")
}

/// `key` holds the chunk count, `key.N` the pieces.
fn put(store: &dyn SecretStore, key: &str, value: &str) -> Res<()> {
    del(store, key)?;
    let chars: Vec<char> = value.chars().collect();
    let mut n = 0;
    for c in chars.chunks(CHUNK) {
        store
            .set(&format!("{key}.{n}"), &c.iter().collect::<String>())
            .map_err(|e| e.to_string())?;
        n += 1;
    }
    store.set(key, &n.to_string()).map_err(|e| e.to_string())
}

fn get(store: &dyn SecretStore, key: &str) -> Res<Option<String>> {
    let n: usize = match store.get(key) {
        Ok(h) => h
            .trim()
            .parse()
            .map_err(|_| format!("cofre: entrada {key} inválida"))?,
        Err(SecretError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    (0..n)
        .map(|i| store.get(&format!("{key}.{i}")).map_err(|e| e.to_string()))
        .collect::<Res<String>>()
        .map(Some)
}

fn del(store: &dyn SecretStore, key: &str) -> Res<()> {
    if let Ok(Ok(n)) = store.get(key).map(|h| h.trim().parse::<usize>()) {
        for i in 0..n {
            let _ = store.delete(&format!("{key}.{i}"));
        }
    }
    match store.delete(key) {
        Ok(()) | Err(SecretError::NotFound(_)) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// What the vault keeps per server: the tokens plus what a refresh needs.
#[derive(Clone, Serialize, Deserialize)]
struct Token {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    /// Unix seconds; 0 when the server did not say.
    #[serde(default)]
    expires_at: i64,
    token_endpoint: String,
    client_id: String,
    auth_method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource: Option<String>,
}

impl Token {
    /// This token updated from a token-endpoint answer. A refresh answer
    /// without `refresh_token` keeps the old one.
    fn renewed(&self, v: &Value) -> Option<Token> {
        let access = v["access_token"].as_str().filter(|s| !s.is_empty())?;
        let expires_in = v["expires_in"]
            .as_i64()
            .or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok()));
        Some(Token {
            access_token: access.to_string(),
            refresh_token: v["refresh_token"]
                .as_str()
                .map(str::to_string)
                .or_else(|| self.refresh_token.clone()),
            expires_at: expires_in.map(|s| now_secs() + s).unwrap_or(0),
            ..self.clone()
        })
    }
}

fn load(store: &dyn SecretStore, name: &str) -> Res<Option<Token>> {
    match get(store, &token_key(name))? {
        Some(j) => serde_json::from_str(&j)
            .map(Some)
            .map_err(|_| "cofre: credencial MCP corrompida; clique em Entrar".to_string()),
        None => Ok(None),
    }
}

fn save(store: &dyn SecretStore, name: &str, t: &Token) -> Res<()> {
    put(
        store,
        &token_key(name),
        &serde_json::to_string(t).map_err(|e| e.to_string())?,
    )
}

/// Whether the person still has to click "Entrar".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    /// Unix seconds of the access token's expiry, when the server said.
    pub expires_at: Option<i64>,
}

pub fn status(store: &dyn SecretStore, name: &str) -> Status {
    match load(store, name).ok().flatten() {
        Some(t) => Status {
            connected: t.refresh_token.is_some() || t.expires_at == 0 || t.expires_at > now_secs(),
            expires_at: (t.expires_at > 0).then_some(t.expires_at),
        },
        None => Status {
            connected: false,
            expires_at: None,
        },
    }
}

/// "Sair": drops the tokens, keeps the registered client for the next login.
pub fn logout(store: &dyn SecretStore, name: &str) -> Res<()> {
    del(store, &token_key(name))
}

/// The server was removed: tokens, client secret and client file go too.
pub fn forget(root: &Path, store: &dyn SecretStore, name: &str) -> Res<()> {
    del(store, &token_key(name))?;
    del(store, &client_key(name))?;
    match std::fs::remove_file(client_file(root, name)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

// ------------------------------------------------------------------- http

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build()
}

/// `application/x-www-form-urlencoded` / query value encoding.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A JSON object from `url`, or `None` (404, not JSON, unreachable): the
/// caller tries the next candidate.
fn get_json(url: &str) -> Option<Value> {
    let v: Value = agent()
        .get(url)
        .set("Accept", "application/json")
        .call()
        .ok()?
        .into_json()
        .ok()?;
    v.is_object().then_some(v)
}

/// An error body as one line: the OAuth `error`/`error_description` when
/// present, else the start of the body; known secrets are blanked either way.
fn describe(code: u16, body: &str, secrets: &[&str]) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let mut out = match (v["error"].as_str(), v["error_description"].as_str()) {
        (Some(e), Some(d)) => format!("{e}: {d}"),
        (Some(e), None) => e.to_string(),
        _ => format!(
            "http {code}: {}",
            body.chars().take(160).collect::<String>()
        ),
    };
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(s, "***");
    }
    out
}

// -------------------------------------------------------------- discovery

/// `https://h:1/a/b?q` → (`https://h:1`, `/a/b`); a bare `/` path is empty.
fn split(url: &str) -> (String, String) {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let start = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[start..].find('/') {
        Some(i) => (
            url[..start + i].to_string(),
            url[start + i..].trim_end_matches('/').to_string(),
        ),
        None => (url.to_string(), String::new()),
    }
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// RFC 9728 §3.1: path-suffixed first, then the origin's.
fn resource_candidates(mcp_url: &str) -> Vec<String> {
    let (o, p) = split(mcp_url);
    dedup(vec![
        format!("{o}/.well-known/oauth-protected-resource{p}"),
        format!("{o}/.well-known/oauth-protected-resource"),
    ])
}

/// RFC 8414 §3 (path inserted after the well-known part), then OIDC both ways.
fn issuer_candidates(issuer: &str) -> Vec<String> {
    let (o, p) = split(issuer);
    dedup(vec![
        format!("{o}/.well-known/oauth-authorization-server{p}"),
        format!("{o}/.well-known/openid-configuration{p}"),
        format!("{o}{p}/.well-known/openid-configuration"),
    ])
}

/// One `key=value` / `key="value"` parameter of a `WWW-Authenticate` header.
fn auth_param(header: &str, key: &str) -> Option<String> {
    let mut at = 0;
    while let Some(i) = header[at..].find(key).map(|i| i + at) {
        at = i + key.len();
        let boundary = header[..i]
            .chars()
            .last()
            .is_none_or(|c| c == ' ' || c == ',');
        let Some(v) = header[at..].strip_prefix('=').filter(|_| boundary) else {
            continue;
        };
        return Some(match v.strip_prefix('"') {
            Some(q) => q.split('"').next().unwrap_or("").to_string(),
            None => v.split([',', ' ']).next().unwrap_or("").to_string(),
        });
    }
    None
}

/// An unauthenticated `initialize`: the 401 names the metadata and scopes.
fn probe(mcp_url: &str) -> (Option<String>, Option<String>) {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": crate::PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "zerith", "version": zerith_core::VERSION },
        },
    });
    match agent()
        .post(mcp_url)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .send_string(&body.to_string())
    {
        Err(ureq::Error::Status(401, r)) => {
            let h = r.header("WWW-Authenticate").unwrap_or("").to_string();
            (
                auth_param(&h, "resource_metadata"),
                auth_param(&h, "scope").filter(|s| !s.is_empty()),
            )
        }
        _ => (None, None),
    }
}

/// Where to log in, found from the MCP URL alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// RFC 8707 `resource`: the protected resource's own id, else the URL.
    pub resource: String,
    pub scope: Option<String>,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
}

pub fn discover(mcp_url: &str) -> Res<Discovery> {
    let (meta, scope) = probe(mcp_url);
    let prm = meta
        .into_iter()
        .chain(resource_candidates(mcp_url))
        .find_map(|u| get_json(&u));
    let issuer = prm
        .as_ref()
        .and_then(|v| v["authorization_servers"][0].as_str())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| split(mcp_url).0);
    let scope = scope.or_else(|| {
        let s = prm.as_ref()?["scopes_supported"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        (!s.is_empty()).then_some(s)
    });
    let asm = issuer_candidates(&issuer)
        .into_iter()
        .find_map(|u| get_json(&u));
    if let Some(m) = &asm {
        let methods = m["code_challenge_methods_supported"].as_array();
        if methods.is_some_and(|l| !l.iter().any(|x| x == "S256")) {
            return Err(format!("{issuer} não oferece PKCE S256; login recusado"));
        }
    }
    // Ask for a refresh token where the AS has the OIDC scope for it
    // (Vercel): otherwise the session dies with the first access token.
    let offline = asm.as_ref().is_some_and(|m| {
        m["scopes_supported"]
            .as_array()
            .is_some_and(|l| l.iter().any(|s| s == "offline_access"))
    });
    let scope = match scope {
        Some(s) if offline && !s.split(' ').any(|x| x == "offline_access") => {
            Some(format!("{s} offline_access"))
        }
        other => other,
    };
    // 2025-03-26 servers without metadata: default paths on the AS origin.
    let base = split(&issuer).0;
    let ep = |k: &str, default: &str| {
        asm.as_ref()
            .and_then(|m| m[k].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{base}{default}"))
    };
    Ok(Discovery {
        resource: prm
            .as_ref()
            .and_then(|v| v["resource"].as_str())
            .unwrap_or(mcp_url)
            .to_string(),
        scope,
        authorization_endpoint: ep("authorization_endpoint", "/authorize"),
        token_endpoint: ep("token_endpoint", "/token"),
        registration_endpoint: match &asm {
            Some(m) => m["registration_endpoint"].as_str().map(str::to_string),
            None => Some(format!("{base}/register")),
        },
        issuer,
    })
}

// ----------------------------------------------------------- registration

/// `<root>/mcp-oauth/<name>.json`: no secret in it, only `has_secret`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub mcp_url: String,
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub scope: Option<String>,
    pub client_id: String,
    pub redirect_uri: String,
    /// `none`, `client_secret_post` or `client_secret_basic`.
    pub auth_method: String,
    pub has_secret: bool,
}

pub fn client_file(root: &Path, name: &str) -> PathBuf {
    root.join("mcp-oauth").join(format!("{name}.json"))
}

fn read_registration(root: &Path, name: &str) -> Option<Registration> {
    serde_json::from_str(&std::fs::read_to_string(client_file(root, name)).ok()?).ok()
}

fn write_registration(root: &Path, name: &str, r: &Registration) -> Res<()> {
    let path = client_file(root, name);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn port_of(redirect: &str) -> Option<u16> {
    let rest = redirect.strip_prefix("http://127.0.0.1:")?;
    rest.split('/').next()?.parse().ok()
}

/// RFC 7591 at the AS's `registration_endpoint`, as a public client. A
/// secret the AS hands out anyway goes to the vault.
fn register(
    store: &dyn SecretStore,
    name: &str,
    mcp_url: &str,
    d: &Discovery,
    port: u16,
) -> Res<Registration> {
    let endpoint = d.registration_endpoint.as_deref().ok_or_else(|| {
        format!(
            "{} não oferece registro automático de cliente (RFC 7591)",
            d.issuer
        )
    })?;
    let redirect = format!("http://127.0.0.1:{port}{CALLBACK}");
    let mut body = json!({
        "client_name": "Zerith",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(s) = &d.scope {
        body["scope"] = json!(s);
    }
    let v: Value = match agent()
        .post(endpoint)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&body.to_string())
    {
        Ok(r) => r
            .into_json()
            .map_err(|e| format!("registro do cliente: resposta inválida ({e})"))?,
        Err(ureq::Error::Status(c, r)) => {
            let b = r.into_string().unwrap_or_default();
            return Err(format!(
                "registro do cliente recusado: {}",
                describe(c, &b, &[])
            ));
        }
        Err(e) => return Err(format!("registro do cliente: {e}")),
    };
    let client_id = v["client_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("registro do cliente sem client_id")?;
    let secret = v["client_secret"].as_str().filter(|s| !s.is_empty());
    match secret {
        Some(s) => put(store, &client_key(name), s)?,
        None => del(store, &client_key(name))?,
    }
    // RFC 7591 §2: an omitted method means client_secret_basic.
    let auth_method = v["token_endpoint_auth_method"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            if secret.is_some() {
                "client_secret_basic"
            } else {
                "none"
            }
            .to_string()
        });
    Ok(Registration {
        mcp_url: mcp_url.to_string(),
        resource: d.resource.clone(),
        issuer: d.issuer.clone(),
        authorization_endpoint: d.authorization_endpoint.clone(),
        token_endpoint: d.token_endpoint.clone(),
        scope: d.scope.clone(),
        client_id: client_id.to_string(),
        redirect_uri: redirect,
        auth_method,
        has_secret: secret.is_some(),
    })
}

// ------------------------------------------------------------------ token

enum TokenErr {
    /// The AS answered 4xx: the grant is dead.
    Rejected(String),
    /// Network or 5xx: try again later, keep the token.
    Net(String),
}

/// One POST to the token endpoint with the client's authentication.
fn token_call(
    store: &dyn SecretStore,
    name: &str,
    t: &Token,
    mut fields: Vec<(&'static str, String)>,
) -> Result<Value, TokenErr> {
    let secret = get(store, &client_key(name)).map_err(TokenErr::Net)?;
    let mut req = agent()
        .post(&t.token_endpoint)
        .set("Accept", "application/json");
    match (&secret, t.auth_method.as_str()) {
        (Some(s), "client_secret_basic") => {
            let raw = format!("{}:{}", enc(&t.client_id), enc(s));
            let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
            req = req.set("Authorization", &format!("Basic {b64}"));
        }
        (Some(s), _) => {
            fields.push(("client_id", t.client_id.clone()));
            fields.push(("client_secret", s.clone()));
        }
        (None, _) => fields.push(("client_id", t.client_id.clone())),
    }
    if let Some(r) = &t.resource {
        fields.push(("resource", r.clone()));
    }
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut secrets: Vec<&str> = fields.iter().map(|(_, v)| v.as_str()).collect();
    secrets.extend(secret.as_deref());
    match req.send_form(&pairs) {
        Ok(r) => r
            .into_json()
            .map_err(|_| TokenErr::Net("resposta do token não é JSON".into())),
        Err(ureq::Error::Status(c, r)) => {
            let b = r.into_string().unwrap_or_default();
            let why = describe(c, &b, &secrets);
            Err(if (400..500).contains(&c) {
                TokenErr::Rejected(why)
            } else {
                TokenErr::Net(why)
            })
        }
        Err(e) => Err(TokenErr::Net(e.to_string())),
    }
}

fn refresh(store: &dyn SecretStore, name: &str, t: &Token) -> Result<Token, TokenErr> {
    let rt = t
        .refresh_token
        .clone()
        .ok_or_else(|| TokenErr::Rejected("sem refresh_token".into()))?;
    let v = token_call(
        store,
        name,
        t,
        vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", rt),
        ],
    )?;
    let next = t
        .renewed(&v)
        .ok_or_else(|| TokenErr::Rejected("resposta sem access_token".into()))?;
    save(store, name, &next).map_err(TokenErr::Net)?;
    Ok(next)
}

/// Bearer tokens for one server, as the HTTP transport asks for them.
pub struct Auth {
    name: String,
    store: Box<dyn SecretStore + Send>,
}

impl Auth {
    pub fn new(name: &str, store: Box<dyn SecretStore + Send>) -> Self {
        Self {
            name: name.to_string(),
            store,
        }
    }

    /// The OS vault, which is where `login` saved the tokens.
    pub fn os(name: &str) -> Self {
        Self::new(name, Box::new(OsKeyring))
    }

    /// The access token, refreshed first when it expires within a minute or
    /// when `force` (the server just answered 401). A refused refresh clears
    /// the token: the person has to log in again.
    pub(crate) fn bearer(&self, force: bool) -> Res<String> {
        let store = self.store.as_ref();
        let tok = load(store, &self.name)?.ok_or_else(|| NOT_CONNECTED.to_string())?;
        let now = now_secs();
        let stale = tok.expires_at > 0 && now >= tok.expires_at - SKEW;
        if !force && !stale {
            return Ok(tok.access_token);
        }
        match refresh(store, &self.name, &tok) {
            Ok(t) => Ok(t.access_token),
            // still valid for a few seconds: use it, refresh next time
            Err(TokenErr::Net(_)) if !force && now < tok.expires_at => Ok(tok.access_token),
            Err(TokenErr::Net(e)) => Err(format!("não deu para renovar a sessão: {e}")),
            Err(TokenErr::Rejected(_)) => {
                self.reject();
                Err(EXPIRED.into())
            }
        }
    }

    /// The server refused even a fresh token.
    pub(crate) fn reject(&self) {
        let _ = del(self.store.as_ref(), &token_key(&self.name));
    }
}

// ------------------------------------------------------------------ login

/// A login waiting for the browser: the loopback socket is already bound.
pub struct Pending {
    pub authorize_url: String,
    pub registration: Registration,
    listener: TcpListener,
    state: String,
    pkce: Pkce,
}

/// Discovery, registration (reused when its redirect port is free and its
/// secret still in the vault) and the authorize URL. Nothing opens here.
pub fn begin(root: &Path, store: &dyn SecretStore, name: &str, mcp_url: &str) -> Res<Pending> {
    let d = discover(mcp_url)?;
    let reuse = read_registration(root, name)
        .filter(|r| {
            r.token_endpoint == d.token_endpoint
                && r.authorization_endpoint == d.authorization_endpoint
                && (!r.has_secret || matches!(get(store, &client_key(name)), Ok(Some(_))))
        })
        .and_then(|r| {
            let l = TcpListener::bind(("127.0.0.1", port_of(&r.redirect_uri)?)).ok()?;
            Some((r, l))
        });
    let (mut reg, listener) = match reuse {
        Some(x) => x,
        None => {
            let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = l.local_addr().map_err(|e| e.to_string())?.port();
            (register(store, name, mcp_url, &d, port)?, l)
        }
    };
    reg.scope = d.scope.clone();
    reg.resource = d.resource.clone();
    reg.mcp_url = mcp_url.to_string();
    write_registration(root, name, &reg)?;
    let pkce = Pkce::generate();
    let state = random_state();
    let sep = if reg.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut authorize_url = format!(
        "{}{sep}response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&resource={}",
        reg.authorization_endpoint,
        enc(&reg.client_id),
        enc(&reg.redirect_uri),
        enc(&pkce.challenge),
        enc(&state),
        enc(&reg.resource),
    );
    if let Some(s) = &reg.scope {
        authorize_url.push_str(&format!("&scope={}", enc(s)));
    }
    Ok(Pending {
        authorize_url,
        registration: reg,
        listener,
        state,
        pkce,
    })
}

/// Waits for the browser's callback (5 min), exchanges the code, saves the
/// tokens in the vault.
pub fn finish(store: &dyn SecretStore, name: &str, p: Pending) -> Res<()> {
    let code = wait_for_code(p.listener, CALLBACK, &p.state).map_err(|e| e.to_string())?;
    let r = &p.registration;
    let blank = Token {
        access_token: String::new(),
        refresh_token: None,
        expires_at: 0,
        token_endpoint: r.token_endpoint.clone(),
        client_id: r.client_id.clone(),
        auth_method: r.auth_method.clone(),
        resource: Some(r.resource.clone()),
    };
    let v = token_call(
        store,
        name,
        &blank,
        vec![
            ("grant_type", "authorization_code".into()),
            ("code", code),
            ("redirect_uri", r.redirect_uri.clone()),
            ("code_verifier", p.pkce.verifier.clone()),
        ],
    )
    .map_err(|e| match e {
        TokenErr::Rejected(w) | TokenErr::Net(w) => format!("troca do código recusada: {w}"),
    })?;
    let tok = blank
        .renewed(&v)
        .ok_or("troca do código: resposta sem access_token")?;
    save(store, name, &tok)
}

/// The whole login. `on_url` gets the authorize URL before the wait: the
/// caller opens the browser and/or shows the link.
pub fn login(
    root: &Path,
    store: &dyn SecretStore,
    name: &str,
    mcp_url: &str,
    on_url: &dyn Fn(&str),
) -> Res<()> {
    let p = begin(root, store, name, mcp_url)?;
    on_url(&p.authorize_url);
    finish(store, name, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_and_well_known_paths_follow_the_rfcs() {
        assert_eq!(
            split("https://mcp.vercel.com"),
            ("https://mcp.vercel.com".into(), "".into())
        );
        assert_eq!(split("https://a.b/").1, "");
        assert_eq!(split("https://a.b:8/mcp?x=1").1, "/mcp");
        assert_eq!(
            resource_candidates("https://mcp.supabase.com/mcp"),
            [
                "https://mcp.supabase.com/.well-known/oauth-protected-resource/mcp",
                "https://mcp.supabase.com/.well-known/oauth-protected-resource"
            ]
        );
        assert_eq!(resource_candidates("https://mcp.vercel.com").len(), 1);
        assert_eq!(
            issuer_candidates("https://x.test/tenant")[0],
            "https://x.test/.well-known/oauth-authorization-server/tenant"
        );
        assert_eq!(port_of("http://127.0.0.1:5123/callback"), Some(5123));
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn www_authenticate_params_parse_quoted_and_bare() {
        let h = r#"Bearer realm="OAuth", resource_metadata="https://m.test/.well-known/oauth-protected-resource/mcp", error="invalid_token", scope="a b""#;
        assert_eq!(
            auth_param(h, "resource_metadata").as_deref(),
            Some("https://m.test/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(auth_param(h, "scope").as_deref(), Some("a b"));
        assert_eq!(
            auth_param("Bearer scope=x,error=y", "scope").as_deref(),
            Some("x")
        );
        // `xscope=` is not `scope=`
        assert_eq!(auth_param("Bearer xscope=1", "scope"), None);
    }

    #[test]
    fn error_text_never_repeats_a_secret() {
        let d = describe(
            400,
            r#"{"error":"invalid_grant","error_description":"bad RT-123456"}"#,
            &["RT-123456"],
        );
        assert_eq!(d, "invalid_grant: bad ***");
        assert!(describe(500, "oops sec-abcdef", &["sec-abcdef"]).ends_with("oops ***"));
    }
}
