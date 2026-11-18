//! OAuth 2.1 for HTTP MCP servers against an in-test authorization server and
//! resource server on one `TcpListener`. The "browser" is this test hitting
//! the loopback callback itself. All tokens here are fake.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use zerith_core::secrets::{SecretError, SecretStore};
use zerith_mcp::oauth::{self, Auth};
use zerith_mcp::{Client, McpError};
use zerith_providers::oauth::Pkce;

#[derive(Clone, Default)]
struct Mem(Arc<Mutex<HashMap<String, String>>>);

impl SecretStore for Mem {
    fn get(&self, n: &str) -> Result<String, SecretError> {
        let m = self.0.lock().unwrap();
        m.get(n).cloned().ok_or(SecretError::NotFound(n.into()))
    }
    fn set(&self, n: &str, v: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(n.into(), v.into());
        Ok(())
    }
    fn delete(&self, n: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(n);
        Ok(())
    }
}

#[derive(Default)]
struct Mock {
    base: String,
    /// Access tokens the resource server accepts right now.
    valid: Vec<String>,
    issued: usize,
    refresh_ok: bool,
    /// Lifetime of the token the code exchange hands out.
    first_expires_in: i64,
    /// code_challenge the "browser" saw.
    challenge: String,
    registrations: Vec<Value>,
    grants: Vec<String>,
    bearers: Vec<String>,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(if b[i] == b'+' { b' ' } else { b[i] });
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn reply(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn handle(m: &mut Mock, method: &str, path: &str, auth: &str, body: &str) -> String {
    let b = m.base.clone();
    match (method, path) {
        ("POST", "/mcp") => {
            let token = auth.strip_prefix("Bearer ").unwrap_or("");
            if !m.valid.iter().any(|v| v == token) {
                let h = format!(
                    "WWW-Authenticate: Bearer error=\"invalid_token\", resource_metadata=\"{b}/.well-known/oauth-protected-resource/mcp\", scope=\"mcp:read mcp:write\"\r\n"
                );
                return reply("401 Unauthorized", &h, r#"{"error":"invalid_token"}"#);
            }
            m.bearers.push(token.to_string());
            let req: Value = serde_json::from_str(body).unwrap();
            let Some(id) = req.get("id") else {
                return reply("202 Accepted", "", "");
            };
            let result = match req["method"].as_str() {
                Some("tools/list") => json!({"tools":[{"name":"echo"}]}),
                _ => json!({}),
            };
            reply(
                "200 OK",
                "",
                &json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
            )
        }
        ("GET", "/.well-known/oauth-protected-resource/mcp") => reply(
            "200 OK",
            "",
            &json!({"resource": format!("{b}/mcp"), "authorization_servers": [format!("{b}/as")],
                    "scopes_supported": ["ignored-when-the-401-names-scopes"]})
            .to_string(),
        ),
        ("GET", "/.well-known/oauth-authorization-server/as") => reply(
            "200 OK",
            "",
            &json!({"issuer": format!("{b}/as"),
                    "authorization_endpoint": format!("{b}/as/authorize"),
                    "token_endpoint": format!("{b}/as/token"),
                    "registration_endpoint": format!("{b}/as/register"),
                    "code_challenge_methods_supported": ["S256"]})
            .to_string(),
        ),
        ("POST", "/as/register") => {
            let v: Value = serde_json::from_str(body).unwrap();
            m.registrations.push(v);
            // Like Supabase: a secret even though we asked for a public client.
            reply(
                "201 Created",
                "",
                r#"{"client_id":"cid-1","client_secret":"sec-SHOULD-NOT-LEAK","token_endpoint_auth_method":"client_secret_post"}"#,
            )
        }
        ("POST", "/as/token") => {
            let f = form(body);
            assert_eq!(f["client_id"], "cid-1");
            assert_eq!(f["client_secret"], "sec-SHOULD-NOT-LEAK");
            assert_eq!(f["resource"], format!("{b}/mcp"));
            let grant = f["grant_type"].clone();
            m.grants.push(grant.clone());
            let expires_in = if grant == "authorization_code" {
                assert_eq!(f["code"], "code-FAKE-123");
                assert!(f["redirect_uri"].starts_with("http://127.0.0.1:"));
                assert_eq!(
                    Pkce::from_verifier(&f["code_verifier"]).challenge,
                    m.challenge
                );
                m.first_expires_in
            } else {
                assert_eq!(f["refresh_token"], "RT-SHOULD-NOT-LEAK");
                if !m.refresh_ok {
                    return reply(
                        "400 Bad Request",
                        "",
                        r#"{"error":"invalid_grant","error_description":"refresh token RT-SHOULD-NOT-LEAK revoked"}"#,
                    );
                }
                3600
            };
            m.issued += 1;
            let at = format!("AT-SHOULD-NOT-LEAK-{}", m.issued);
            m.valid.push(at.clone());
            let mut v =
                json!({"access_token": at, "token_type": "Bearer", "expires_in": expires_in});
            if grant == "authorization_code" {
                v["refresh_token"] = json!("RT-SHOULD-NOT-LEAK");
            }
            reply("200 OK", "", &v.to_string())
        }
        _ => reply("404 Not Found", "", "{}"),
    }
}

/// Serves forever on its own thread; the test process ends it.
fn serve(state: Arc<Mutex<Mock>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    state.lock().unwrap().base = base.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let (mut len, mut auth) = (0usize, String::new());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    auth = line["authorization:".len()..].trim().to_string();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let mut parts = first.split_whitespace();
            let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let out = handle(
                &mut state.lock().unwrap(),
                method,
                path,
                &auth,
                &String::from_utf8_lossy(&body),
            );
            let _ = s.write_all(out.as_bytes());
        }
    });
    base
}

fn query(url: &str, key: &str) -> String {
    let q = url.split_once('?').unwrap().1;
    form(q).remove(key).unwrap_or_default()
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mcp-oauth-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

const LEAKS: [&str; 4] = ["AT-SHOULD", "RT-SHOULD", "sec-SHOULD", "code-FAKE"];

fn clean(s: &str) {
    for l in LEAKS {
        assert!(!s.contains(l), "secret {l} leaked into: {s}");
    }
}

#[test]
fn login_then_bearer_refresh_on_expiry_and_401_then_expired_session() {
    let mock = Arc::new(Mutex::new(Mock {
        refresh_ok: true,
        first_expires_in: 30, // inside the 60 s skew: first use refreshes
        ..Mock::default()
    }));
    let base = serve(mock.clone());
    let url = format!("{base}/mcp");
    let root = scratch("flow");
    let store = Mem::default();

    // --- login: discovery (401 + resource_metadata), DCR, code + PKCE
    let seen = mock.clone();
    oauth::login(&root, &store, "srv", &url, &|authorize: &str| {
        assert!(authorize.starts_with(&format!("{base}/as/authorize?")));
        assert_eq!(query(authorize, "response_type"), "code");
        assert_eq!(query(authorize, "client_id"), "cid-1");
        assert_eq!(query(authorize, "code_challenge_method"), "S256");
        assert_eq!(query(authorize, "resource"), format!("{base}/mcp"));
        assert_eq!(query(authorize, "scope"), "mcp:read mcp:write");
        seen.lock().unwrap().challenge = query(authorize, "code_challenge");
        let cb = format!(
            "{}?code=code-FAKE-123&state={}",
            query(authorize, "redirect_uri"),
            query(authorize, "state")
        );
        std::thread::spawn(move || {
            // the browser lands on the loopback callback
            let r = ureq::get(&cb).call().expect("callback");
            assert_eq!(r.status(), 200);
        });
    })
    .expect("login");

    {
        let m = mock.lock().unwrap();
        let reg = &m.registrations[0];
        assert_eq!(reg["client_name"], "Zerith");
        assert_eq!(reg["token_endpoint_auth_method"], "none");
        assert_eq!(
            reg["grant_types"],
            json!(["authorization_code", "refresh_token"])
        );
        assert!(reg["redirect_uris"][0]
            .as_str()
            .unwrap()
            .ends_with("/callback"));
    }
    // the client file has no secret; the vault has them
    let file = std::fs::read_to_string(oauth::client_file(&root, "srv")).unwrap();
    clean(&file);
    assert!(file.contains("cid-1") && file.contains("\"has_secret\": true"));
    let vault = format!("{:?}", store.0.lock().unwrap());
    assert!(vault.contains("sec-SHOULD-NOT-LEAK") && vault.contains("RT-SHOULD-NOT-LEAK"));
    assert!(oauth::status(&store, "srv").connected);

    // --- use: 30 s left < skew, so the first request refreshes first
    let auth = || Some(Auth::new("srv", Box::new(store.clone())));
    let mut c = Client::connect_http_auth("srv", &url, &[], auth()).expect("connect");
    assert_eq!(c.tools().unwrap()[0].name, "echo");
    {
        let m = mock.lock().unwrap();
        assert_eq!(m.grants, ["authorization_code", "refresh_token"]);
        assert!(m.bearers.iter().all(|b| b == "AT-SHOULD-NOT-LEAK-2"));
    }

    // --- the server revokes the token: 401, refresh, retry once
    mock.lock().unwrap().valid.clear();
    assert_eq!(c.tools().unwrap()[0].name, "echo");
    {
        let m = mock.lock().unwrap();
        assert_eq!(m.grants.len(), 3);
        assert_eq!(m.bearers.last().unwrap(), "AT-SHOULD-NOT-LEAK-3");
    }

    // --- refresh refused: token cleared, "Sessão expirada"
    {
        let mut m = mock.lock().unwrap();
        m.valid.clear();
        m.refresh_ok = false;
    }
    let err = c.tools().unwrap_err();
    let text = err.to_string();
    clean(&text);
    assert!(matches!(err, McpError::Http(_)));
    assert!(text.contains(oauth::EXPIRED), "{text}");
    assert!(!oauth::status(&store, "srv").connected);
    // and nothing to send any more: the next connect says to log in
    let err = Client::connect_http_auth("srv", &url, &[], auth())
        .err()
        .unwrap();
    clean(&err.to_string());
    assert!(err.to_string().contains(oauth::NOT_CONNECTED));

    // --- a second login reuses the registered client (port free again)
    let p = oauth::begin(&root, &store, "srv", &url).expect("begin");
    assert_eq!(p.registration.client_id, "cid-1");
    assert_eq!(mock.lock().unwrap().registrations.len(), 1);
    drop(p);

    oauth::forget(&root, &store, "srv").unwrap();
    assert!(store.0.lock().unwrap().is_empty());
    assert!(!oauth::client_file(&root, "srv").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_server_without_metadata_is_its_own_authorization_server() {
    // 401 without resource_metadata, and every well-known path 404s.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let out = if first.starts_with("POST /mcp") {
                reply("401 Unauthorized", "WWW-Authenticate: Bearer\r\n", "{}")
            } else {
                reply("404 Not Found", "", "{}")
            };
            let _ = s.write_all(out.as_bytes());
        }
    });
    let d = oauth::discover(&format!("{base}/mcp")).unwrap();
    assert_eq!(d.issuer, base);
    assert_eq!(d.authorization_endpoint, format!("{base}/authorize"));
    assert_eq!(d.token_endpoint, format!("{base}/token"));
    assert_eq!(d.registration_endpoint, Some(format!("{base}/register")));
    assert_eq!(d.resource, format!("{base}/mcp"));
    assert_eq!(d.scope, None);
}

/// Real Vercel, Supabase and Canva: discovery + DCR + authorize URL, and the
/// AS must not reject the URL (no browser, no login, no token). Registers a
/// "Zerith" client at each AS. Run with `--ignored` on purpose.
#[test]
#[ignore]
fn live_providers_accept_the_authorize_url() {
    // Kept between runs: a rerun reuses the registered clients (same port
    // free) instead of registering new ones at each provider.
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("mcp-oauth-live");
    let store = Mem::default();
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    for (name, url) in [
        ("vercel", "https://mcp.vercel.com"),
        ("supabase", "https://mcp.supabase.com/mcp"),
        ("canva", "https://mcp.canva.com/mcp"),
    ] {
        let d = oauth::discover(url).unwrap_or_else(|e| panic!("{name}: discovery: {e}"));
        println!(
            "{name}: discovery ok issuer={} authorize={} token={} register={:?} resource={} scope={:?}",
            d.issuer, d.authorization_endpoint, d.token_endpoint, d.registration_endpoint, d.resource, d.scope
        );
        let p = oauth::begin(&root, &store, name, url).unwrap_or_else(|e| panic!("{name}: {e}"));
        let r = &p.registration;
        println!(
            "{name}: DCR client_id={} auth_method={} secret_in_vault={} redirect={}",
            r.client_id, r.auth_method, r.has_secret, r.redirect_uri
        );
        let (status, location, body) = match agent.get(&p.authorize_url).call() {
            Ok(resp) => (
                resp.status(),
                resp.header("location").unwrap_or("").to_string(),
                resp.into_string().unwrap_or_default(),
            ),
            Err(ureq::Error::Status(c, resp)) => (
                c,
                resp.header("location").unwrap_or("").to_string(),
                resp.into_string().unwrap_or_default(),
            ),
            Err(e) => panic!("{name}: authorize GET: {e}"),
        };
        let loc: String = location.chars().take(140).collect();
        let title = body
            .split_once("<title>")
            .and_then(|(_, t)| t.split_once("</title>"))
            .map(|(t, _)| t.trim().to_string())
            .unwrap_or_default();
        println!("{name}: authorize GET -> {status} location={loc} title={title:?}");
        let low = (location.clone() + &body).to_ascii_lowercase();
        assert!(
            status < 400,
            "{name}: AS refused the authorize URL ({status}): {}",
            body.chars().take(300).collect::<String>()
        );
        for bad in [
            "invalid_client",
            "invalid_redirect",
            "invalid_request",
            "redirect_uri_mismatch",
        ] {
            assert!(!low.contains(bad), "{name}: AS answered {bad}");
        }
        assert!(
            !location.starts_with(&r.redirect_uri),
            "{name}: AS bounced straight back to us: {loc}"
        );
    }
}
