//! `browser`: drives a browser over the Chrome DevTools Protocol (CDP),
//! supporting both **Cloudflare Kitesurf** (lightweight stateless Rust/Wasm
//! agent browser via `workerd` / CDP WebSocket) and a local **Chromium**
//! (Edge/Chrome) instance with a persistent `<data>/browser-profile`.
//!
//! Routing rules (`BrowserEngine::Auto`, default):
//! - Local, LAN (RFC1918), ISP CGNAT (`100.64.0.0/10`) and `.local`/`.lan`
//!   addresses always route to the local Chromium browser so device WebGUIs
//!   (MikroTik WebFig, FortiGate, Huawei, Unifi, Zabbix, Proxmox) and local
//!   dev servers have full SPA/WebGL support and persistent login cookies.
//! - Public web URLs route to Kitesurf when a Kitesurf CDP endpoint is
//!   configured (`[browser] kitesurf_url` in `settings.toml`,
//!   `ZERITH_KITESURF_URL`, or `ZERITH_BROWSER_CDP_URL`), falling back
//!   automatically to local Chromium if Kitesurf is unreachable or fails.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zerith_core::{BrowserEngine, Paths, Settings};
use zerith_providers::media::{b64_decode, b64_encode};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

const LOAD_WAIT: Duration = Duration::from_secs(30);
const TEXT_CAP: usize = 15_000;
/// Socket poll interval: how often a blocked read looks at cancel/deadline.
const POLL: Duration = Duration::from_millis(200);
const MAX_MESSAGE: usize = 64 << 20;
const DEFAULT_KITESURF_URL: &str = "ws://127.0.0.1:8787/cdp";

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make_with(
        "browser",
        "Controla um navegador via Chrome DevTools Protocol (CDP): abre páginas, lê o conteúdo, clica, digita, roda JavaScript, ajusta viewport responsivo e tira prints. \
Suporta motor híbrido (`engine`): `auto` (padrão: usa Cloudflare Kitesurf para páginas públicas quando configurado e Chromium local para IPs de LAN/CGNAT/localhost ou como fallback automático), `kitesurf` (motor leve Wasm/Blitz) ou `chromium` (Edge/Chrome local com perfil persistente). \
Fluxo: navigate {url} → snapshot (título, texto visível e lista numerada de elementos [n]) → click/type com ref=n → snapshot de novo para ver o resultado. \
Login em painel (OLT, roteador, firewall): type o usuário e use fill_secret {ref do campo de senha, secret} para a senha; nunca peça a senha ao usuário nem a escreva com type. \
screenshot salva o PNG (passe full_page=true para capturar páginas longas com scroll), anexa a imagem automaticamente para você inspecionar visualmente e devolve uma linha Markdown ![screenshot](caminho) para mostrar ao usuário. \
viewport altera a resolução da página (ex.: width=390, height=844 para mobile ou width=1440, height=900 para desktop; reset=true restaura) para testar responsividade. \
console lista erros/avisos de console JavaScript e exceções não tratadas capturadas desde o último navigate.",
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["navigate", "snapshot", "click", "type", "fill_secret", "press", "eval", "screenshot", "viewport", "console", "back", "close"]},
            "secret": {"type": "string", "description": "fill_secret: nome do segredo no cofre do sistema (zerith secret set <nome>) ou nome de um equipamento do inventário com secret_ref; o valor é digitado no campo de senha sem passar por você"},
            "engine": {"type": "string", "enum": ["auto", "kitesurf", "chromium"], "description": "motor CDP: auto (Kitesurf na web pública com fallback Chromium; Chromium direto em LAN/CGNAT/localhost), kitesurf ou chromium"},
            "url": {"type": "string", "description": "navigate: endereço http(s)"},
            "ref": {"type": "integer", "description": "click/type: número [n] do elemento no último snapshot"},
            "selector": {"type": "string", "description": "click/type: seletor CSS, alternativa ao ref"},
            "text": {"type": "string", "description": "type: texto a digitar (substitui o conteúdo do campo)"},
            "submit": {"type": "boolean", "description": "type: aperta Enter depois de digitar"},
            "key": {"type": "string", "description": "press: Enter, Tab, Escape, Backspace, Delete, ArrowUp/Down/Left/Right, PageUp, PageDown, Home, End, Space ou um caractere"},
            "expression": {"type": "string", "description": "eval: expressão JavaScript; o valor volta como JSON (promises são aguardadas)"},
            "full_page": {"type": "boolean", "description": "screenshot: captura a página inteira além da área visível"},
            "width": {"type": "integer", "description": "viewport: largura em pixels (160–4096, ex.: 390 mobile, 768 tablet, 1440 desktop)"},
            "height": {"type": "integer", "description": "viewport: altura em pixels (160–4096, ex.: 844 mobile, 1024 tablet, 900 desktop)"},
            "mobile": {"type": "boolean", "description": "viewport: emula dispositivo móvel (padrão: true quando width <= 640)"},
            "device_scale_factor": {"type": "number", "description": "viewport: densidade de pixel / DPR (padrão: 2 em mobile, 1 em desktop)"},
            "reset": {"type": "boolean", "description": "viewport: limpa a emulação de viewport e volta ao tamanho padrão"}
        }, "required": ["action"]}),
        "navigate/click/type/press/back: título, motor ativo e URL da página; snapshot: texto visível e elementos numerados; eval: o valor; screenshot: caminho do PNG e linha Markdown; viewport: dimensões aplicadas",
        Permission::Network,
        60,
        false,
        |a, _| Ok(Some(Action::Network(label(a)?))),
        run,
    )]
}

#[derive(Debug, Clone, PartialEq)]
enum ViewportSpec {
    Reset,
    Set {
        width: u32,
        height: u32,
        mobile: bool,
        device_scale_factor: f64,
    },
}

fn viewport_spec(args: &Value) -> Result<ViewportSpec> {
    if args["reset"].as_bool() == Some(true) {
        return Ok(ViewportSpec::Reset);
    }
    let width = args["width"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            ToolError::BadArguments(
                "viewport precisa de width e height (160–4096) ou reset=true".into(),
            )
        })?;
    let height = args["height"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            ToolError::BadArguments(
                "viewport precisa de width e height (160–4096) ou reset=true".into(),
            )
        })?;
    if !(160..=4096).contains(&width) || !(160..=4096).contains(&height) {
        return Err(ToolError::BadArguments(format!(
            "viewport fora do intervalo permitido (160–4096): {width}x{height}"
        )));
    }
    let mobile = args["mobile"].as_bool().unwrap_or(width <= 640);
    let dpr = args["device_scale_factor"]
        .as_f64()
        .unwrap_or(if mobile { 2.0 } else { 1.0 });
    if !dpr.is_finite() || dpr <= 0.0 || dpr > 4.0 {
        return Err(ToolError::BadArguments(format!(
            "device_scale_factor inválido (use > 0 até 4): {dpr}"
        )));
    }
    Ok(ViewportSpec::Set {
        width,
        height,
        mobile,
        device_scale_factor: dpr,
    })
}

/// What the approval prompt shows: the action and what it targets.
fn label(args: &Value) -> Result<String> {
    let action = str_arg(args, "action")?;
    let detail: String = match action {
        "navigate" => str_arg(args, "url")?.into(),
        "click" | "type" => opt_str(args, "selector")
            .map(String::from)
            .unwrap_or_else(|| format!("ref {}", args["ref"])),
        // The approval card names the secret and the page, never the value.
        "fill_secret" => format!("{} no campo ref {}", str_arg(args, "secret")?, args["ref"]),
        "press" => str_arg(args, "key")?.into(),
        "eval" => str_arg(args, "expression")?.chars().take(200).collect(),
        "viewport" => match viewport_spec(args)? {
            ViewportSpec::Reset => "reset".into(),
            ViewportSpec::Set { width, height, .. } => format!("{width}x{height}"),
        },
        "screenshot" => {
            if args["full_page"].as_bool() == Some(true) {
                "full_page".into()
            } else {
                String::new()
            }
        }
        "snapshot" | "console" | "back" | "close" => String::new(),
        other => return Err(bad_action(other)),
    };
    Ok(format!("navegador {action} {detail}")
        .trim_end()
        .to_string())
}

fn bad_action(a: &str) -> ToolError {
    ToolError::BadArguments(format!(
        "ação desconhecida: {a}; use navigate, snapshot, click, type, fill_secret, press, eval, screenshot, viewport, console, back ou close"
    ))
}

static SESSION: Mutex<Option<Cdp>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq)]
enum BackendKind {
    Chromium { port: u16 },
    Kitesurf { endpoint: String },
}

impl BackendKind {
    fn badge(&self) -> &'static str {
        match self {
            Self::Chromium { .. } => "Chromium",
            Self::Kitesurf { .. } => "Kitesurf",
        }
    }
}

/// Extracts the hostname from an `http(s)://` URL (or bare `host/path`),
/// stripping port and IPv6 brackets.
fn extract_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let after_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ws://"))
        .or_else(|| trimmed.strip_prefix("wss://"))
        .unwrap_or(trimmed);
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, hp)| hp)
        .trim();
    if host_port.is_empty() {
        return None;
    }
    if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest.find(']')?;
        return Some(rest[..end].to_ascii_lowercase());
    }
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(h, _)| h)
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Returns `true` when `url` targets `localhost`, RFC1918 private LANs,
/// ISP CGNAT (`100.64.0.0/10`), link-local addresses, or `.local`/`.lan`
/// mDNS/internal hostnames that require the local Chromium instance.
pub fn is_private_or_local_url(url: &str) -> bool {
    let Some(host) = extract_url_host(url) else {
        return false;
    };
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".lan")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
        || host.ends_with(".corp")
        || host.ends_with(".intranet")
        || (!host.contains('.') && !host.contains(':'))
    {
        return true;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        let oct = ip.octets();
        let is_cgnat = oct[0] == 100 && (oct[1] & 0b1100_0000) == 64;
        return ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_unspecified()
            || is_cgnat;
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        let seg0 = ip.segments()[0];
        let is_ula = (seg0 & 0xfe00) == 0xfc00;
        let is_ll = (seg0 & 0xffc0) == 0xfe80;
        return ip.is_loopback() || ip.is_unspecified() || is_ula || is_ll;
    }
    false
}

/// Parses a Kitesurf/CDP endpoint (`ws://host:port/path`, `http://host:port`,
/// or `host:port/path`) into `(host, port, path)`.
pub fn parse_cdp_endpoint(raw: &str) -> Result<(String, u16, String)> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(ToolError::BadArguments(
            "endpoint CDP do Kitesurf vazio".into(),
        ));
    }
    if s.starts_with("wss://") || s.starts_with("https://") {
        return Err(ToolError::BadArguments(
            "endpoint CDP local deve usar ws:// ou http:// (ou aponte para o túnel local workerd)"
                .into(),
        ));
    }
    let without_scheme = s
        .strip_prefix("ws://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (host_port, raw_path) = match without_scheme.find('/') {
        Some(idx) => (&without_scheme[..idx], &without_scheme[idx..]),
        None => (without_scheme, ""),
    };
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| ToolError::BadArguments(format!("endpoint CDP inválido: {s}")))?;
        let h = &rest[..end];
        let after = &rest[end + 1..];
        let p = if let Some(p_str) = after.strip_prefix(':') {
            p_str
                .parse::<u16>()
                .map_err(|_| ToolError::BadArguments(format!("porta CDP inválida em {s}")))?
        } else {
            8787
        };
        (h.to_string(), p)
    } else if let Some((h, p_str)) = host_port.rsplit_once(':') {
        let p = p_str
            .parse::<u16>()
            .map_err(|_| ToolError::BadArguments(format!("porta CDP inválida em {s}")))?;
        (h.trim().to_string(), p)
    } else {
        (host_port.trim().to_string(), 8787)
    };
    if host.is_empty() {
        return Err(ToolError::BadArguments(format!(
            "host ausente no endpoint CDP: {s}"
        )));
    }
    let path = if raw_path.is_empty() || raw_path == "/" {
        "/cdp".to_string()
    } else {
        raw_path.to_string()
    };
    Ok((host, port, path))
}

/// Decides which engine to use and whether automatic fallback to local Chromium
/// is enabled.
pub fn select_target_engine(
    requested: BrowserEngine,
    kitesurf_url: Option<&str>,
    target_url: Option<&str>,
) -> (BrowserEngine, bool) {
    match requested {
        BrowserEngine::Chromium => (BrowserEngine::Chromium, false),
        BrowserEngine::Kitesurf => (BrowserEngine::Kitesurf, false),
        BrowserEngine::Auto => {
            if target_url.is_some_and(is_private_or_local_url) {
                (BrowserEngine::Chromium, false)
            } else if kitesurf_url.is_some_and(|u| !u.trim().is_empty()) {
                (BrowserEngine::Kitesurf, true)
            } else {
                (BrowserEngine::Chromium, false)
            }
        }
    }
}

fn resolve_browser_config(args: &Value) -> Result<(BrowserEngine, Option<String>, bool)> {
    let paths = Paths::resolve();
    let settings = Settings::load(&paths, None).settings;
    let mut engine = settings.browser.engine;
    if let Ok(env_eng) = std::env::var("ZERITH_BROWSER_ENGINE") {
        if !env_eng.trim().is_empty() {
            engine = BrowserEngine::parse(&env_eng).ok_or_else(|| {
                ToolError::BadArguments(format!(
                    "ZERITH_BROWSER_ENGINE inválido: {env_eng} (use auto, kitesurf ou chromium)"
                ))
            })?;
        }
    }
    if let Some(arg_eng) = opt_str(args, "engine") {
        engine = BrowserEngine::parse(arg_eng).ok_or_else(|| {
            ToolError::BadArguments(format!(
                "engine inválido: {arg_eng} (use auto, kitesurf ou chromium)"
            ))
        })?;
    }
    let kitesurf_url = std::env::var("ZERITH_KITESURF_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("ZERITH_BROWSER_CDP_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            settings
                .browser
                .kitesurf_url
                .filter(|s| !s.trim().is_empty())
        });
    let headless = settings.browser.headless
        || std::env::var("ZERITH_BROWSER_HEADLESS").is_ok_and(|v| v == "1");
    Ok((engine, kitesurf_url, headless))
}

fn run(args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let action = str_arg(args, "action")?;
    let root = Paths::resolve().root;
    let profile = root.join("browser-profile");
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if action == "close" {
        let was_kitesurf = slot
            .as_ref()
            .is_some_and(|c| matches!(c.backend, BackendKind::Kitesurf { .. }));
        *slot = None;
        if was_kitesurf && live_port(&profile).is_none() {
            return Ok("Sessão Kitesurf encerrada.".into());
        }
        return close(&profile, exec);
    }

    let (requested_engine, kitesurf_url, headless) = resolve_browser_config(args)?;
    let nav_target = if action == "navigate" {
        Some(nav_url(str_arg(args, "url")?)?)
    } else {
        None
    };

    // For non-navigate actions without an explicit `engine` override, stay on
    // whichever engine currently holds the active page session.
    let (primary, allow_fallback) =
        if action != "navigate" && opt_str(args, "engine").is_none() && slot.is_some() {
            let current = match slot.as_ref().unwrap().backend {
                BackendKind::Kitesurf { .. } => BrowserEngine::Kitesurf,
                BackendKind::Chromium { .. } => BrowserEngine::Chromium,
            };
            (current, false)
        } else {
            select_target_engine(
                requested_engine,
                kitesurf_url.as_deref(),
                nav_target.as_deref(),
            )
        };

    if primary == BrowserEngine::Kitesurf {
        let endpoint = kitesurf_url.as_deref().unwrap_or(DEFAULT_KITESURF_URL);
        match attach_kitesurf(&mut slot, endpoint, exec)
            .and_then(|c| act(c, action, args, &root, exec))
        {
            Ok(out) => return Ok(out),
            Err(err) => {
                *slot = None;
                if !allow_fallback
                    || matches!(err, ToolError::Cancelled | ToolError::BadArguments(_))
                {
                    return Err(err);
                }
                // Automatic fallback to local Chromium when Kitesurf is
                // unavailable or fails to render a public page in Auto mode.
            }
        }
    }

    let c = attach_chromium(&mut slot, &profile, headless, exec)?;
    let r = act(c, action, args, &root, exec);
    // A broken socket (or a frame cut by cancel/timeout) is not reusable.
    if matches!(
        r,
        Err(ToolError::Io(_) | ToolError::Timeout(_) | ToolError::Cancelled)
    ) {
        *slot = None;
    }
    r
}

fn act(c: &mut Cdp, action: &str, args: &Value, root: &Path, exec: &Exec<'_>) -> Result<String> {
    // Drop events left over from earlier calls (late loads, page timers).
    c.wait(&[], Duration::ZERO, exec)?;
    c.seen.clear();
    match action {
        "navigate" => {
            let url = nav_url(str_arg(args, "url")?)?;
            c.console_errors.clear();
            let r = c.call("Page.navigate", json!({ "url": url }), exec)?;
            if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
                return Err(ToolError::Failed(format!("não abriu {url}: {e}")));
            }
            // No loaderId = same-document navigation: no load event comes.
            let loaded =
                r["loaderId"].is_null() || c.wait(&["Page.loadEventFired"], LOAD_WAIT, exec)?;
            Ok(format!(
                "{}{}\nUse snapshot para ler a página e ver os elementos.",
                c.page(exec)?,
                if loaded {
                    ""
                } else {
                    "\n(ainda carregando após 30 s)"
                }
            ))
        }
        "snapshot" => {
            let v = c.eval(SNAPSHOT_JS, exec)?["value"].take();
            let full = v["text"].as_str().unwrap_or("");
            let text: String = full.chars().take(TEXT_CAP).collect();
            let items: Vec<&str> = v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            Ok(format!(
                "Título: {}\nURL: {}\n\n--- Texto visível{} ---\n{text}\n\n--- Elementos interativos (use ref=N em click/type) ---\n{}",
                v["title"].as_str().unwrap_or(""),
                v["url"].as_str().unwrap_or(""),
                if full.chars().count() > TEXT_CAP { " (cortado em 15 mil caracteres)" } else { "" },
                if items.is_empty() { "(nenhum)".to_string() } else { items.join("\n") }
            ))
        }
        "click" => {
            let at = c.eval(
                &format!(
                    "(() => {{ const e = {}; if (!e) return null; e.scrollIntoView({{block: 'center', inline: 'center'}}); \
                     const r = e.getBoundingClientRect(); \
                     return {{x: r.left + r.width / 2, y: r.top + r.height / 2, \
                     label: (e.getAttribute('aria-label') || e.innerText || e.value || e.tagName).trim().replace(/\\s+/g, ' ').slice(0, 80)}}; }})()",
                    find(args)?
                ),
                exec,
            )?["value"]
                .take();
            if at.is_null() {
                return Err(not_found());
            }
            // Trusted mouse events at the element's centre, like a real click.
            for t in ["mouseMoved", "mousePressed", "mouseReleased"] {
                c.call(
                    "Input.dispatchMouseEvent",
                    json!({"type": t, "x": at["x"], "y": at["y"], "button": "left", "clickCount": 1}),
                    exec,
                )?;
            }
            c.settle(exec)?;
            Ok(format!(
                "Clicou em \"{}\".\n{}",
                at["label"].as_str().unwrap_or(""),
                c.page(exec)?
            ))
        }
        "type" => {
            let text = args["text"]
                .as_str()
                .ok_or_else(|| ToolError::BadArguments("text precisa ser uma string".into()))?;
            let found = c.eval(
                &format!(
                    "(() => {{ const e = {}; if (!e) return false; e.focus(); if (typeof e.select === 'function') e.select(); return true; }})()",
                    find(args)?
                ),
                exec,
            )?;
            if found["value"] != true {
                return Err(not_found());
            }
            // insertText replaces the selection, so the field ends with `text`.
            c.call("Input.insertText", json!({ "text": text }), exec)?;
            let submit = args["submit"].as_bool().unwrap_or(false);
            if submit {
                press(c, "Enter", exec)?;
                c.settle(exec)?;
            }
            Ok(format!(
                "Digitou {} caracteres{}.\n{}",
                text.chars().count(),
                if submit { " e apertou Enter" } else { "" },
                c.page(exec)?
            ))
        }
        "fill_secret" => {
            let name = str_arg(args, "secret")?;
            let value = vault_value(name, exec)?;
            // Only a password field: a page told to "type the secret into the
            // search box" must not get it back into the snapshot.
            let found = c.eval(
                &format!(
                    "(() => {{ const e = {}; if (!e) return 'none'; if (e.type !== 'password') return 'not-password'; e.focus(); if (typeof e.select === 'function') e.select(); return 'ok'; }})()",
                    find(args)?
                ),
                exec,
            )?;
            match found["value"].as_str() {
                Some("ok") => {}
                Some("not-password") => {
                    return Err(ToolError::Failed(
                        "fill_secret só preenche campo de senha (type=password); para o usuário use type".into(),
                    ))
                }
                _ => return Err(not_found()),
            }
            c.call("Input.insertText", json!({ "text": value }), exec)?;
            let submit = args["submit"].as_bool().unwrap_or(false);
            if submit {
                press(c, "Enter", exec)?;
                c.settle(exec)?;
            }
            Ok(format!(
                "Senha `{name}` preenchida do cofre ({} caracteres; o valor não passa por você){}.\n{}",
                value.chars().count(),
                if submit { " e apertou Enter" } else { "" },
                c.page(exec)?
            ))
        }
        "press" => {
            let key = str_arg(args, "key")?;
            press(c, key, exec)?;
            c.settle(exec)?;
            Ok(format!("Apertou {key}.\n{}", c.page(exec)?))
        }
        "eval" => {
            let r = c.eval(str_arg(args, "expression")?, exec)?;
            Ok(match &r["value"] {
                Value::String(s) => s.clone(),
                Value::Null if r["type"] == "undefined" => "undefined".into(),
                v => serde_json::to_string_pretty(v).unwrap_or_default(),
            })
        }
        "screenshot" => {
            c.call("Page.bringToFront", json!({}), exec)?;
            let full_page = args["full_page"].as_bool().unwrap_or(false);
            let params = if full_page {
                json!({
                    "format": "png",
                    "captureBeyondViewport": true,
                    "fromSurface": true
                })
            } else {
                json!({"format": "png"})
            };
            let r = c.call("Page.captureScreenshot", params, exec)?;
            let png = r["data"]
                .as_str()
                .and_then(b64_decode)
                .ok_or_else(|| ToolError::Failed("o navegador não devolveu a imagem".into()))?;
            let dir = root.join("browser").join("shots");
            std::fs::create_dir_all(&dir)?;
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let path = dir.join(format!("{ms}.png"));
            std::fs::write(&path, &png)?;
            let kb = png.len().div_ceil(1024);
            let shown = path.display().to_string().replace('\\', "/");
            let kind = if full_page {
                "Captura de página inteira"
            } else {
                "Captura"
            };
            Ok(format!(
                "{} png, {kb} KB: ![imagem](<{shown}>)\n{kind} salva em {} ({kb} KB).\n{}\nCole a linha Markdown acima na resposta para o usuário ver a imagem.",
                crate::IMAGE_MARKER,
                path.display(),
                md_image(&path)
            ))
        }
        "viewport" => match viewport_spec(args)? {
            ViewportSpec::Reset => {
                c.call("Emulation.clearDeviceMetricsOverride", json!({}), exec)?;
                Ok("Viewport restaurado ao tamanho padrão da janela.".into())
            }
            ViewportSpec::Set {
                width,
                height,
                mobile,
                device_scale_factor,
            } => {
                c.call(
                    "Emulation.setDeviceMetricsOverride",
                    json!({
                        "width": width,
                        "height": height,
                        "deviceScaleFactor": device_scale_factor,
                        "mobile": mobile,
                    }),
                    exec,
                )?;
                Ok(format!(
                    "Viewport ajustado para {width}x{height} ({}, DPR {device_scale_factor}).",
                    if mobile { "mobile" } else { "desktop" }
                ))
            }
        },
        "console" => {
            // Pump pending CDP events briefly so recent console errors land in `console_errors`.
            let _ = c.eval("void 0", exec);
            if c.console_errors.is_empty() {
                Ok("Nenhum erro ou aviso de console capturado nesta página.".into())
            } else {
                Ok(format!(
                    "{} evento(s) de console:\n{}",
                    c.console_errors.len(),
                    c.console_errors.join("\n")
                ))
            }
        }
        "back" => {
            c.eval("history.back()", exec)?;
            c.settle(exec)?;
            c.page(exec)
        }
        other => Err(bad_action(other)),
    }
}

/// The value behind `secret`: a vault entry by that name, or the
/// `secret_ref` of an inventory device with that name. Concept after Hermes
/// Agent's browser vault (MIT): the model only ever handles the name, and the
/// value is also scrubbed from anything the page echoes back.
fn vault_value(name: &str, exec: &Exec<'_>) -> Result<String> {
    let key = match zerith_core::secrets::get_secret(name) {
        Ok(v) if !v.is_empty() => {
            exec.host.note_secret(&v);
            return Ok(v);
        }
        _ => crate::network::device_secret_ref(name),
    };
    let Some(key) = key else {
        return Err(ToolError::Failed(format!(
            "segredo `{name}` não está no cofre nem é um equipamento com secret_ref; peça ao usuário: zerith secret set {name}"
        )));
    };
    let v = zerith_core::secrets::get_secret(&key).map_err(|e| {
        ToolError::Failed(format!(
            "segredo `{key}` (do equipamento {name}) não está no cofre ({e}); peça: zerith secret set {key}"
        ))
    })?;
    exec.host.note_secret(&v);
    Ok(v)
}

fn not_found() -> ToolError {
    ToolError::BadArguments(
        "elemento não encontrado; rode snapshot de novo (os números mudam quando a página muda)"
            .into(),
    )
}

/// JS expression for the element `ref` (from the last snapshot) or `selector`.
fn find(args: &Value) -> Result<String> {
    let r = &args["ref"];
    let n = r.as_f64().map(|f| f as u64).or_else(|| {
        r.as_str()?
            .trim_matches(|c| c == '[' || c == ']')
            .parse()
            .ok()
    });
    match (n, opt_str(args, "selector")) {
        (Some(n), _) => Ok(format!(
            "document.querySelector('[data-zerith-ref=\"{n}\"]')"
        )),
        // A JSON string literal is a valid JS string literal.
        (None, Some(s)) => Ok(format!("document.querySelector({})", Value::from(s))),
        (None, None) => Err(ToolError::BadArguments(
            "informe ref (número do snapshot) ou selector".into(),
        )),
    }
}

fn nav_url(u: &str) -> Result<String> {
    let u = u.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower == "about:blank" {
        Ok(u.into())
    } else if u.contains("://") {
        Err(ToolError::BadArguments(
            "só URLs http:// e https:// são aceitas".into(),
        ))
    } else {
        Ok(format!("https://{u}"))
    }
}

/// `![screenshot](path)`, same rule as `media::markdown_image`: `/` separators,
/// `<...>` around a path with spaces or `)`.
fn md_image(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    if p.contains(char::is_whitespace) || p.contains(')') {
        format!("![screenshot](<{p}>)")
    } else {
        format!("![screenshot]({p})")
    }
}

fn press(c: &mut Cdp, key: &str, exec: &Exec<'_>) -> Result<()> {
    const NAMED: &[(&str, u32)] = &[
        ("Enter", 13),
        ("Tab", 9),
        ("Escape", 27),
        ("Backspace", 8),
        ("Delete", 46),
        ("ArrowLeft", 37),
        ("ArrowUp", 38),
        ("ArrowRight", 39),
        ("ArrowDown", 40),
        ("Home", 36),
        ("End", 35),
        ("PageUp", 33),
        ("PageDown", 34),
        ("Space", 32),
    ];
    let (key, code, vk, text) =
        if let Some(&(name, vk)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(key)) {
            match name {
                "Enter" => (name, name, vk, "\r"),
                "Space" => (" ", name, vk, " "),
                _ => (name, name, vk, ""),
            }
        } else if let [ch] = key.chars().collect::<Vec<_>>()[..] {
            let vk = if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase() as u32
            } else {
                0
            };
            (key, "", vk, key)
        } else {
            return Err(ToolError::BadArguments(format!(
                "tecla desconhecida: {key}"
            )));
        };
    let mut ev = json!({"type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk});
    if !text.is_empty() {
        ev["text"] = text.into();
    }
    c.call("Input.dispatchKeyEvent", ev.clone(), exec)?;
    ev["type"] = "keyUp".into();
    if let Some(o) = ev.as_object_mut() {
        o.remove("text");
    }
    c.call("Input.dispatchKeyEvent", ev, exec)?;
    Ok(())
}

/// Visible text plus every visible interactive element, numbered through a
/// `data-zerith-ref` attribute that `find` targets later.
const SNAPSHOT_JS: &str = r#"(() => {
  document.querySelectorAll('[data-zerith-ref]').forEach(e => e.removeAttribute('data-zerith-ref'));
  const sel = 'a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable=true]';
  const items = [];
  let n = 0;
  for (const e of document.querySelectorAll(sel)) {
    if (n >= 150) break;
    const r = e.getBoundingClientRect(), s = getComputedStyle(e);
    if (r.width === 0 || r.height === 0 || s.visibility === 'hidden' || s.display === 'none') continue;
    n++;
    e.setAttribute('data-zerith-ref', n);
    const tag = e.tagName.toLowerCase();
    const role = e.getAttribute('role') || (tag === 'input' ? 'input[' + (e.type || 'text') + ']' : tag);
    const label = (e.getAttribute('aria-label') || e.innerText || (tag === 'input' && e.type !== 'password' ? e.value : '') || e.title || e.alt || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    let line = '[' + n + '] ' + role + ' "' + label + '"';
    if (e.placeholder) line += ' placeholder="' + e.placeholder + '"';
    if (tag === 'a') line += ' -> ' + e.href;
    items.push(line);
  }
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n').trim();
  return {title: document.title, url: location.href, text, items};
})()"#;

// --- browser process -----------------------------------------------------------

fn browser_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZERITH_BROWSER") {
        return Some(p.into());
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
            "/snap/bin/chromium",
        ]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

fn http_host(host: &str, port: u16, method: &str, path: &str) -> io::Result<Value> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()
        .request(method, &format!("http://{authority}{path}"))
        .call()
        .map_err(|e| io::Error::other(e.to_string()))?
        .into_json()
}

fn http(port: u16, method: &str, path: &str) -> io::Result<Value> {
    http_host("127.0.0.1", port, method, path)
}

fn file_port(profile: &Path) -> Option<u16> {
    std::fs::read_to_string(profile.join("DevToolsActivePort"))
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// The port of a browser already serving this profile (the file outlives a
/// crashed browser, so the port must also answer).
fn live_port(profile: &Path) -> Option<u16> {
    file_port(profile).filter(|p| http(*p, "GET", "/json/version").is_ok())
}

fn pages(port: u16) -> io::Result<Vec<String>> {
    Ok(http(port, "GET", "/json/list")?
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "page")
        .filter_map(|t| t["id"].as_str().map(String::from))
        .collect())
}

fn launch(profile: &Path, headless: bool, exec: &Exec<'_>) -> Result<u16> {
    let exe = browser_exe().ok_or_else(|| {
        ToolError::Failed(
            "nenhum Edge/Chrome/Chromium encontrado; defina ZERITH_BROWSER com o caminho do executável"
                .into(),
        )
    })?;
    std::fs::create_dir_all(profile)?;
    let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
    let mut cmd = Command::new(&exe);
    cmd.arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--no-first-run", "--no-default-browser-check"]);
    if headless {
        cmd.arg("--headless=new");
    }
    // ponytail: never killed here; it outlives this process so the next call
    // (or the next app start) reattaches. `close` is what ends it.
    cmd.arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ToolError::Failed(format!("não consegui abrir {}: {e}", exe.display())))?;
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        exec.check()?;
        if let Some(p) = live_port(profile) {
            return Ok(p);
        }
        std::thread::sleep(POLL);
    }
    Err(ToolError::Failed(
        "o navegador abriu mas não expôs a porta DevTools em 20 s".into(),
    ))
}

fn discover_kitesurf_path(host: &str, port: u16, default_path: &str) -> String {
    if let Ok(list) = http_host(host, port, "GET", "/json/list") {
        if let Some(arr) = list.as_array() {
            for item in arr {
                if let Some(ws_url) = item["webSocketDebuggerUrl"].as_str() {
                    if let Ok((_, _, p)) = parse_cdp_endpoint(ws_url) {
                        return p;
                    }
                }
                if let Some(id) = item["id"].as_str() {
                    return format!("/devtools/page/{id}");
                }
            }
        }
    }
    if let Ok(ver) = http_host(host, port, "GET", "/json/version") {
        if let Some(ws_url) = ver["webSocketDebuggerUrl"].as_str() {
            if let Ok((_, _, p)) = parse_cdp_endpoint(ws_url) {
                return p;
            }
        }
    }
    default_path.to_string()
}

/// Attaches to a Cloudflare Kitesurf CDP endpoint (`workerd` / WebSocket),
/// reusing the existing socket when already connected to the same endpoint.
fn attach_kitesurf<'a>(
    slot: &'a mut Option<Cdp>,
    raw_endpoint: &str,
    exec: &Exec<'_>,
) -> Result<&'a mut Cdp> {
    let (host, port, raw_path) = parse_cdp_endpoint(raw_endpoint)?;
    let key = format!("{host}:{port}{raw_path}");
    if slot.as_ref().is_some_and(
        |c| !matches!(&c.backend, BackendKind::Kitesurf { endpoint } if endpoint == &key),
    ) {
        *slot = None;
    }
    if slot.is_none() {
        let ws_path = if raw_endpoint.trim().starts_with("http://") {
            discover_kitesurf_path(&host, port, &raw_path)
        } else {
            raw_path
        };
        let ws = Ws::connect_host(&host, port, &ws_path).map_err(|e| fail(exec, e))?;
        let mut c = Cdp {
            ws,
            backend: BackendKind::Kitesurf {
                endpoint: key.clone(),
            },
            target: ws_path,
            frame: String::new(),
            id: 0,
            seen: Vec::new(),
            console_errors: Vec::new(),
        };
        // Kitesurf implements a streamlined CDP subset; tolerate missing
        // frame-tree metadata or Runtime domain on stateless isolates.
        let _ = c.call("Page.enable", json!({}), exec);
        let _ = c.call("Runtime.enable", json!({}), exec);
        if let Ok(tree) = c.call("Page.getFrameTree", json!({}), exec) {
            c.frame = tree["frameTree"]["frame"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        *slot = Some(c);
    }
    Ok(slot.as_mut().expect("kitesurf session set above"))
}

/// The session for the current page: reused while its tab exists, else a
/// fresh connection to the first tab (launching the browser if none runs).
fn attach_chromium<'a>(
    slot: &'a mut Option<Cdp>,
    profile: &Path,
    headless: bool,
    exec: &Exec<'_>,
) -> Result<&'a mut Cdp> {
    let (port, list) = match file_port(profile).and_then(|p| pages(p).ok().map(|l| (p, l))) {
        Some(x) => x,
        None => {
            let p = launch(profile, headless, exec)?;
            (p, pages(p).map_err(|e| fail(exec, e))?)
        }
    };
    if slot.as_ref().is_some_and(|c| {
        !matches!(c.backend, BackendKind::Chromium { port: cp } if cp == port)
            || !list.contains(&c.target)
    }) {
        *slot = None;
    }
    if slot.is_none() {
        let target = match list.into_iter().next() {
            Some(t) => t,
            None => http(port, "PUT", "/json/new?about:blank").map_err(|e| fail(exec, e))?["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        let ws =
            Ws::connect(port, &format!("/devtools/page/{target}")).map_err(|e| fail(exec, e))?;
        let mut c = Cdp {
            ws,
            backend: BackendKind::Chromium { port },
            target,
            frame: String::new(),
            id: 0,
            seen: Vec::new(),
            console_errors: Vec::new(),
        };
        c.call("Page.enable", json!({}), exec)?;
        let _ = c.call("Runtime.enable", json!({}), exec);
        let tree = c.call("Page.getFrameTree", json!({}), exec)?;
        c.frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        *slot = Some(c);
    }
    Ok(slot.as_mut().expect("session set above"))
}

fn close(profile: &Path, exec: &Exec<'_>) -> Result<String> {
    let Some(port) = live_port(profile) else {
        return Ok("O navegador não está aberto.".into());
    };
    let v = http(port, "GET", "/json/version").map_err(|e| fail(exec, e))?;
    let path = v["webSocketDebuggerUrl"]
        .as_str()
        .and_then(|u| u.find("/devtools/").map(|i| u[i..].to_string()))
        .ok_or_else(|| ToolError::Failed("o navegador não informou o endereço DevTools".into()))?;
    let mut ws = Ws::connect(port, &path).map_err(|e| fail(exec, e))?;
    ws.send(&json!({"id": 1, "method": "Browser.close"}).to_string())
        .map_err(|e| fail(exec, e))?;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && live_port(profile).is_some() {
        std::thread::sleep(POLL);
    }
    Ok("Navegador fechado.".into())
}

fn fail(exec: &Exec<'_>, e: io::Error) -> ToolError {
    exec.check().err().unwrap_or(ToolError::Io(e))
}

// --- DevTools protocol -----------------------------------------------------------

struct Cdp {
    ws: Ws,
    backend: BackendKind,
    target: String,
    /// Main frame id: events of other frames do not count as navigation.
    frame: String,
    id: u64,
    /// Main-frame `Page.*` events since the action started.
    seen: Vec<String>,
    /// Console errors/warnings and uncaught exceptions captured since `navigate`.
    console_errors: Vec<String>,
}

fn format_cdp_console_event(method: &str, params: &Value) -> Option<String> {
    match method {
        "Runtime.exceptionThrown" => {
            let det = &params["exceptionDetails"];
            let msg = det["exception"]["description"]
                .as_str()
                .or_else(|| det["text"].as_str())
                .unwrap_or("exceção não tratada");
            let loc = det["url"]
                .as_str()
                .filter(|u| !u.is_empty())
                .map(|u| {
                    let line = det["lineNumber"].as_u64().map(|n| n + 1).unwrap_or(1);
                    format!(" ({u}:{line})")
                })
                .unwrap_or_default();
            Some(format!("[exception] {msg}{loc}"))
        }
        "Runtime.consoleAPICalled" => {
            let kind = params["type"].as_str()?;
            if !matches!(kind, "error" | "warning" | "assert") {
                return None;
            }
            let text = params["args"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| {
                            a["value"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| a["description"].as_str().map(str::to_string))
                                .or_else(|| (!a["value"].is_null()).then(|| a["value"].to_string()))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(format!("[console.{kind}] {text}"))
            }
        }
        _ => None,
    }
}

impl Cdp {
    fn send(&mut self, method: &str, params: Value) -> io::Result<u64> {
        self.id += 1;
        self.ws
            .send(&json!({"id": self.id, "method": method, "params": params}).to_string())?;
        Ok(self.id)
    }

    fn recv(&mut self, exec: &Exec<'_>) -> io::Result<Value> {
        let stop = || {
            exec.check()
                .map_err(|e| io::Error::new(io::ErrorKind::TimedOut, e.to_string()))
        };
        let text = read_message(
            &mut Patient {
                r: &mut self.ws.r,
                stop,
            },
            &mut self.ws.w,
        )?;
        let v: Value = serde_json::from_str(&text)?;
        if let Some(m) = v["method"].as_str() {
            let p = &v["params"];
            if let Some(entry) = format_cdp_console_event(m, p) {
                let capped: String = entry.chars().take(400).collect();
                self.console_errors.push(capped);
                if self.console_errors.len() > 40 {
                    self.console_errors.remove(0);
                }
            }
            if self.frame.is_empty()
                || p["frameId"]
                    .as_str()
                    .or(p["frame"]["id"].as_str())
                    .is_none_or(|f| f == self.frame)
            {
                self.seen.push(m.to_string());
                if self.seen.len() > 64 {
                    self.seen.remove(0);
                }
            }
        }
        Ok(v)
    }

    /// One request; events read while waiting go to `seen`.
    fn call(&mut self, method: &str, params: Value, exec: &Exec<'_>) -> Result<Value> {
        let id = self.send(method, params).map_err(|e| fail(exec, e))?;
        loop {
            let mut v = self.recv(exec).map_err(|e| fail(exec, e))?;
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    return Err(ToolError::Failed(format!(
                        "{method}: {}",
                        e["message"].as_str().unwrap_or("erro do navegador")
                    )));
                }
                return Ok(v["result"].take());
            }
        }
    }

    /// `Runtime.evaluate` by value; a JS exception is an error. Returns the
    /// `RemoteObject` (`value`, `type`).
    fn eval(&mut self, expr: &str, exec: &Exec<'_>) -> Result<Value> {
        let mut r = self.call(
            "Runtime.evaluate",
            json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
            exec,
        )?;
        if let Some(d) = r.get("exceptionDetails") {
            let msg = d["exception"]["description"]
                .as_str()
                .or(d["text"].as_str())
                .unwrap_or("erro");
            return Err(ToolError::Failed(format!("JavaScript: {msg}")));
        }
        Ok(r["result"].take())
    }

    fn page(&mut self, exec: &Exec<'_>) -> Result<String> {
        let v = self.eval("[document.title, location.href]", exec)?;
        Ok(format!(
            "Página [{}]: {} — {}",
            self.backend.badge(),
            v["value"][0].as_str().unwrap_or(""),
            v["value"][1].as_str().unwrap_or("")
        ))
    }

    /// Waits up to `dur` for one of `events`. A frame is only started when
    /// bytes are already waiting, so the local timeout never cuts one.
    fn wait(&mut self, events: &[&str], dur: Duration, exec: &Exec<'_>) -> Result<bool> {
        let until = Instant::now() + dur;
        loop {
            if self.seen.iter().any(|s| events.contains(&s.as_str())) {
                return Ok(true);
            }
            let ready = self.ws.r.fill_buf().map(|b| !b.is_empty());
            match ready {
                Ok(true) => {
                    self.recv(exec).map_err(|e| fail(exec, e))?;
                }
                Ok(false) => {
                    return Err(fail(
                        exec,
                        io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "o navegador fechou a conexão",
                        ),
                    ))
                }
                Err(e) if is_timeout(&e) => {
                    exec.check()?;
                    if Instant::now() >= until {
                        return Ok(false);
                    }
                }
                Err(e) => return Err(fail(exec, e)),
            }
        }
    }

    /// After input that may navigate: waits briefly for a main-frame load to
    /// start and, if one did, for it to finish.
    // ponytail: frameStoppedLoading also ends the wait (bfcache restores
    // fire no load event); a page that navigates after 1 s is not awaited.
    fn settle(&mut self, exec: &Exec<'_>) -> Result<()> {
        let started = ["Page.frameStartedLoading", "Page.frameNavigated"];
        if self.wait(&started, Duration::from_secs(1), exec)? {
            self.wait(
                &["Page.loadEventFired", "Page.frameStoppedLoading"],
                LOAD_WAIT,
                exec,
            )?;
        }
        Ok(())
    }
}

// --- WebSocket (RFC 6455, client side, no TLS) ---------------------------------

struct Ws {
    r: BufReader<TcpStream>,
    w: TcpStream,
}

impl Ws {
    fn connect(port: u16, path: &str) -> io::Result<Ws> {
        Self::connect_host("127.0.0.1", port, path)
    }

    fn connect_host(host: &str, port: u16, path: &str) -> io::Result<Ws> {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::other(format!("não resolveu {host}:{port}")))?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut w = s.try_clone()?;
        let key = b64_encode(&random::<16>());
        let host_hdr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        // No Origin header: Chromium refuses DevTools sockets with an
        // unlisted Origin, and accepts them without one.
        write!(
            w,
            "GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut r = BufReader::new(s);
        let mut status = String::new();
        r.read_line(&mut status)?;
        if !status.contains(" 101") {
            return Err(io::Error::other(format!(
                "o DevTools recusou a conexão: {}",
                status.trim()
            )));
        }
        // ponytail: Sec-WebSocket-Accept is not checked (needs SHA-1, not in
        // this crate); the peer is a localhost port read from the browser's
        // own profile file.
        loop {
            let mut line = String::new();
            if r.read_line(&mut line)? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if line.trim().is_empty() {
                break;
            }
        }
        r.get_ref().set_read_timeout(Some(POLL))?;
        Ok(Ws { r, w })
    }

    fn send(&mut self, text: &str) -> io::Result<()> {
        self.w
            .write_all(&encode_frame(0x1, text.as_bytes(), Some(random())))
    }
}

/// Non-cryptographic random bytes from std's randomly seeded hasher: enough
/// for a handshake nonce and frame masks on localhost.
fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    for (i, c) in out.chunks_mut(8).enumerate() {
        c.copy_from_slice(&RandomState::new().hash_one(i).to_le_bytes()[..c.len()]);
    }
    out
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// A reader that sits out the socket's poll timeout, asking `stop` each time
/// (cancel / deadline), so a frame is never cut by the poll interval itself.
struct Patient<'a, F: Fn() -> io::Result<()>> {
    r: &'a mut BufReader<TcpStream>,
    stop: F,
}

impl<F: Fn() -> io::Result<()>> Read for Patient<'_, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.r.read(buf) {
                Err(e) if is_timeout(&e) => (self.stop)()?,
                r => return r,
            }
        }
    }
}

/// One final frame; masked when `mask` is given (client to server).
fn encode_frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut f = vec![0x80 | opcode];
    let m = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => f.push(m | n as u8),
        n if n <= 0xffff => {
            f.push(m | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(m | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(k) => {
            f.extend_from_slice(&k);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
        }
        None => f.extend_from_slice(payload),
    }
    f
}

/// One frame as `(fin, opcode, payload)`, unmasked.
fn read_frame(r: &mut impl Read) -> io::Result<(bool, u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h)?;
    let len = match h[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            u64::from_be_bytes(b)
        }
        n => u64::from(n),
    };
    if len > MAX_MESSAGE as u64 {
        return Err(io::Error::other("mensagem do navegador grande demais"));
    }
    let mask = if h[1] & 0x80 != 0 {
        let mut k = [0u8; 4];
        r.read_exact(&mut k)?;
        Some(k)
    } else {
        None
    };
    let mut p = vec![0u8; len as usize];
    r.read_exact(&mut p)?;
    if let Some(k) = mask {
        for (i, b) in p.iter_mut().enumerate() {
            *b ^= k[i % 4];
        }
    }
    Ok((h[0] & 0x80 != 0, h[0] & 0x0f, p))
}

/// The next data message, reassembled from fragments; pings are answered on
/// `w`, a close frame is an error.
fn read_message(r: &mut impl Read, w: &mut impl Write) -> io::Result<String> {
    let mut msg = Vec::new();
    loop {
        let (fin, op, p) = read_frame(r)?;
        match op {
            0x0..=0x2 => {
                msg.extend_from_slice(&p);
                if msg.len() > MAX_MESSAGE {
                    return Err(io::Error::other("mensagem do navegador grande demais"));
                }
                if fin {
                    return String::from_utf8(msg)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
                }
            }
            0x8 => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "o navegador fechou a conexão",
                ))
            }
            0x9 => w.write_all(&encode_frame(0xA, &p, Some(random())))?,
            _ => {} // pong
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_every_length_encoding() {
        for (n, marker) in [
            (0, 0),
            (125, 125),
            (126, 126),
            (65_535, 126),
            (65_536, 127),
            (70_000, 127),
        ] {
            let p: Vec<u8> = (0..n).map(|i| (i * 31 + 7) as u8).collect();
            let f = encode_frame(0x1, &p, Some([9, 8, 7, 6]));
            assert_eq!(f[0], 0x81, "fin + text");
            assert_eq!(f[1], 0x80 | marker, "mask bit + length marker for {n}");
            if n > 0 {
                assert_ne!(&f[f.len() - n..], &p[..], "payload is masked");
            }
            let (fin, op, q) = read_frame(&mut &f[..]).unwrap();
            assert!(fin);
            assert_eq!(op, 0x1);
            assert_eq!(q, p);
        }
    }

    #[test]
    fn a_message_is_reassembled_and_pings_are_answered() {
        let mut first = encode_frame(0x1, b"{\"id\":", None);
        first[0] &= 0x7f; // not final
        let mut wire = first;
        wire.extend(encode_frame(0x9, b"hi", None));
        wire.extend(encode_frame(0x0, b"1}", None));
        wire.extend(encode_frame(0x8, b"", None));
        let mut r = &wire[..];
        let mut out = Vec::new();
        assert_eq!(read_message(&mut r, &mut out).unwrap(), "{\"id\":1}");
        assert_eq!(out[0], 0x8A, "pong");
        assert_ne!(out[1] & 0x80, 0, "client frames are masked");
        assert_eq!(
            read_frame(&mut &out[..]).unwrap(),
            (true, 0xA, b"hi".to_vec())
        );
        let e = read_message(&mut r, &mut out).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn targets_and_urls_are_checked() {
        assert_eq!(
            find(&json!({"ref": "[3]"})).unwrap(),
            "document.querySelector('[data-zerith-ref=\"3\"]')"
        );
        assert_eq!(
            find(&json!({"selector": "a[title=\"x\"]"})).unwrap(),
            "document.querySelector(\"a[title=\\\"x\\\"]\")"
        );
        assert!(find(&json!({})).is_err());
        assert_eq!(nav_url("example.com").unwrap(), "https://example.com");
        assert!(nav_url("file:///etc/passwd").is_err());
        assert!(label(&json!({"action": "explode"})).is_err());
        // The approval card names the secret, never its value.
        assert_eq!(
            label(&json!({"action": "fill_secret", "secret": "olt-huawei", "ref": 3})).unwrap(),
            "navegador fill_secret olt-huawei no campo ref 3"
        );
        assert!(label(&json!({"action": "fill_secret", "ref": 3})).is_err());
        assert_eq!(
            label(&json!({"action": "navigate", "url": "https://a.b"})).unwrap(),
            "navegador navigate https://a.b"
        );
    }

    #[test]
    fn lan_cgnat_and_localhost_urls_route_to_local_chromium() {
        for local_url in [
            "http://localhost:5183",
            "https://127.0.0.1:8443/login",
            "http://[::1]:3000",
            "https://192.168.1.1/webfig/",
            "https://10.20.30.1:4443",
            "http://172.16.0.1",
            "http://172.31.255.254",
            "http://100.64.10.1:8080",
            "http://100.127.255.1",
            "https://olt01.core.lan",
            "http://mikrotik.local",
            "http://router",
        ] {
            assert!(
                is_private_or_local_url(local_url),
                "expected local/private classification for {local_url}"
            );
            assert_eq!(
                select_target_engine(
                    BrowserEngine::Auto,
                    Some("ws://127.0.0.1:8787/cdp"),
                    Some(local_url)
                ),
                (BrowserEngine::Chromium, false),
                "Auto must route {local_url} directly to local Chromium"
            );
        }

        for public_url in [
            "https://cloudflare.com",
            "https://docs.rs/tokio/latest",
            "https://1.1.1.1",
            "http://172.32.0.1",
            "http://100.128.0.1",
        ] {
            assert!(
                !is_private_or_local_url(public_url),
                "expected public classification for {public_url}"
            );
            assert_eq!(
                select_target_engine(
                    BrowserEngine::Auto,
                    Some("ws://127.0.0.1:8787/cdp"),
                    Some(public_url)
                ),
                (BrowserEngine::Kitesurf, true),
                "Auto with Kitesurf URL must route {public_url} to Kitesurf with Chromium fallback"
            );
            assert_eq!(
                select_target_engine(BrowserEngine::Auto, None, Some(public_url)),
                (BrowserEngine::Chromium, false),
                "Auto without Kitesurf URL defaults to Chromium"
            );
        }

        assert_eq!(
            select_target_engine(BrowserEngine::Kitesurf, None, Some("https://192.168.1.1")),
            (BrowserEngine::Kitesurf, false)
        );
        assert_eq!(
            select_target_engine(
                BrowserEngine::Chromium,
                Some("ws://127.0.0.1:8787/cdp"),
                Some("https://cloudflare.com")
            ),
            (BrowserEngine::Chromium, false)
        );
    }

    #[test]
    fn kitesurf_cdp_endpoints_parse_cleanly() {
        assert_eq!(
            parse_cdp_endpoint("ws://127.0.0.1:8787/cdp").unwrap(),
            ("127.0.0.1".into(), 8787, "/cdp".into())
        );
        assert_eq!(
            parse_cdp_endpoint("http://localhost:8787").unwrap(),
            ("localhost".into(), 8787, "/cdp".into())
        );
        assert_eq!(
            parse_cdp_endpoint("127.0.0.1:9222/devtools/page/abc").unwrap(),
            ("127.0.0.1".into(), 9222, "/devtools/page/abc".into())
        );
        assert_eq!(
            parse_cdp_endpoint("ws://[::1]:8787/cdp").unwrap(),
            ("::1".into(), 8787, "/cdp".into())
        );
        assert!(parse_cdp_endpoint("").is_err());
    }

    #[test]
    fn format_cdp_console_event_captures_errors_warnings_and_exceptions_only() {
        let err = format_cdp_console_event(
            "Runtime.consoleAPICalled",
            &json!({
                "type": "error",
                "args": [{"value": "Uncaught"}, {"description": "TypeError: x is undefined"}]
            }),
        );
        assert_eq!(
            err.as_deref(),
            Some("[console.error] Uncaught TypeError: x is undefined")
        );

        let info = format_cdp_console_event(
            "Runtime.consoleAPICalled",
            &json!({"type": "log", "args": [{"value": "hello"}]}),
        );
        assert!(info.is_none());

        let exc = format_cdp_console_event(
            "Runtime.exceptionThrown",
            &json!({
                "exceptionDetails": {
                    "text": "Uncaught",
                    "url": "http://localhost:5183/src/App.tsx",
                    "lineNumber": 41,
                    "exception": {"description": "ReferenceError: foo is not defined"}
                }
            }),
        );
        assert_eq!(
            exc.as_deref(),
            Some("[exception] ReferenceError: foo is not defined (http://localhost:5183/src/App.tsx:42)")
        );
    }

    #[test]
    fn viewport_spec_and_labels_validate_bounds_and_defaults() {
        assert_eq!(
            viewport_spec(&json!({"width": 390, "height": 844})).unwrap(),
            ViewportSpec::Set {
                width: 390,
                height: 844,
                mobile: true,
                device_scale_factor: 2.0,
            }
        );
        assert_eq!(
            viewport_spec(&json!({"width": 1440, "height": 900})).unwrap(),
            ViewportSpec::Set {
                width: 1440,
                height: 900,
                mobile: false,
                device_scale_factor: 1.0,
            }
        );
        assert_eq!(
            viewport_spec(&json!({"reset": true})).unwrap(),
            ViewportSpec::Reset
        );
        assert!(viewport_spec(&json!({"width": 100, "height": 800})).is_err());
        assert!(
            viewport_spec(&json!({"width": 800, "height": 600, "device_scale_factor": 0})).is_err()
        );
        assert_eq!(
            label(&json!({"action": "viewport", "width": 390, "height": 844})).unwrap(),
            "navegador viewport 390x844"
        );
        assert_eq!(
            label(&json!({"action": "viewport", "reset": true})).unwrap(),
            "navegador viewport reset"
        );
        assert_eq!(
            label(&json!({"action": "screenshot", "full_page": true})).unwrap(),
            "navegador screenshot full_page"
        );
    }
}
