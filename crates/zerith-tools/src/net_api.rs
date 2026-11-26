//! Ferramenta `net_api`: cliente HTTP/REST estruturado para equipamentos de rede
//! e virtualização (RouterOS v7 `/rest/...`, Proxmox VE `:8006/api2/json/...`,
//! RESTCONF `/restconf/data/...` e APIs HTTP/HTTPS genéricas).
//!
//! Reutiliza `zerith_workflows::zabbix::build_http_agent` (`ureq` + TLS opcional
//! `insecure_tls`) e `zerith_workflows::zabbix::vault_secret`. Nunca aceita senhas
//! ou tokens em texto puro e redige segredos/cabeçalhos de autenticação de todas
//! as saídas e mensagens de erro.

use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::{Action, Mode, Sandbox};
use zerith_workflows::zabbix::{build_http_agent, vault_secret};

use crate::fntool::opt_str;
use crate::network::{load_inventory, DeviceEntry, Vendor};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const MAX_BODY_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(NetApiTool::new())]
}

struct NetApiTool {
    spec: ToolSpec,
}

impl NetApiTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: "net_api".into(),
                description: "Cliente HTTP/REST para equipamentos de rede e infraestrutura \
                    (RouterOS v7 `/rest/...`, Proxmox VE `:8006/api2/json/...`, RESTCONF \
                    `/restconf/data/...`, APIs HTTP/HTTPS). Aceita `url` completa ou `device` \
                    do inventário + `path`, autenticação via cofre (`secret_ref`, nunca texto \
                    puro: `none`, `basic`, `bearer`, `pve_token`, `header`) e `insecure_tls` \
                    para certificados autoassinados."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "URL completa (http:// ou https://). Alternativa a `device`."
                        },
                        "device": {
                            "type": "string",
                            "description": "nome do equipamento no inventory.toml (ou `cliente/nome`); monta URL base pelo vendor (Proxmox :8006/api2/json, RouterOS /rest, genérico https://host)"
                        },
                        "client": {
                            "type": "string",
                            "description": "cliente opcional para desambiguar `device`"
                        },
                        "path": {
                            "type": "string",
                            "description": "caminho opcional concatenado à URL base (ex.: /system/resource, /nodes, /restconf/data/...)"
                        },
                        "method": {
                            "type": "string",
                            "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"],
                            "description": "método HTTP (padrão GET)"
                        },
                        "auth_mode": {
                            "type": "string",
                            "enum": ["none", "basic", "bearer", "pve_token", "header"],
                            "description": "modo de autenticação (padrão: none se sem secret_ref; basic/bearer se secret_ref presente)"
                        },
                        "username": {
                            "type": "string",
                            "description": "usuário para `auth_mode: \"basic\"` (herda `user` do dispositivo se `device` informado)"
                        },
                        "secret_ref": {
                            "type": "string",
                            "description": "nome do segredo no cofre (nunca envie senha/token em texto puro; herda `secret_ref` do dispositivo quando `device` informado)"
                        },
                        "auth_header": {
                            "type": "string",
                            "description": "nome do cabeçalho HTTP quando `auth_mode: \"header\"` (ex.: X-API-Key)"
                        },
                        "query": {
                            "type": "object",
                            "description": "parâmetros de query string como objeto JSON (chave -> valor)"
                        },
                        "headers": {
                            "type": "object",
                            "description": "cabeçalhos HTTP extras como objeto JSON (ex.: Accept: application/yang-data+json)"
                        },
                        "body": {
                            "description": "corpo da requisição para POST/PUT/PATCH (objeto/valor JSON ou string)"
                        },
                        "insecure_tls": {
                            "type": "boolean",
                            "description": "aceita certificado HTTPS autoassinado (padrão false; útil em RouterOS/Proxmox)"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "timeout em segundos (5..=120, padrão 20)"
                        }
                    }
                }),
                returns: "status HTTP, cabeçalhos relevantes e corpo (JSON formatado quando válido, limite 256 KB)",
                permission: Permission::Network,
                timeout: Duration::from_secs(125),
                cancellable: true,
                local: false,
                source: "built-in".into(),
            },
        }
    }
}

fn normalized_method(args: &Value) -> String {
    opt_str(args, "method")
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase()
}

fn is_read_only_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
}

fn validate_method(method: &str) -> Result<&'static str> {
    match method {
        "GET" => Ok("GET"),
        "HEAD" => Ok("HEAD"),
        "POST" => Ok("POST"),
        "PUT" => Ok("PUT"),
        "PATCH" => Ok("PATCH"),
        "DELETE" => Ok("DELETE"),
        other => Err(ToolError::BadArguments(format!(
            "método HTTP não suportado: '{other}' (use GET, HEAD, POST, PUT, PATCH ou DELETE)"
        ))),
    }
}

fn extract_host_label(args: &Value) -> String {
    if let Some(url) = opt_str(args, "url") {
        if let Some(host) = host_from_url(url) {
            return host;
        }
    }
    if let Some(dev) = opt_str(args, "device") {
        return dev.trim().to_string();
    }
    "api".to_string()
}

fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    if host_port.starts_with('[') {
        let end = host_port.find(']')?;
        Some(host_port[..=end].to_string())
    } else {
        let host = host_port.split(':').next()?;
        (!host.is_empty()).then(|| host.to_string())
    }
}

impl Tool for NetApiTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        let m = normalized_method(args);
        is_read_only_method(&m)
    }

    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        let method = normalized_method(args);
        if !is_read_only_method(&method)
            && (ctx.policy.preset.sandbox == Sandbox::ReadOnly || ctx.policy.mode() == Mode::Plan)
        {
            return Err(ToolError::Denied(format!(
                "requisição HTTP de escrita (`{method}`) bloqueada no modo plan/read-only"
            )));
        }
        let host = extract_host_label(args);
        Ok(Some(Action::Network(format!("net_api:{method}:{host}"))))
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        reject_plaintext_credentials(args)?;

        let method = validate_method(&normalized_method(args))?;
        let device = if opt_str(args, "device").is_some() {
            Some(lookup_inventory_device(args, ctx)?)
        } else {
            None
        };

        let url = resolve_request_url(args, device.as_ref())?;
        let (auth_header, secrets) = build_auth_header(args, device.as_ref(), |name| {
            vault_secret(name).map_err(|e| ToolError::Failed(e.to_string()))
        })?;
        for s in &secrets {
            exec.host.note_secret(s);
        }

        let extra_headers = parse_extra_headers(args)?;
        let body = prepare_body(args)?;
        let insecure_tls = args
            .get("insecure_tls")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(5, 120);

        let req_spec = PreparedRequest {
            method,
            url,
            auth_header,
            extra_headers,
            body,
            insecure_tls,
            timeout: Duration::from_secs(timeout_secs),
            secrets,
        };

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = execute_http_request(req_spec);
            let _ = tx.send(res);
        });

        loop {
            exec.check()?;
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(res) => return res,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ToolError::Failed(
                        "chamada HTTP (`net_api`) interrompida inesperadamente".into(),
                    ))
                }
            }
        }
    }
}

// --- RFC 4648 Base64 Encoder (zero external crates) -------------------------

pub(crate) fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// --- Request construction & validation --------------------------------------

fn reject_plaintext_credentials(args: &Value) -> Result<()> {
    for forbidden in ["password", "token", "secret", "api_key", "apikey"] {
        if args.get(forbidden).is_some_and(|v| !v.is_null()) {
            return Err(ToolError::BadArguments(format!(
                "credencial em texto puro (`{forbidden}`) não é aceita em `net_api`: \
                 grave no cofre (`zerith secret set <nome>`) e informe `secret_ref`"
            )));
        }
    }
    Ok(())
}

fn lookup_inventory_device(args: &Value, ctx: &CallCtx<'_>) -> Result<DeviceEntry> {
    let dev_arg = opt_str(args, "device")
        .ok_or_else(|| ToolError::BadArguments("informe `url` ou `device`".into()))?;
    let global_root = zerith_core::Paths::resolve().root().to_path_buf();
    let inv = load_inventory(ctx.workspace, Some(&global_root)).map_err(ToolError::Failed)?;
    let (client_hint, name_hint) = match dev_arg.split_once('/') {
        Some((c, n)) => (Some(c.trim()), n.trim()),
        None => (opt_str(args, "client"), dev_arg.trim()),
    };
    let found = inv
        .into_iter()
        .find(|d| {
            let name_ok = d.name.eq_ignore_ascii_case(name_hint);
            let client_ok = client_hint.is_none_or(|c| {
                d.client
                    .as_deref()
                    .is_some_and(|dc| dc.eq_ignore_ascii_case(c))
            });
            name_ok && client_ok
        })
        .ok_or_else(|| {
            ToolError::Failed(format!(
                "dispositivo '{dev_arg}' não encontrado no inventário (inventory.toml)"
            ))
        })?;
    if found.host.trim().is_empty() {
        return Err(ToolError::Failed(format!(
            "dispositivo '{}' não possui `host` cadastrado no inventário",
            found.display_qualified_name()
        )));
    }
    Ok(found)
}

fn format_host_for_url(host: &str) -> String {
    let h = host.trim();
    if h.contains(':') && !(h.starts_with('[') && h.ends_with(']')) {
        format!("[{h}]")
    } else {
        h.to_string()
    }
}

pub(crate) fn base_url_for_device(dev: &DeviceEntry) -> String {
    let host = format_host_for_url(&dev.host);
    if dev.vendor == Vendor::Proxmox || dev.port == 8006 {
        return format!("https://{host}:8006/api2/json");
    }
    let port_suffix = match dev.port {
        0 | 22 | 23 | 443 => String::new(),
        p => format!(":{p}"),
    };
    match dev.vendor {
        Vendor::MikrotikRouterOs => format!("https://{host}{port_suffix}/rest"),
        _ => format!("https://{host}{port_suffix}"),
    }
}

fn join_url_path(base: &str, path: Option<&str>) -> String {
    let base = base.trim();
    let Some(p) = path.map(str::trim).filter(|s| !s.is_empty()) else {
        return base.to_string();
    };
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        p.trim_start_matches('/')
    )
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn append_query_params(mut url: String, query: Option<&Value>) -> Result<String> {
    let Some(q) = query else {
        return Ok(url);
    };
    if q.is_null() {
        return Ok(url);
    }
    let Some(map) = q.as_object() else {
        return Err(ToolError::BadArguments(
            "`query` precisa ser um objeto JSON (chave -> valor)".into(),
        ));
    };
    if map.is_empty() {
        return Ok(url);
    }
    let mut first = !url.contains('?');
    for (k, v) in map {
        let val_str = match v {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        url.push(if first { '?' } else { '&' });
        first = false;
        url.push_str(&percent_encode(k));
        url.push('=');
        url.push_str(&percent_encode(&val_str));
    }
    Ok(url)
}

pub(crate) fn resolve_request_url(args: &Value, device: Option<&DeviceEntry>) -> Result<String> {
    let base = if let Some(u) = opt_str(args, "url") {
        let trimmed = u.trim();
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return Err(ToolError::BadArguments(
                "`url` deve começar com http:// ou https://".into(),
            ));
        }
        trimmed.to_string()
    } else if let Some(dev) = device {
        base_url_for_device(dev)
    } else {
        return Err(ToolError::BadArguments(
            "informe `url` (http:// ou https://) ou `device` (do inventory.toml)".into(),
        ));
    };

    let joined = join_url_path(&base, opt_str(args, "path"));
    append_query_params(joined, args.get("query"))
}

fn validate_header_token(field: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolError::BadArguments(format!(
            "`{field}` não pode ficar vazio"
        )));
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || matches!(c, '\r' | '\n'))
    {
        return Err(ToolError::BadArguments(format!(
            "`{field}` contém caractere de controle inválido"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) type AuthHeaderResult = (Option<(String, String)>, Vec<String>);

pub(crate) fn build_auth_header(
    args: &Value,
    device: Option<&DeviceEntry>,
    mut lookup_secret: impl FnMut(&str) -> Result<String>,
) -> Result<AuthHeaderResult> {
    reject_plaintext_credentials(args)?;

    let secret_ref = opt_str(args, "secret_ref").or_else(|| {
        device
            .and_then(|d| d.secret_ref.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });

    let username = opt_str(args, "username")
        .or_else(|| device.map(|d| d.user.trim()).filter(|s| !s.is_empty()));

    let explicit_mode = opt_str(args, "auth_mode").map(|m| m.trim().to_ascii_lowercase());
    let mode = match explicit_mode.as_deref() {
        Some(m) => m,
        None => {
            if secret_ref.is_none() {
                "none"
            } else if username.is_some() {
                "basic"
            } else {
                "bearer"
            }
        }
    };

    match mode {
        "none" => Ok((None, Vec::new())),
        "basic" => {
            let user = username.ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"basic\" exige `username` (ou `user` cadastrado no `device`)"
                        .into(),
                )
            })?;
            let sec_name = secret_ref.ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"basic\" exige `secret_ref` (nome do segredo no cofre)".into(),
                )
            })?;
            let secret = lookup_secret(sec_name)?.trim().to_string();
            if secret.is_empty() {
                return Err(ToolError::Failed(format!(
                    "segredo '{sec_name}' está vazio no cofre"
                )));
            }
            let encoded = base64_encode(format!("{user}:{secret}").as_bytes());
            let header_val = format!("Basic {encoded}");
            let secrets = vec![secret, encoded, header_val.clone()];
            Ok((Some(("Authorization".into(), header_val)), secrets))
        }
        "bearer" => {
            let sec_name = secret_ref.ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"bearer\" exige `secret_ref` (nome do segredo no cofre)".into(),
                )
            })?;
            let raw = lookup_secret(sec_name)?.trim().to_string();
            let token = raw
                .strip_prefix("Bearer ")
                .or_else(|| raw.strip_prefix("bearer "))
                .unwrap_or(&raw)
                .trim()
                .to_string();
            if token.is_empty() {
                return Err(ToolError::Failed(format!(
                    "segredo '{sec_name}' está vazio no cofre"
                )));
            }
            let header_val = format!("Bearer {token}");
            let secrets = vec![token, header_val.clone()];
            Ok((Some(("Authorization".into(), header_val)), secrets))
        }
        "pve_token" => {
            let sec_name = secret_ref.ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"pve_token\" exige `secret_ref` (ex.: `USER@REALM!TOKENID=UUID` no cofre)".into(),
                )
            })?;
            let raw = lookup_secret(sec_name)?.trim().to_string();
            let token = raw
                .strip_prefix("PVEAPIToken=")
                .unwrap_or(&raw)
                .trim()
                .to_string();
            if token.is_empty() {
                return Err(ToolError::Failed(format!(
                    "segredo '{sec_name}' está vazio no cofre"
                )));
            }
            let header_val = format!("PVEAPIToken={token}");
            let secrets = vec![token, header_val.clone()];
            Ok((Some(("Authorization".into(), header_val)), secrets))
        }
        "header" => {
            let hdr_name = opt_str(args, "auth_header").ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"header\" exige `auth_header` (ex.: X-API-Key)".into(),
                )
            })?;
            let hdr_name = validate_header_token("auth_header", hdr_name)?;
            if hdr_name.contains(char::is_whitespace) || hdr_name.contains(':') {
                return Err(ToolError::BadArguments(format!(
                    "nome de cabeçalho inválido em `auth_header`: '{hdr_name}'"
                )));
            }
            let sec_name = secret_ref.ok_or_else(|| {
                ToolError::BadArguments(
                    "auth_mode=\"header\" exige `secret_ref` (nome do segredo no cofre)".into(),
                )
            })?;
            let secret = lookup_secret(sec_name)?.trim().to_string();
            if secret.is_empty() {
                return Err(ToolError::Failed(format!(
                    "segredo '{sec_name}' está vazio no cofre"
                )));
            }
            let secrets = vec![secret.clone()];
            Ok((Some((hdr_name, secret)), secrets))
        }
        other => Err(ToolError::BadArguments(format!(
            "auth_mode inválido: '{other}' (use none, basic, bearer, pve_token ou header)"
        ))),
    }
}

fn parse_extra_headers(args: &Value) -> Result<Vec<(String, String)>> {
    let Some(h) = args.get("headers") else {
        return Ok(Vec::new());
    };
    if h.is_null() {
        return Ok(Vec::new());
    }
    let Some(map) = h.as_object() else {
        return Err(ToolError::BadArguments(
            "`headers` precisa ser um objeto JSON (nome -> valor)".into(),
        ));
    };
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        let name = validate_header_token("headers", k)?;
        if name.contains(char::is_whitespace) || name.contains(':') {
            return Err(ToolError::BadArguments(format!(
                "nome de cabeçalho inválido: '{name}'"
            )));
        }
        let val_raw = match v {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let val = validate_header_token(&name, &val_raw)?;
        out.push((name, val));
    }
    Ok(out)
}

enum RequestBody {
    None,
    Json(String),
    Text(String),
}

fn prepare_body(args: &Value) -> Result<RequestBody> {
    match args.get("body") {
        None | Some(Value::Null) => Ok(RequestBody::None),
        Some(Value::String(s)) => {
            if serde_json::from_str::<Value>(s.trim()).is_ok() {
                Ok(RequestBody::Json(s.clone()))
            } else {
                Ok(RequestBody::Text(s.clone()))
            }
        }
        Some(other) => {
            let serialized =
                serde_json::to_string(other).map_err(|e| ToolError::BadArguments(e.to_string()))?;
            Ok(RequestBody::Json(serialized))
        }
    }
}

struct PreparedRequest {
    method: &'static str,
    url: String,
    auth_header: Option<(String, String)>,
    extra_headers: Vec<(String, String)>,
    body: RequestBody,
    insecure_tls: bool,
    timeout: Duration,
    secrets: Vec<String>,
}

pub(crate) fn redact_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    // Redact longest tokens first so full Authorization values get masked before substrings.
    let mut ordered: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in ordered {
        out = out.replace(secret, "[redacted]");
    }
    out
}

fn read_capped_body(resp: ureq::Response) -> (String, bool) {
    let mut buf = Vec::new();
    let mut reader = resp.into_reader().take((MAX_BODY_BYTES + 1) as u64);
    let _ = reader.read_to_end(&mut buf);
    let truncated = buf.len() > MAX_BODY_BYTES;
    if truncated {
        buf.truncate(MAX_BODY_BYTES);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    (text, truncated)
}

fn format_body(raw: &str, truncated: bool) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut rendered = if !truncated {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| trimmed.to_string()),
            Err(_) => raw.to_string(),
        }
    } else {
        raw.to_string()
    };
    if truncated {
        rendered.push_str("\n\n[resposta truncada em 256 KB]");
    }
    rendered
}

fn format_response_headers(resp: &ureq::Response) -> String {
    let mut lines = Vec::new();
    for name in resp.headers_names() {
        let low = name.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "set-cookie" | "authorization" | "proxy-authorization"
        ) {
            continue;
        }
        if let Some(val) = resp.header(&name) {
            lines.push(format!("{name}: {val}"));
        }
    }
    lines.sort();
    lines.join("\n")
}

fn execute_http_request(spec: PreparedRequest) -> Result<String> {
    let agent = build_http_agent(spec.timeout, spec.insecure_tls);
    let mut req = agent.request(spec.method, &spec.url);

    let has_content_type = spec
        .extra_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    let has_accept = spec
        .extra_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("accept"));

    if !has_accept {
        req = req.set("Accept", "application/json, */*;q=0.8");
    }
    for (k, v) in &spec.extra_headers {
        req = req.set(k, v);
    }
    if let Some((ref k, ref v)) = spec.auth_header {
        req = req.set(k, v);
    }

    let resp_result = match &spec.body {
        RequestBody::None => req.call(),
        RequestBody::Json(s) => {
            if !has_content_type {
                req = req.set("Content-Type", "application/json");
            }
            req.send_string(s)
        }
        RequestBody::Text(s) => {
            if !has_content_type {
                req = req.set("Content-Type", "text/plain; charset=utf-8");
            }
            req.send_string(s)
        }
    };

    let safe_url = redact_secrets(&spec.url, &spec.secrets);
    match resp_result {
        Ok(resp) => {
            let status = resp.status();
            let status_text = resp.status_text().to_string();
            let content_type = resp.header("content-type").unwrap_or("").to_string();
            if spec.method == "HEAD" {
                let hdrs = format_response_headers(&resp);
                let out = if hdrs.is_empty() {
                    format!("HTTP {status} {status_text} ({} {safe_url})", spec.method)
                } else {
                    format!(
                        "HTTP {status} {status_text} ({} {safe_url})\n{hdrs}",
                        spec.method
                    )
                };
                return Ok(redact_secrets(&out, &spec.secrets));
            }
            let (raw_body, truncated) = read_capped_body(resp);
            let body = format_body(&raw_body, truncated);
            let mut out = format!("HTTP {status} {status_text} ({} {safe_url})", spec.method);
            if !content_type.is_empty() {
                out.push_str(&format!("\nContent-Type: {content_type}"));
            }
            if !body.is_empty() {
                out.push_str("\n\n");
                out.push_str(&body);
            }
            Ok(redact_secrets(&out, &spec.secrets))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let status_text = resp.status_text().to_string();
            let (raw_body, truncated) = read_capped_body(resp);
            let body = format_body(&raw_body, truncated);
            let mut msg = format!("HTTP {code} {status_text} ({} {safe_url})", spec.method);
            if !body.is_empty() {
                msg.push_str("\n\n");
                msg.push_str(&body);
            }
            Err(ToolError::Failed(redact_secrets(&msg, &spec.secrets)))
        }
        Err(e) => Err(ToolError::Failed(redact_secrets(
            &format!("falha HTTP ({} {safe_url}): {e}", spec.method),
            &spec.secrets,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use zerith_security::{preset, Policy};

    #[test]
    fn rfc4648_base64_vectors_and_credentials() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"admin:S3nh@!"), "YWRtaW46UzNuaEAh");
    }

    #[test]
    fn url_and_device_resolution_for_proxmox_routeros_and_generic() {
        let pve = DeviceEntry {
            name: "pve-01".into(),
            host: "10.0.0.10".into(),
            port: 22,
            vendor: Vendor::Proxmox,
            ..Default::default()
        };
        assert_eq!(
            resolve_request_url(
                &json!({"device": "pve-01", "path": "/nodes/pve01/status"}),
                Some(&pve)
            )
            .unwrap(),
            "https://10.0.0.10:8006/api2/json/nodes/pve01/status"
        );

        let mk = DeviceEntry {
            name: "mk-borda".into(),
            host: "192.168.88.1".into(),
            port: 22,
            vendor: Vendor::MikrotikRouterOs,
            ..Default::default()
        };
        assert_eq!(
            resolve_request_url(
                &json!({"device": "mk-borda", "path": "system/resource", "query": {".proplist": "uptime,cpu-load"}}),
                Some(&mk)
            )
            .unwrap(),
            "https://192.168.88.1/rest/system/resource?.proplist=uptime%2Ccpu-load"
        );

        let generic_v6 = DeviceEntry {
            name: "core-v6".into(),
            host: "2001:db8::1".into(),
            port: 8443,
            vendor: Vendor::CiscoIos,
            ..Default::default()
        };
        assert_eq!(
            resolve_request_url(
                &json!({"device": "core-v6", "path": "/restconf/data/ietf-interfaces:interfaces"}),
                Some(&generic_v6)
            )
            .unwrap(),
            "https://[2001:db8::1]:8443/restconf/data/ietf-interfaces:interfaces"
        );

        assert_eq!(
            resolve_request_url(
                &json!({"url": "https://api.example.com/v1/", "path": "/health", "query": {"full": true}}),
                None
            )
            .unwrap(),
            "https://api.example.com/v1/health?full=true"
        );

        assert!(resolve_request_url(&json!({"url": "ftp://invalid"}), None).is_err());
    }

    #[test]
    fn auth_header_building_and_plaintext_rejection() {
        let dev = DeviceEntry {
            name: "mk-01".into(),
            host: "10.0.0.1".into(),
            user: "noc".into(),
            vendor: Vendor::MikrotikRouterOs,
            secret_ref: Some("net_mk_01".into()),
            ..Default::default()
        };

        // Plaintext credentials are strictly refused
        let err = build_auth_header(
            &json!({"auth_mode": "basic", "username": "noc", "password": "raw"}),
            None,
            |_| Ok("unused".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("texto puro"), "{err}");

        // Basic inherits user and secret_ref from inventory device
        let (hdr, secrets) = build_auth_header(&json!({"auth_mode": "basic"}), Some(&dev), |k| {
            assert_eq!(k, "net_mk_01");
            Ok("S3nh@!".into())
        })
        .unwrap();
        assert_eq!(
            hdr,
            Some((
                "Authorization".into(),
                format!("Basic {}", base64_encode(b"noc:S3nh@!"))
            ))
        );
        assert!(secrets.contains(&"S3nh@!".to_string()));

        // Bearer
        let (hdr, _) = build_auth_header(
            &json!({"auth_mode": "bearer", "secret_ref": "api_tok"}),
            None,
            |_| Ok("tok-xyz-999".into()),
        )
        .unwrap();
        assert_eq!(
            hdr,
            Some(("Authorization".into(), "Bearer tok-xyz-999".into()))
        );

        // PVE API token (adds prefix if absent, avoids doubling if present)
        let (hdr, _) = build_auth_header(
            &json!({"auth_mode": "pve_token", "secret_ref": "pve_tok"}),
            None,
            |_| Ok("root@pam!noc=1111-2222".into()),
        )
        .unwrap();
        assert_eq!(
            hdr,
            Some((
                "Authorization".into(),
                "PVEAPIToken=root@pam!noc=1111-2222".into()
            ))
        );
        let (hdr2, _) = build_auth_header(
            &json!({"auth_mode": "pve_token", "secret_ref": "pve_tok"}),
            None,
            |_| Ok("PVEAPIToken=root@pam!noc=1111-2222".into()),
        )
        .unwrap();
        assert_eq!(hdr2, hdr);

        // Custom header
        let (hdr, _) = build_auth_header(
            &json!({"auth_mode": "header", "auth_header": "X-API-Key", "secret_ref": "k"}),
            None,
            |_| Ok("my-key-123".into()),
        )
        .unwrap();
        assert_eq!(hdr, Some(("X-API-Key".into(), "my-key-123".into())));
    }

    #[test]
    fn secret_redaction_masks_tokens_and_auth_headers() {
        let secrets = vec![
            "S3nh@SuperSecreta".to_string(),
            "Basic YWRtaW46UzNuaEBTdXBlclNlY3JldGE=".to_string(),
        ];
        let raw = "Erro usando Basic YWRtaW46UzNuaEBTdXBlclNlY3JldGE= com senha S3nh@SuperSecreta";
        let scrubbed = redact_secrets(raw, &secrets);
        assert!(!scrubbed.contains("S3nh@SuperSecreta"), "{scrubbed}");
        assert!(!scrubbed.contains("YWRtaW46"), "{scrubbed}");
        assert_eq!(scrubbed, "Erro usando [redacted] com senha [redacted]");
    }

    #[test]
    fn concurrency_safe_and_plan_mode_distinguish_reads_from_writes() {
        let tool = NetApiTool::new();
        assert!(tool.concurrency_safe(&json!({})));
        assert!(tool.concurrency_safe(&json!({"method": "GET"})));
        assert!(tool.concurrency_safe(&json!({"method": "head"})));
        assert!(!tool.concurrency_safe(&json!({"method": "POST"})));
        assert!(!tool.concurrency_safe(&json!({"method": "PUT"})));
        assert!(!tool.concurrency_safe(&json!({"method": "PATCH"})));
        assert!(!tool.concurrency_safe(&json!({"method": "DELETE"})));

        let ws = std::path::Path::new(".");
        let plan_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Plan, ws);
        let plan_ctx = CallCtx {
            workspace: ws,
            policy: &plan_pol,
            memory: None,
            scope: None,
        };
        let get_act = tool
            .action(
                &json!({"url": "https://10.0.0.1/rest/system/resource"}),
                &plan_ctx,
            )
            .unwrap();
        assert_eq!(
            get_act,
            Some(Action::Network("net_api:GET:10.0.0.1".into()))
        );
        assert!(matches!(
            tool.action(
                &json!({"url": "https://10.0.0.1/rest/ip/route", "method": "POST"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
    }

    #[test]
    fn loopback_http_json_pretty_print_auth_error_redaction_and_truncation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_auth: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen_clone = Arc::clone(&seen_auth);

        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut rd = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                if rd.read_line(&mut req_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("authorization:") {
                        seen_clone.lock().unwrap().push(v.trim().to_string());
                    }
                    if let Some(v) = low.strip_prefix("content-length:") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_len > 0 {
                    let mut body = vec![0u8; content_len];
                    let _ = rd.read_exact(&mut body);
                }

                if req_line.contains("/ok") {
                    let body = r#"{"board-name":"CCR2004","version":"7.15"}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else if req_line.contains("/err") {
                    let body = r#"{"error":"invalid token tok-SEGREDO-999"}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else {
                    let big = "A".repeat(MAX_BODY_BYTES + 1024);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{big}",
                        big.len()
                    );
                }
            }
        });

        let ok = execute_http_request(PreparedRequest {
            method: "GET",
            url: format!("http://{addr}/ok"),
            auth_header: Some(("Authorization".into(), "Bearer tok-SEGREDO-999".into())),
            extra_headers: Vec::new(),
            body: RequestBody::None,
            insecure_tls: false,
            timeout: Duration::from_secs(5),
            secrets: vec!["tok-SEGREDO-999".into(), "Bearer tok-SEGREDO-999".into()],
        })
        .unwrap();
        assert!(ok.contains("HTTP 200 OK"), "{ok}");
        assert!(ok.contains("\"board-name\": \"CCR2004\""), "{ok}");
        assert_eq!(seen_auth.lock().unwrap()[0], "bearer tok-segredo-999");

        let err = execute_http_request(PreparedRequest {
            method: "POST",
            url: format!("http://{addr}/err"),
            auth_header: Some(("Authorization".into(), "Bearer tok-SEGREDO-999".into())),
            extra_headers: Vec::new(),
            body: RequestBody::Json(r#"{"a":1}"#.into()),
            insecure_tls: false,
            timeout: Duration::from_secs(5),
            secrets: vec!["tok-SEGREDO-999".into(), "Bearer tok-SEGREDO-999".into()],
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(!err.contains("tok-SEGREDO-999"), "secret leaked: {err}");
        assert!(err.contains("[redacted]"), "{err}");

        let big = execute_http_request(PreparedRequest {
            method: "GET",
            url: format!("http://{addr}/big"),
            auth_header: None,
            extra_headers: Vec::new(),
            body: RequestBody::None,
            insecure_tls: false,
            timeout: Duration::from_secs(5),
            secrets: Vec::new(),
        })
        .unwrap();
        assert!(big.contains("[resposta truncada em 256 KB]"));
    }
}
