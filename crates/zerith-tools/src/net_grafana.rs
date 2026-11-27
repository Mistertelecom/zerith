//! Ferramenta `net_grafana`: integração completa com a API HTTP do Grafana
//! (health, datasources, search, dashboards, folders, alerts, annotations, ds/query)
//! e gerador determinístico de dashboards NOC (Grafana v39 JSON + Formato NOC Portátil).
//!
//! Reutiliza `zerith_workflows::zabbix::{build_http_agent, vault_secret}`.
//! Tokens em texto puro são recusados; o modo offline (`generate_dashboard`
//! com `publish: false`) funciona sem `token_secret`.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use zerith_security::{Action, Mode, Sandbox, Verdict};
use zerith_workflows::zabbix::{build_http_agent, vault_secret};

use crate::fntool::opt_str;
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_BODY_BYTES: usize = 512 * 1024;
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(NetGrafanaTool::new())]
}

struct NetGrafanaTool {
    spec: ToolSpec,
}

impl NetGrafanaTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: "net_grafana".into(),
                description: "Opera a API HTTP do Grafana e gera dashboards NOC determinísticos \
                    (Grafana schemaVersion 39 + JSON portátil de 24 colunas): `health`, \
                    `datasources`, `search`, `get_dashboard`, `save_dashboard`, `delete_dashboard`, \
                    `folders`, `alerts`, `annotations` (marcos de mudança/manutenção), `query` \
                    (`/api/ds/query`) e `generate_dashboard` (presets ISP/SRE offline ou com \
                    `publish: true`). Exige `token_secret` no cofre para chamadas de API; \
                    texto puro é recusado."
                    .into(),
                parameters: {
                    let mut props = serde_json::Map::new();
                    let base = json!({
                        "action": {
                            "type": "string",
                            "enum": [
                                "health",
                                "datasources",
                                "search",
                                "get_dashboard",
                                "save_dashboard",
                                "delete_dashboard",
                                "folders",
                                "alerts",
                                "annotations",
                                "query",
                                "generate_dashboard"
                            ],
                            "description": "operação no Grafana (padrão: health)"
                        },
                        "url": {
                            "type": "string",
                            "description": "URL base do Grafana (ex.: https://grafana.empresa)"
                        },
                        "url_secret": {
                            "type": "string",
                            "description": "opcional: nome do segredo no cofre com a URL do Grafana"
                        },
                        "token_secret": {
                            "type": "string",
                            "description": "nome do segredo no cofre com o Service Account Token / API Key do Grafana (obrigatório em chamadas de API ou publish=true)"
                        },
                        "insecure_tls": {
                            "type": "boolean",
                            "description": "aceita certificado HTTPS autoassinado/interno (padrão false)"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "timeout em segundos (padrão 30, máx 120)"
                        },
                        "uid": {
                            "type": "string",
                            "description": "UID do dashboard ou pasta (get_dashboard, delete_dashboard, folders, generate_dashboard)"
                        },
                        "query": {
                            "type": "string",
                            "description": "search: termo de busca por título/dashboard"
                        },
                        "tag": {
                            "type": "string",
                            "description": "search: filtro por tag"
                        },
                        "folder_ids": {
                            "type": "array",
                            "items": {"type": "integer"},
                            "description": "search: IDs de pastas"
                        },
                        "folder_uid": {
                            "type": "string",
                            "description": "save_dashboard/publish: UID da pasta de destino"
                        },
                        "folder_id": {
                            "type": "integer",
                            "description": "save_dashboard/publish: ID numérico da pasta de destino"
                        },
                        "title": {
                            "type": "string",
                            "description": "título do dashboard (generate_dashboard) ou pasta (folders create)"
                        },
                        "message": {
                            "type": "string",
                            "description": "save_dashboard: mensagem de revisão/commit no Grafana"
                        },
                        "overwrite": {
                            "type": "boolean",
                            "description": "save_dashboard: sobrescreve dashboard existente (padrão true)"
                        },
                        "dashboard": {
                            "type": "object",
                            "description": "save_dashboard: objeto JSON completo do dashboard Grafana"
                        }
                    });
                    let ops = json!({
                        "op": {
                            "type": "string",
                            "enum": ["list", "create"],
                            "description": "folders/annotations: list (padrão) ou create"
                        },
                        "text": {
                            "type": "string",
                            "description": "annotations create: texto do evento/mudança no NOC"
                        },
                        "tags": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "tags para annotations ou generate_dashboard"
                        },
                        "time": {
                            "description": "annotations: início em ms UNIX, segundos UNIX ou relativo (ex.: 'now', '1h')"
                        },
                        "time_end": {
                            "description": "annotations: fim opcional em ms UNIX, segundos UNIX ou relativo"
                        },
                        "dashboard_uid": {
                            "type": "string",
                            "description": "annotations: UID do dashboard associado"
                        },
                        "panel_id": {
                            "type": "integer",
                            "description": "annotations: ID do painel associado"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "limite máximo de itens retornados (search/annotations)"
                        },
                        "queries": {
                            "type": "array",
                            "items": {"type": "object"},
                            "description": "query: array completo de targets para POST /api/ds/query"
                        },
                        "datasource_uid": {
                            "type": "string",
                            "description": "query: UID do datasource (atalho quando queries não informado)"
                        },
                        "datasource_type": {
                            "type": "string",
                            "description": "query: tipo do datasource (ex.: prometheus, alexanderzobnin-zabbix-datasource)"
                        },
                        "expr": {
                            "type": "string",
                            "description": "query: expressão PromQL/LogQL/query (atalho quando queries não informado)"
                        },
                        "from": {
                            "type": "string",
                            "description": "query: janela inicial (padrão 'now-1h')"
                        },
                        "to": {
                            "type": "string",
                            "description": "query: janela final (padrão 'now')"
                        },
                        "max_data_points": {
                            "type": "integer",
                            "description": "query: máximo de pontos (padrão 300)"
                        },
                        "interval_ms": {
                            "type": "integer",
                            "description": "query: intervalo em ms (padrão 15000)"
                        },
                        "preset": {
                            "type": "string",
                            "enum": [
                                "isp_backbone_cisco_asr",
                                "isp_olt_gpon",
                                "isp_datacom_ring",
                                "isp_ubiquiti_rf",
                                "linux_sre_host",
                                "noc_executive_overview",
                                "custom"
                            ],
                            "description": "generate_dashboard: preset NOC/ISP ou custom"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["grafana_json", "noc_portable_json", "both"],
                            "description": "generate_dashboard: formato de saída (padrão grafana_json)"
                        },
                        "datasource": {
                            "description": "generate_dashboard: objeto {\"name\", \"type\", \"uid\"} ou string com nome do datasource"
                        },
                        "variables": {
                            "type": "array",
                            "items": {"type": "object"},
                            "description": "generate_dashboard: variáveis de template adicionais/customizadas"
                        },
                        "panels": {
                            "type": "array",
                            "items": {"type": "object"},
                            "description": "generate_dashboard: painéis customizados (para preset='custom' ou extras)"
                        },
                        "output_path": {
                            "type": "string",
                            "description": "generate_dashboard: caminho relativo no workspace para gravação atômica"
                        },
                        "publish": {
                            "type": "boolean",
                            "description": "generate_dashboard: se true, publica no Grafana via POST /api/dashboards/db (padrão false)"
                        }
                    });
                    if let Value::Object(m) = base {
                        props.extend(m);
                    }
                    if let Value::Object(m) = ops {
                        props.extend(m);
                    }
                    json!({
                        "type": "object",
                        "properties": Value::Object(props)
                    })
                },
                returns: "JSON formatado da API do Grafana ou dashboard NOC gerado (grafana_json / noc_portable_json / both)",
                permission: Permission::Network,
                timeout: Duration::from_secs(60),
                cancellable: true,
                local: false,
                source: "built-in".into(),
            },
        }
    }
}

fn grafana_action(args: &Value) -> &str {
    opt_str(args, "action").unwrap_or("health")
}

fn is_annotations_create(args: &Value) -> bool {
    match opt_str(args, "op") {
        Some(op) => op.eq_ignore_ascii_case("create"),
        None => opt_str(args, "text").is_some(),
    }
}

fn is_folders_create(args: &Value) -> bool {
    opt_str(args, "op").is_some_and(|op| op.eq_ignore_ascii_case("create"))
}

fn is_readonly_call(args: &Value) -> bool {
    match grafana_action(args) {
        "health" | "datasources" | "search" | "get_dashboard" | "alerts" | "query" => true,
        "folders" => !is_folders_create(args),
        "annotations" => !is_annotations_create(args),
        "generate_dashboard" => {
            !args
                .get("publish")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && opt_str(args, "output_path").is_none()
        }
        "save_dashboard" | "delete_dashboard" => false,
        _ => false,
    }
}

impl Tool for NetGrafanaTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        is_readonly_call(args)
    }

    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        let act = grafana_action(args);
        if act == "generate_dashboard" {
            let publish = args
                .get("publish")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let out_path = opt_str(args, "output_path");
            if (publish || out_path.is_some())
                && (ctx.policy.preset.sandbox == Sandbox::ReadOnly
                    || ctx.policy.mode() == Mode::Plan)
            {
                return Err(ToolError::Denied(
                    "publicação ou gravação em disco de dashboard bloqueada no modo plan/read-only"
                        .into(),
                ));
            }
            let target = opt_str(args, "url")
                .or_else(|| opt_str(args, "url_secret"))
                .unwrap_or("grafana");
            if let Some(p) = out_path {
                let resolved = ctx.resolve(p);
                if publish {
                    if let Verdict::Deny(reason) = ctx
                        .policy
                        .check_tool("write_file", &Action::Write(resolved))
                    {
                        return Err(ToolError::Denied(reason));
                    }
                    return Ok(Some(Action::Network(format!("grafana publish {target}"))));
                }
                return Ok(Some(Action::Write(resolved)));
            }
            if publish {
                return Ok(Some(Action::Network(format!("grafana publish {target}"))));
            }
            return Ok(None);
        }

        if !is_readonly_call(args)
            && (ctx.policy.preset.sandbox == Sandbox::ReadOnly || ctx.policy.mode() == Mode::Plan)
        {
            return Err(ToolError::Denied(format!(
                "operação de escrita no Grafana (`{act}`) bloqueada no modo plan/read-only"
            )));
        }

        let target = opt_str(args, "url")
            .or_else(|| opt_str(args, "url_secret"))
            .unwrap_or("grafana");
        Ok(Some(Action::Network(format!("grafana {act} {target}"))))
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        reject_plaintext_credentials(args)?;

        let act = grafana_action(args);
        if act == "generate_dashboard" {
            let publish = args
                .get("publish")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !publish {
                return run_offline_generate(args, ctx, exec);
            }
            // Publish mode requires Grafana credentials and network access.
            let (url, token) = resolve_url_and_token(args, |name| {
                let v = vault_secret(name).map_err(|e| ToolError::Failed(e.to_string()))?;
                exec.host.note_secret(&v);
                Ok(v)
            })?;
            let timeout = timeout_from_args(args);
            let insecure = insecure_from_args(args);
            let bundle = generate_dashboard_bundle(args)?;
            if let Some(out_path) = opt_str(args, "output_path") {
                save_generated_bundle(ctx, exec, out_path, &bundle.primary_text)?;
            }
            let payload = json!({
                "dashboard": bundle.grafana_json,
                "folderUid": opt_str(args, "folder_uid"),
                "folderId": args.get("folder_id").and_then(Value::as_i64),
                "message": opt_str(args, "message").unwrap_or("Publicado por Zerith NOC"),
                "overwrite": args.get("overwrite").and_then(Value::as_bool).unwrap_or(true)
            });
            let publish_resp = run_in_worker(exec, move || {
                let client = GrafanaClient::new(&url, token, timeout, insecure)?;
                let res = client.post_json("/api/dashboards/db", &payload)?;
                Ok(res)
            })?;
            return pretty_json(&json!({
                "published": true,
                "grafana_response": publish_resp,
                "output": bundle.output_value
            }));
        }

        let (url, token) = resolve_url_and_token(args, |name| {
            let v = vault_secret(name).map_err(|e| ToolError::Failed(e.to_string()))?;
            exec.host.note_secret(&v);
            Ok(v)
        })?;
        let timeout = timeout_from_args(args);
        let insecure = insecure_from_args(args);
        let args_owned = args.clone();

        run_in_worker(exec, move || {
            let client = GrafanaClient::new(&url, token, timeout, insecure)?;
            execute_with_client(&client, &args_owned)
        })
    }
}

fn timeout_from_args(args: &Value) -> Duration {
    Duration::from_secs(
        args.get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 120),
    )
}

fn insecure_from_args(args: &Value) -> bool {
    args.get("insecure_tls")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn run_in_worker<T: Send + 'static>(
    exec: &Exec<'_>,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    loop {
        exec.check()?;
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(res) => return res,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ToolError::Failed(
                    "chamada ao Grafana interrompida inesperadamente".into(),
                ))
            }
        }
    }
}

// --- Credential handling & redaction ----------------------------------------

fn reject_plaintext_credentials(args: &Value) -> Result<()> {
    for forbidden in ["token", "password", "api_key", "apikey", "secret"] {
        if args.get(forbidden).is_some_and(|v| !v.is_null()) {
            return Err(ToolError::BadArguments(format!(
                "credencial em texto puro (`{forbidden}`) não é aceita em `net_grafana`: \
                 grave no cofre (`zerith secret set <nome>`) e informe `token_secret`"
            )));
        }
    }
    Ok(())
}

fn resolve_url_and_token(
    args: &Value,
    mut lookup_secret: impl FnMut(&str) -> Result<String>,
) -> Result<(String, String)> {
    reject_plaintext_credentials(args)?;
    let token_secret = opt_str(args, "token_secret").ok_or_else(|| {
        ToolError::BadArguments(
            "token_secret é obrigatório para acessar a API do Grafana (grave no cofre com `zerith secret set <nome>`)"
                .into(),
        )
    })?;
    let raw_token = lookup_secret(token_secret)?.trim().to_string();
    let token = raw_token
        .strip_prefix("Bearer ")
        .or_else(|| raw_token.strip_prefix("bearer "))
        .unwrap_or(&raw_token)
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(ToolError::Failed(format!(
            "segredo '{token_secret}' está vazio no cofre"
        )));
    }

    let url = if let Some(u) = opt_str(args, "url") {
        u.trim().to_string()
    } else if let Some(u_sec) = opt_str(args, "url_secret") {
        lookup_secret(u_sec)?.trim().to_string()
    } else {
        return Err(ToolError::BadArguments(
            "informe `url` (ex.: https://grafana.empresa) ou `url_secret`".into(),
        ));
    };

    validate_http_url(&url)?;
    Ok((url, token))
}

fn validate_http_url(url: &str) -> Result<()> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(ToolError::BadArguments(
            "URL do Grafana deve começar com http:// ou https:// (ex.: https://grafana.empresa)"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn redact_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
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

// --- Grafana HTTP Client ----------------------------------------------------

pub(crate) struct GrafanaClient {
    base_url: String,
    token: String,
    agent: ureq::Agent,
}

impl GrafanaClient {
    pub(crate) fn new(
        url: &str,
        token: String,
        timeout: Duration,
        insecure_tls: bool,
    ) -> Result<Self> {
        validate_http_url(url)?;
        if token.trim().is_empty() {
            return Err(ToolError::BadArguments("token do Grafana vazio".into()));
        }
        Ok(Self {
            base_url: url.trim().trim_end_matches('/').to_string(),
            token,
            agent: build_http_agent(timeout, insecure_tls),
        })
    }

    fn secrets(&self) -> [String; 2] {
        [format!("Bearer {}", self.token), self.token.clone()]
    }

    fn redact(&self, msg: &str) -> String {
        redact_secrets(msg, &self.secrets())
    }

    fn endpoint(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{path}", self.base_url)
        } else {
            format!("{}/{path}", self.base_url)
        }
    }

    fn get_json(&self, path: &str) -> Result<Value> {
        self.request_json("GET", path, None)
    }

    fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.request_json("POST", path, Some(body))
    }

    fn delete_json(&self, path: &str) -> Result<Value> {
        self.request_json("DELETE", path, None)
    }

    fn request_json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let full_url = self.endpoint(path);
        let mut req = self
            .agent
            .request(method, &full_url)
            .set("Accept", "application/json")
            .set("Authorization", &format!("Bearer {}", self.token));

        let resp_result = match body {
            Some(payload) => {
                req = req.set("Content-Type", "application/json");
                let text = serde_json::to_string(payload)
                    .map_err(|e| ToolError::Failed(self.redact(&e.to_string())))?;
                req.send_string(&text)
            }
            None => req.call(),
        };

        match resp_result {
            Ok(resp) => {
                let raw = read_capped_text(resp);
                if raw.trim().is_empty() {
                    return Ok(json!({"status": "ok"}));
                }
                serde_json::from_str(&raw).map_err(|e| {
                    ToolError::Failed(self.redact(&format!(
                        "Grafana {method} {path}: resposta JSON inválida ({e}): {}",
                        clip_text(raw.trim(), 250)
                    )))
                })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let raw = read_capped_text(resp);
                let hint = if code == 401 || code == 403 {
                    " (verifique permissões/validade do token em `token_secret`)"
                } else {
                    ""
                };
                Err(ToolError::Failed(self.redact(&format!(
                    "Grafana {method} {path}: HTTP {code}{hint} {}",
                    clip_text(raw.trim(), 300)
                ))))
            }
            Err(e) => Err(ToolError::Failed(
                self.redact(&format!("Grafana {method} {path}: falha de rede ({e})")),
            )),
        }
    }
}

fn read_capped_text(resp: ureq::Response) -> String {
    let mut buf = Vec::new();
    let mut rd = resp.into_reader().take((MAX_BODY_BYTES + 1) as u64);
    let _ = rd.read_to_end(&mut buf);
    if buf.len() > MAX_BODY_BYTES {
        buf.truncate(MAX_BODY_BYTES);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn clip_text(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// --- API Action Dispatcher --------------------------------------------------

pub(crate) fn execute_with_client(client: &GrafanaClient, args: &Value) -> Result<String> {
    let action = grafana_action(args);
    match action {
        "health" => {
            let health = client.get_json("/api/health")?;
            let org = client.get_json("/api/org").ok();
            pretty_json(&json!({
                "health": health,
                "org": org
            }))
        }
        "datasources" => {
            let list = client.get_json("/api/datasources")?;
            pretty_json(&list)
        }
        "search" => {
            let mut params = vec![("type".to_string(), "dash-db".to_string())];
            if let Some(q) = opt_str(args, "query") {
                params.push(("query".into(), q.into()));
            }
            if let Some(tag) = opt_str(args, "tag") {
                params.push(("tag".into(), tag.into()));
            }
            if let Some(arr) = args.get("folder_ids").and_then(Value::as_array) {
                for id in arr {
                    if let Some(n) = id.as_i64() {
                        params.push(("folderIds".into(), n.to_string()));
                    }
                }
            }
            if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
                params.push(("limit".into(), limit.clamp(1, 500).to_string()));
            }
            let qs: Vec<String> = params
                .into_iter()
                .map(|(k, v)| format!("{}={}", url_encode(&k), url_encode(&v)))
                .collect();
            let res = client.get_json(&format!("/api/search?{}", qs.join("&")))?;
            pretty_json(&res)
        }
        "get_dashboard" => {
            let uid = opt_str(args, "uid").ok_or_else(|| {
                ToolError::BadArguments("get_dashboard exige `uid` do dashboard".into())
            })?;
            let res = client.get_json(&format!("/api/dashboards/uid/{}", url_encode(uid)))?;
            pretty_json(&res)
        }
        "save_dashboard" => {
            let dash = match args.get("dashboard") {
                Some(Value::Object(_)) => args["dashboard"].clone(),
                Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| {
                    ToolError::BadArguments(format!("`dashboard` JSON inválido: {e}"))
                })?,
                _ => {
                    return Err(ToolError::BadArguments(
                        "save_dashboard exige objeto `dashboard`".into(),
                    ))
                }
            };
            let mut body = Map::new();
            body.insert("dashboard".into(), dash);
            if let Some(fuid) = opt_str(args, "folder_uid") {
                body.insert("folderUid".into(), json!(fuid));
            }
            if let Some(fid) = args.get("folder_id").and_then(Value::as_i64) {
                body.insert("folderId".into(), json!(fid));
            }
            body.insert(
                "message".into(),
                json!(opt_str(args, "message").unwrap_or("Atualizado por Zerith NOC")),
            );
            body.insert(
                "overwrite".into(),
                json!(args.get("overwrite").and_then(Value::as_bool).unwrap_or(true)),
            );
            let res = client.post_json("/api/dashboards/db", &Value::Object(body))?;
            pretty_json(&res)
        }
        "delete_dashboard" => {
            let uid = opt_str(args, "uid").ok_or_else(|| {
                ToolError::BadArguments("delete_dashboard exige `uid` do dashboard".into())
            })?;
            let res = client.delete_json(&format!("/api/dashboards/uid/{}", url_encode(uid)))?;
            pretty_json(&res)
        }
        "folders" => {
            if is_folders_create(args) {
                let title = opt_str(args, "title").ok_or_else(|| {
                    ToolError::BadArguments("folders op=\"create\" exige `title`".into())
                })?;
                let mut body = json!({"title": title});
                if let Some(uid) = opt_str(args, "uid") {
                    body["uid"] = json!(uid);
                }
                let res = client.post_json("/api/folders", &body)?;
                pretty_json(&res)
            } else {
                let res = client.get_json("/api/folders")?;
                pretty_json(&res)
            }
        }
        "alerts" => {
            let rules = client.get_json("/api/v1/provisioning/alert-rules")?;
            let active = client
                .get_json("/api/alertmanager/grafana/api/v2/alerts")
                .ok();
            pretty_json(&json!({
                "alert_rules": rules,
                "active_alerts": active
            }))
        }
        "annotations" => execute_annotations(client, args),
        "query" => execute_ds_query(client, args),
        "generate_dashboard" => {
            let bundle = generate_dashboard_bundle(args)?;
            let payload = json!({
                "dashboard": bundle.grafana_json,
                "folderUid": opt_str(args, "folder_uid"),
                "folderId": args.get("folder_id").and_then(Value::as_i64),
                "message": opt_str(args, "message").unwrap_or("Publicado por Zerith NOC"),
                "overwrite": args.get("overwrite").and_then(Value::as_bool).unwrap_or(true)
            });
            let publish_resp = client.post_json("/api/dashboards/db", &payload)?;
            pretty_json(&json!({
                "published": true,
                "grafana_response": publish_resp,
                "output": bundle.output_value
            }))
        }
        other => Err(ToolError::BadArguments(format!(
            "ação Grafana desconhecida: '{other}' (use health, datasources, search, get_dashboard, \
             save_dashboard, delete_dashboard, folders, alerts, annotations, query ou generate_dashboard)"
        ))),
    }
}

fn execute_annotations(client: &GrafanaClient, args: &Value) -> Result<String> {
    if is_annotations_create(args) {
        let text = opt_str(args, "text").ok_or_else(|| {
            ToolError::BadArguments(
                "annotations op=\"create\" exige `text` descrevendo o evento/mudança".into(),
            )
        })?;
        let now_ms = now_unix_ms();
        let time_ms = parse_ms_timestamp(args.get("time"), now_ms)?.unwrap_or(now_ms);
        let mut body = Map::new();
        body.insert("text".into(), json!(text));
        body.insert("time".into(), json!(time_ms));
        if let Some(end_ms) = parse_ms_timestamp(args.get("time_end"), now_ms)? {
            body.insert("timeEnd".into(), json!(end_ms));
        }
        let tags = string_list_any(args, "tags");
        if !tags.is_empty() {
            body.insert("tags".into(), json!(tags));
        }
        if let Some(duid) = opt_str(args, "dashboard_uid") {
            body.insert("dashboardUID".into(), json!(duid));
        }
        if let Some(pid) = args.get("panel_id").and_then(Value::as_i64) {
            body.insert("panelId".into(), json!(pid));
        }
        let res = client.post_json("/api/annotations", &Value::Object(body))?;
        pretty_json(&res)
    } else {
        let now_ms = now_unix_ms();
        let mut params = Vec::new();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, 500);
        params.push(("limit".to_string(), limit.to_string()));
        if let Some(duid) = opt_str(args, "dashboard_uid") {
            params.push(("dashboardUID".into(), duid.into()));
        }
        if let Some(from) = parse_ms_timestamp(args.get("time"), now_ms)? {
            params.push(("from".into(), from.to_string()));
        }
        if let Some(to) = parse_ms_timestamp(args.get("time_end"), now_ms)? {
            params.push(("to".into(), to.to_string()));
        }
        for tag in string_list_any(args, "tags") {
            params.push(("tags".into(), tag));
        }
        let qs: Vec<String> = params
            .into_iter()
            .map(|(k, v)| format!("{}={}", url_encode(&k), url_encode(&v)))
            .collect();
        let res = client.get_json(&format!("/api/annotations?{}", qs.join("&")))?;
        pretty_json(&res)
    }
}

fn execute_ds_query(client: &GrafanaClient, args: &Value) -> Result<String> {
    let from = opt_str(args, "from").unwrap_or("now-1h");
    let to = opt_str(args, "to").unwrap_or("now");
    let max_points = args
        .get("max_data_points")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .clamp(10, 5000);
    let interval_ms = args
        .get("interval_ms")
        .and_then(Value::as_u64)
        .unwrap_or(15_000)
        .clamp(1000, 3_600_000);

    let queries = if let Some(Value::Array(arr)) = args.get("queries") {
        if arr.is_empty() {
            return Err(ToolError::BadArguments(
                "`queries` não pode estar vazio".into(),
            ));
        }
        arr.clone()
    } else {
        let ds_uid = opt_str(args, "datasource_uid").ok_or_else(|| {
            ToolError::BadArguments("query exige `queries` ou `datasource_uid` + `expr`".into())
        })?;
        let expr = opt_str(args, "expr").ok_or_else(|| {
            ToolError::BadArguments("query exige `expr` quando `queries` é omitido".into())
        })?;
        let ds_type = opt_str(args, "datasource_type").unwrap_or("prometheus");
        vec![json!({
            "refId": "A",
            "datasource": {"type": ds_type, "uid": ds_uid},
            "expr": expr,
            "maxDataPoints": max_points,
            "intervalMs": interval_ms
        })]
    };

    let body = json!({
        "from": from,
        "to": to,
        "queries": queries
    });
    let res = client.post_json("/api/ds/query", &body)?;
    pretty_json(&res)
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_ms_timestamp(val: Option<&Value>, now_ms: i64) -> Result<Option<i64>> {
    let Some(v) = val else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    if let Some(n) = v.as_i64() {
        // Values under 1e10 are treated as Unix seconds and normalized to ms.
        let ms = if n.abs() < 10_000_000_000 {
            n * 1000
        } else {
            n
        };
        return Ok(Some(ms));
    }
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.eq_ignore_ascii_case("now") {
            return Ok(Some(now_ms));
        }
        if let Ok(n) = trimmed.parse::<i64>() {
            let ms = if n.abs() < 10_000_000_000 {
                n * 1000
            } else {
                n
            };
            return Ok(Some(ms));
        }
        let rel = trimmed.to_ascii_lowercase();
        let rel = rel.strip_prefix("now-").unwrap_or(&rel);
        if rel.len() >= 2 {
            let (num, unit) = rel.split_at(rel.len() - 1);
            if let Ok(qty) = num.trim().parse::<i64>() {
                let mult = match unit {
                    "s" => 1_000,
                    "m" => 60_000,
                    "h" => 3_600_000,
                    "d" => 86_400_000,
                    _ => {
                        return Err(ToolError::BadArguments(format!(
                            "unidade de tempo inválida em '{trimmed}'"
                        )))
                    }
                };
                return Ok(Some(now_ms - qty * mult));
            }
        }
        return Err(ToolError::BadArguments(format!(
            "timestamp inválido: '{trimmed}'"
        )));
    }
    Err(ToolError::BadArguments(
        "timestamp inválido: informe inteiro UNIX ou relativo ('now', '1h', '24h')".into(),
    ))
}

fn string_list_any(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::trim))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

// --- Deterministic NOC Dashboard Generator ----------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DashboardPreset {
    IspBackboneCiscoAsr,
    IspOltGpon,
    IspDatacomRing,
    IspUbiquitiRf,
    LinuxSreHost,
    NocExecutiveOverview,
    Custom,
}

impl DashboardPreset {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "isp_backbone_cisco_asr" => Ok(Self::IspBackboneCiscoAsr),
            "isp_olt_gpon" => Ok(Self::IspOltGpon),
            "isp_datacom_ring" => Ok(Self::IspDatacomRing),
            "isp_ubiquiti_rf" => Ok(Self::IspUbiquitiRf),
            "linux_sre_host" => Ok(Self::LinuxSreHost),
            "noc_executive_overview" => Ok(Self::NocExecutiveOverview),
            "custom" => Ok(Self::Custom),
            other => Err(ToolError::BadArguments(format!(
                "preset inválido em generate_dashboard: '{other}' (use isp_backbone_cisco_asr, \
                 isp_olt_gpon, isp_datacom_ring, isp_ubiquiti_rf, linux_sre_host, noc_executive_overview ou custom)"
            ))),
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::IspBackboneCiscoAsr => "isp-backbone-cisco-asr",
            Self::IspOltGpon => "isp-olt-gpon",
            Self::IspDatacomRing => "isp-datacom-ring",
            Self::IspUbiquitiRf => "isp-ubiquiti-rf",
            Self::LinuxSreHost => "linux-sre-host",
            Self::NocExecutiveOverview => "noc-executive-overview",
            Self::Custom => "noc-custom",
        }
    }

    fn default_title(self) -> &'static str {
        match self {
            Self::IspBackboneCiscoAsr => "NOC Backbone Cisco ASR 1001-X (IOS-XE / BGP / DOM)",
            Self::IspOltGpon => "NOC OLT GPON / XGS-PON (PON Ports & ONUs)",
            Self::IspDatacomRing => "NOC Anel Metro Datacom DmOS (EAPS/ERPS & DOM)",
            Self::IspUbiquitiRf => "NOC Enlaces RF Ubiquiti (airFiber / airMAX)",
            Self::LinuxSreHost => "NOC SRE Linux Core Host (Compute / Net / Storage)",
            Self::NocExecutiveOverview => "NOC Executive SLA & Capacidade Global",
            Self::Custom => "NOC Custom Dashboard",
        }
    }

    fn default_datasource_type(self) -> &'static str {
        match self {
            Self::LinuxSreHost => "prometheus",
            _ => "alexanderzobnin-zabbix-datasource",
        }
    }

    fn default_datasource_name(self) -> &'static str {
        match self {
            Self::LinuxSreHost => "Prometheus",
            _ => "Zabbix",
        }
    }

    fn focus_var_name(self) -> &'static str {
        match self {
            Self::IspOltGpon => "pon_port",
            Self::IspUbiquitiRf => "rf_link",
            Self::LinuxSreHost => "device",
            _ => "interface",
        }
    }

    fn focus_var_label(self) -> &'static str {
        match self {
            Self::IspOltGpon => "Porta PON",
            Self::IspUbiquitiRf => "Enlace RF",
            Self::LinuxSreHost => "Disco / NIC",
            _ => "Interface",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    GrafanaJson,
    NocPortableJson,
    Both,
}

impl OutputFormat {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "grafana_json" => Ok(Self::GrafanaJson),
            "noc_portable_json" => Ok(Self::NocPortableJson),
            "both" => Ok(Self::Both),
            other => Err(ToolError::BadArguments(format!(
                "format inválido em generate_dashboard: '{other}' (use grafana_json, noc_portable_json ou both)"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
struct DatasourceSpec {
    name: String,
    kind: String,
    uid: String,
}

#[derive(Debug, Clone)]
struct VarSpec {
    name: String,
    label: String,
    kind: String,
    query: String,
    multi: bool,
    include_all: bool,
}

#[derive(Debug, Clone, Copy)]
struct ThresholdStep {
    value: Option<f64>,
    color: &'static str,
    label: &'static str,
}

#[derive(Debug, Clone)]
struct TargetSpec {
    ref_id: String,
    expr: String,
    item_key: String,
    legend: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    Stat,
    Gauge,
    Timeseries,
    Table,
    StateTimeline,
    Logs,
}

impl PanelKind {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "stat" => Self::Stat,
            "gauge" => Self::Gauge,
            "table" => Self::Table,
            "state_timeline" | "state-timeline" | "status-history" => Self::StateTimeline,
            "logs" => Self::Logs,
            _ => Self::Timeseries,
        }
    }

    fn grafana_type(self) -> &'static str {
        match self {
            Self::Stat => "stat",
            Self::Gauge => "gauge",
            Self::Timeseries => "timeseries",
            Self::Table => "table",
            Self::StateTimeline => "state-timeline",
            Self::Logs => "logs",
        }
    }

    fn portable_type(self) -> &'static str {
        match self {
            Self::Stat => "stat",
            Self::Gauge => "gauge",
            Self::Timeseries => "timeseries",
            Self::Table => "table",
            Self::StateTimeline => "state_timeline",
            Self::Logs => "logs",
        }
    }
}

#[derive(Debug, Clone)]
struct PanelBlueprint {
    title: String,
    description: String,
    kind: PanelKind,
    w: u32,
    h: u32,
    unit: String,
    targets: Vec<TargetSpec>,
    thresholds: Vec<ThresholdStep>,
    mirrored_rx_tx: bool,
}

#[derive(Debug, Clone)]
struct TierBlueprint {
    title: String,
    collapsed: bool,
    panels: Vec<PanelBlueprint>,
}

#[derive(Debug, Clone, Copy)]
struct GridPos {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Debug, Clone)]
struct PlacedPanel {
    id: u32,
    grid: GridPos,
    spec: PanelBlueprint,
}

#[derive(Debug, Clone)]
struct PlacedTier {
    row_id: u32,
    title: String,
    collapsed: bool,
    row_grid: GridPos,
    panels: Vec<PlacedPanel>,
}

struct NocDashboardModel {
    uid: String,
    title: String,
    description: String,
    tags: Vec<String>,
    refresh_secs: u64,
    time_from: String,
    time_to: String,
    datasource: DatasourceSpec,
    variables: Vec<VarSpec>,
    tiers: Vec<PlacedTier>,
}

struct DashboardBundle {
    grafana_json: Value,
    output_value: Value,
    primary_text: String,
}

fn run_offline_generate(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let bundle = generate_dashboard_bundle(args)?;
    if let Some(out_path) = opt_str(args, "output_path") {
        let path = save_generated_bundle(ctx, exec, out_path, &bundle.primary_text)?;
        return pretty_json(&json!({
            "saved": true,
            "path": path.display().to_string(),
            "bytes": bundle.primary_text.len(),
            "format": opt_str(args, "format").unwrap_or("grafana_json").to_ascii_lowercase(),
            "preset": opt_str(args, "preset").unwrap_or("noc_executive_overview"),
            "uid": bundle.grafana_json["uid"]
        }));
    }
    Ok(bundle.primary_text)
}

fn save_generated_bundle(
    ctx: &CallCtx<'_>,
    exec: &Exec<'_>,
    out_path: &str,
    content: &str,
) -> Result<std::path::PathBuf> {
    let path = ctx.resolve(out_path);
    exec.host.check_editable(&path)?;
    exec.host.before_write(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&path, content.as_bytes())?;
    exec.host.note_read(&path);
    Ok(path)
}

fn generate_dashboard_bundle(args: &Value) -> Result<DashboardBundle> {
    let preset_raw = opt_str(args, "preset").unwrap_or("noc_executive_overview");
    let preset = DashboardPreset::parse(preset_raw)?;
    let fmt = OutputFormat::parse(opt_str(args, "format").unwrap_or("grafana_json"))?;

    let title = opt_str(args, "title")
        .unwrap_or_else(|| preset.default_title())
        .to_string();
    let uid = opt_str(args, "uid")
        .map(str::to_string)
        .unwrap_or_else(|| deterministic_uid_slug(preset.slug(), &title));

    let datasource = parse_datasource_spec(args, preset);
    let variables = build_variables(args, preset, &datasource);

    let mut tiers_bp = build_preset_tiers(preset);
    if let Some(custom_panels) = parse_custom_panels(args, &datasource) {
        if preset == DashboardPreset::Custom {
            tiers_bp = vec![TierBlueprint {
                title: "1. Painéis Customizados NOC".into(),
                collapsed: false,
                panels: custom_panels,
            }];
        } else {
            tiers_bp.push(TierBlueprint {
                title: "5. Painéis Adicionais Customizados".into(),
                collapsed: false,
                panels: custom_panels,
            });
        }
    }

    let placed_tiers = layout_tiers(tiers_bp);

    let mut tags = vec!["zerith-noc".to_string(), preset.slug().to_string()];
    for extra in string_list_any(args, "tags") {
        if !tags.contains(&extra) {
            tags.push(extra);
        }
    }

    let model = NocDashboardModel {
        uid,
        title: title.clone(),
        description: format!(
            "Dashboard NOC determinístico gerado pelo Zerith ({}) — hierarquia 4 tiers em grid 24 colunas.",
            preset.slug()
        ),
        tags,
        refresh_secs: 30,
        time_from: opt_str(args, "from").unwrap_or("now-6h").to_string(),
        time_to: opt_str(args, "to").unwrap_or("now").to_string(),
        datasource,
        variables,
        tiers: placed_tiers,
    };

    let grafana_json = render_grafana_json(&model);
    let portable_json = render_noc_portable_json(&model);

    let output_value = match fmt {
        OutputFormat::GrafanaJson => grafana_json.clone(),
        OutputFormat::NocPortableJson => portable_json,
        OutputFormat::Both => json!({
            "grafana_json": grafana_json,
            "noc_portable_json": portable_json
        }),
    };
    let primary_text = pretty_json(&output_value)?;

    Ok(DashboardBundle {
        grafana_json,
        output_value,
        primary_text,
    })
}

fn deterministic_uid_slug(prefix: &str, title: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in prefix.as_bytes().iter().chain(title.as_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let short: String = prefix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(20)
        .collect();
    format!("{short}-{:08x}", (h & 0xffff_ffff) as u32)
}

fn parse_datasource_spec(args: &Value, preset: DashboardPreset) -> DatasourceSpec {
    match args.get("datasource") {
        Some(Value::Object(map)) => {
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| preset.default_datasource_name())
                .to_string();
            let kind = map
                .get("type")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| preset.default_datasource_type())
                .to_string();
            let uid = map
                .get("uid")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("${datasource}")
                .to_string();
            DatasourceSpec { name, kind, uid }
        }
        Some(Value::String(s)) if !s.trim().is_empty() => DatasourceSpec {
            name: s.trim().to_string(),
            kind: preset.default_datasource_type().to_string(),
            uid: "${datasource}".to_string(),
        },
        _ => DatasourceSpec {
            name: preset.default_datasource_name().to_string(),
            kind: preset.default_datasource_type().to_string(),
            uid: "${datasource}".to_string(),
        },
    }
}

fn build_variables(args: &Value, preset: DashboardPreset, ds: &DatasourceSpec) -> Vec<VarSpec> {
    let mut vars = vec![
        VarSpec {
            name: "datasource".into(),
            label: "Datasource".into(),
            kind: "datasource".into(),
            query: ds.kind.clone(),
            multi: false,
            include_all: false,
        },
        VarSpec {
            name: "site".into(),
            label: "POP / Site".into(),
            kind: "query".into(),
            query: if ds.kind.contains("prometheus") {
                "label_values(up, site)".into()
            } else {
                "*".into()
            },
            multi: true,
            include_all: true,
        },
        VarSpec {
            name: "host".into(),
            label: "Host / Equipamento".into(),
            kind: "query".into(),
            query: if ds.kind.contains("prometheus") {
                "label_values(up{site=~\"$site\"}, instance)".into()
            } else {
                "{group=\"$site\", host=/.*/}".into()
            },
            multi: true,
            include_all: true,
        },
        VarSpec {
            name: preset.focus_var_name().into(),
            label: preset.focus_var_label().into(),
            kind: "query".into(),
            query: if ds.kind.contains("prometheus") {
                format!(
                    "label_values(node_network_receive_bytes_total{{instance=~\"$host\"}}, {})",
                    preset.focus_var_name()
                )
            } else {
                format!("{{host=\"$host\", item=/{}/}}", preset.focus_var_name())
            },
            multi: true,
            include_all: true,
        },
    ];

    if let Some(Value::Array(custom)) = args.get("variables") {
        for item in custom {
            if let Some(name) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                if !vars.iter().any(|v| v.name == name) {
                    vars.push(VarSpec {
                        name: name.into(),
                        label: name.into(),
                        kind: "query".into(),
                        query: "*".into(),
                        multi: true,
                        include_all: true,
                    });
                }
            } else if let Some(obj) = item.as_object() {
                let Some(name) = obj.get("name").and_then(Value::as_str).map(str::trim) else {
                    continue;
                };
                if name.is_empty() || vars.iter().any(|v| v.name == name) {
                    continue;
                }
                vars.push(VarSpec {
                    name: name.into(),
                    label: obj
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or(name)
                        .to_string(),
                    kind: obj
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("query")
                        .to_string(),
                    query: obj
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or("*")
                        .to_string(),
                    multi: obj.get("multi").and_then(Value::as_bool).unwrap_or(true),
                    include_all: obj
                        .get("include_all")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                });
            }
        }
    }
    vars
}

fn parse_custom_panels(args: &Value, ds: &DatasourceSpec) -> Option<Vec<PanelBlueprint>> {
    let arr = args.get("panels").and_then(Value::as_array)?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (idx, p) in arr.iter().enumerate() {
        let title = p
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Painel Custom")
            .to_string();
        let kind = p
            .get("type")
            .and_then(Value::as_str)
            .map(PanelKind::parse)
            .unwrap_or(PanelKind::Timeseries);
        let w = p
            .get("w")
            .and_then(Value::as_u64)
            .unwrap_or(12)
            .clamp(2, 24) as u32;
        let h = p.get("h").and_then(Value::as_u64).unwrap_or(8).clamp(2, 20) as u32;
        let unit = p
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("short")
            .to_string();
        let expr = p
            .get("expr")
            .or_else(|| p.get("query"))
            .and_then(Value::as_str)
            .unwrap_or(if ds.kind.contains("prometheus") {
                "up{instance=~\"$host\"}"
            } else {
                "icmpping"
            })
            .to_string();
        let item_key = p
            .get("item_key")
            .and_then(Value::as_str)
            .unwrap_or(&expr)
            .to_string();
        let legend = p
            .get("legend")
            .and_then(Value::as_str)
            .unwrap_or("{{host}}")
            .to_string();
        out.push(PanelBlueprint {
            title,
            description: p
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Painel customizado NOC")
                .to_string(),
            kind,
            w,
            h,
            unit,
            targets: vec![TargetSpec {
                ref_id: format!("Q{}", idx + 1),
                expr,
                item_key,
                legend,
            }],
            thresholds: thr_pct_high_bad(),
            mirrored_rx_tx: p
                .get("mirrored_rx_tx")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Some(out)
}

// --- Threshold Presets ------------------------------------------------------

fn thr_sla_high_good() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "red",
            label: "Crítico",
        },
        ThresholdStep {
            value: Some(95.0),
            color: "orange",
            label: "Degradado",
        },
        ThresholdStep {
            value: Some(99.0),
            color: "yellow",
            label: "Atenção",
        },
        ThresholdStep {
            value: Some(99.9),
            color: "green",
            label: "SLA OK",
        },
    ]
}

fn thr_pct_high_bad() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "green",
            label: "OK",
        },
        ThresholdStep {
            value: Some(70.0),
            color: "yellow",
            label: "Atenção",
        },
        ThresholdStep {
            value: Some(85.0),
            color: "orange",
            label: "Alto",
        },
        ThresholdStep {
            value: Some(95.0),
            color: "red",
            label: "Crítico",
        },
    ]
}

fn thr_count_zero_good() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "green",
            label: "Zero",
        },
        ThresholdStep {
            value: Some(1.0),
            color: "yellow",
            label: "Alerta",
        },
        ThresholdStep {
            value: Some(5.0),
            color: "orange",
            label: "Elevado",
        },
        ThresholdStep {
            value: Some(15.0),
            color: "red",
            label: "Crítico",
        },
    ]
}

fn thr_temp_celsius() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "green",
            label: "Normal",
        },
        ThresholdStep {
            value: Some(55.0),
            color: "yellow",
            label: "Quente",
        },
        ThresholdStep {
            value: Some(68.0),
            color: "orange",
            label: "Alto",
        },
        ThresholdStep {
            value: Some(78.0),
            color: "red",
            label: "Crítico",
        },
    ]
}

fn thr_latency_ms() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "green",
            label: "Ótimo",
        },
        ThresholdStep {
            value: Some(25.0),
            color: "yellow",
            label: "Atenção",
        },
        ThresholdStep {
            value: Some(60.0),
            color: "orange",
            label: "Alto",
        },
        ThresholdStep {
            value: Some(120.0),
            color: "red",
            label: "Crítico",
        },
    ]
}

fn thr_optical_dbm() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "red",
            label: "Crítico (LOS)",
        },
        ThresholdStep {
            value: Some(-26.0),
            color: "orange",
            label: "Atenuado",
        },
        ThresholdStep {
            value: Some(-22.0),
            color: "yellow",
            label: "Atenção",
        },
        ThresholdStep {
            value: Some(-18.0),
            color: "green",
            label: "Sinal OK",
        },
    ]
}

fn thr_rf_rssi_dbm() -> Vec<ThresholdStep> {
    vec![
        ThresholdStep {
            value: None,
            color: "red",
            label: "Crítico",
        },
        ThresholdStep {
            value: Some(-74.0),
            color: "orange",
            label: "Degradado",
        },
        ThresholdStep {
            value: Some(-66.0),
            color: "yellow",
            label: "Atenção",
        },
        ThresholdStep {
            value: Some(-58.0),
            color: "green",
            label: "Forte",
        },
    ]
}

fn tgt(ref_id: &str, expr: &str, item_key: &str, legend: &str) -> TargetSpec {
    TargetSpec {
        ref_id: ref_id.into(),
        expr: expr.into(),
        item_key: item_key.into(),
        legend: legend.into(),
    }
}

fn stat_p(
    title: &str,
    desc: &str,
    unit: &str,
    targets: Vec<TargetSpec>,
    thresholds: Vec<ThresholdStep>,
) -> PanelBlueprint {
    PanelBlueprint {
        title: title.into(),
        description: desc.into(),
        kind: PanelKind::Stat,
        w: 4,
        h: 4,
        unit: unit.into(),
        targets,
        thresholds,
        mirrored_rx_tx: false,
    }
}

fn gauge_p(
    title: &str,
    desc: &str,
    unit: &str,
    targets: Vec<TargetSpec>,
    thresholds: Vec<ThresholdStep>,
) -> PanelBlueprint {
    PanelBlueprint {
        title: title.into(),
        description: desc.into(),
        kind: PanelKind::Gauge,
        w: 4,
        h: 4,
        unit: unit.into(),
        targets,
        thresholds,
        mirrored_rx_tx: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn ts_p(
    title: &str,
    desc: &str,
    w: u32,
    h: u32,
    unit: &str,
    targets: Vec<TargetSpec>,
    thresholds: Vec<ThresholdStep>,
    mirrored: bool,
) -> PanelBlueprint {
    PanelBlueprint {
        title: title.into(),
        description: desc.into(),
        kind: PanelKind::Timeseries,
        w,
        h,
        unit: unit.into(),
        targets,
        thresholds,
        mirrored_rx_tx: mirrored,
    }
}

fn build_preset_tiers(preset: DashboardPreset) -> Vec<TierBlueprint> {
    match preset {
        DashboardPreset::IspBackboneCiscoAsr => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (Cisco ASR 1001-X)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade SLA", "Disponibilidade ICMP/SNMP do roteador de borda/core", "percent", vec![tgt("A", "avg(icmpping)*100", "icmpping", "SLA %")], thr_sla_high_good()),
                    stat_p("Peers BGP Established", "Sessões BGP ativas em Established(6)", "short", vec![tgt("A", "count(bgp.peer.state==6)", "bgp.peer.state[*]", "Peers UP")], thr_count_zero_good()),
                    gauge_p("CPU Route Processor (5m)", "Carga do Control Plane IOS-XE", "percent", vec![tgt("A", "cisco.cpu.util[cpmCPUTotal5minRev]", "cisco.cpu.util[cpmCPUTotal5minRev]", "RP CPU")], thr_pct_high_bad()),
                    gauge_p("QFP / ESP Load", "Carga do Forwarding Engine (QFP)", "percent", vec![tgt("A", "cisco.qfp.util[cpmCPUTotal5minRev.2]", "cisco.qfp.util[cpmCPUTotal5minRev.2]", "QFP Load")], thr_pct_high_bad()),
                    stat_p("Perda de Pacotes ICMP", "Perda na borda/core", "percent", vec![tgt("A", "icmppingloss", "icmppingloss", "Loss %")], thr_pct_high_bad()),
                    stat_p("Temperatura Chassi", "Sensor térmico principal ASR", "celsius", vec![tgt("A", "cisco.temp.chassis", "cisco.temp.chassis", "Temp °C")], thr_temp_celsius()),
                ],
            },
            TierBlueprint {
                title: "2. Tráfego & Capacidade (Throughput Mirrored RX/TX + P95)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Throughput Uplinks 10G/100G (RX + / TX -)",
                        "Tráfego bidirecional espelhado (RX positivo, TX espelhado negativo) via IF-MIB 64-bit",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "net.if.in[ifHCInOctets.$interface]", "net.if.in[ifHCInOctets.*]", "RX {{interface}}"),
                            tgt("B", "net.if.out[ifHCOutOctets.$interface]", "net.if.out[ifHCOutOctets.*]", "TX {{interface}}"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Utilização Agregada & Percentil 95 (P95)",
                        "Capacidade consumida nos troncos de trânsito/IX com linha P95",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "sum(net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "Aggregate RX"),
                            tgt("B", "quantile(0.95, net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "P95 RX"),
                        ],
                        thr_pct_high_bad(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Qualidade, QFP Drops & Camada Física (DOM Óptico)".into(),
                collapsed: false,
                panels: vec![
                    ts_p("QFP Drops & Tail Drops", "Descartes no data plane Cisco QFP", 8, 7, "pps", vec![tgt("A", "cisco.qfp.drops", "cisco.qfp.drops", "QFP Drops/s")], thr_count_zero_good(), false),
                    ts_p("Erros CRC / Input Errors", "Erros físicos por interface", 8, 7, "pps", vec![tgt("A", "net.if.in.errors[ifInErrors.$interface]", "net.if.in.errors[ifInErrors.*]", "CRC Errors")], thr_count_zero_good(), false),
                    ts_p("DOM Óptico Rx (dBm)", "Potência recebida nos transceivers SFP+/QSFP", 8, 7, "dBm", vec![tgt("A", "cisco.dom.rx[$interface]", "cisco.dom.rx[*]", "Rx dBm")], thr_optical_dbm(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & Tabelas de Diagnóstico".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline de Sessões BGP & Interfaces".into(),
                        description: "Histórico temporal de flapping BGP e link down".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "bgp.peer.state[$host]", "bgp.peer.state[*]", "{{peer}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Top Interfaces Críticas (Saturação / Erros / DOM)".into(),
                        description: "Tabela ordenável com coloração por threshold".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "bps".into(),
                        targets: vec![tgt("A", "net.if.in[ifHCInOctets.*]", "net.if.in[ifHCInOctets.*]", "{{interface}}")],
                        thresholds: thr_pct_high_bad(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
        DashboardPreset::IspOltGpon => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (OLT GPON/XGS-PON)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade OLT", "Disponibilidade ICMP/SNMP da OLT", "percent", vec![tgt("A", "avg(icmpping)*100", "icmpping", "SLA %")], thr_sla_high_good()),
                    stat_p("ONUs Online", "Total de ONUs autenticadas e operacionais", "short", vec![tgt("A", "sum(olt.pon.onus_online)", "olt.pon.onus_online[*]", "ONUs UP")], thr_count_zero_good()),
                    stat_p("ONUs Offline / LOS", "ONUs fora do ar (LOS / DyingGasp)", "short", vec![tgt("A", "sum(olt.pon.onus_offline)", "olt.pon.onus_offline[*]", "ONUs Down")], thr_count_zero_good()),
                    gauge_p("CPU OLT", "Carga da controladora GPON", "percent", vec![tgt("A", "olt.cpu.util", "olt.cpu.util", "CPU %")], thr_pct_high_bad()),
                    stat_p("Temperatura OLT", "Temperatura interna / placas PON", "celsius", vec![tgt("A", "olt.temp", "olt.temp", "Temp °C")], thr_temp_celsius()),
                    stat_p("ONUs Sinal Crítico (<-26 dBm)", "Clientes com atenuação óptica severa", "short", vec![tgt("A", "count(olt.onu.rx<-26)", "olt.onu.rx[*]", "Weak ONUs")], thr_count_zero_good()),
                ],
            },
            TierBlueprint {
                title: "2. Tráfego & Capacidade (Uplinks + Portas PON RX/TX)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Uplinks OLT 10G/100G (RX + / TX -)",
                        "Tráfego agregado nos uplinks rumo ao BNG/Core",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "net.if.in[ifHCInOctets.$pon_port]", "net.if.in[ifHCInOctets.*]", "RX {{interface}}"),
                            tgt("B", "net.if.out[ifHCOutOctets.$pon_port]", "net.if.out[ifHCOutOctets.*]", "TX {{interface}}"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Ocupação por Porta PON & P95",
                        "Taxa agregada Downstream/Upstream por porta PON",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "olt.pon.traffic.rx[$pon_port]", "olt.pon.traffic.rx[*]", "PON RX"),
                            tgt("B", "olt.pon.traffic.tx[$pon_port]", "olt.pon.traffic.tx[*]", "PON TX"),
                        ],
                        thr_pct_high_bad(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Qualidade Óptica GPON (OLT Tx / ONU Rx / Drops)".into(),
                collapsed: false,
                panels: vec![
                    ts_p("Potência Tx OLT PON (dBm)", "Saída óptica dos GBICs GPON B+/C+", 8, 7, "dBm", vec![tgt("A", "olt.pon.tx_power[$pon_port]", "olt.pon.tx_power[*]", "OLT Tx dBm")], thr_optical_dbm(), false),
                    ts_p("Distribuição Sinal Rx ONUs (dBm)", "Média e mínimo Rx nas ONUs da porta PON", 8, 7, "dBm", vec![tgt("A", "olt.onu.rx[$pon_port]", "olt.onu.rx[*]", "ONU Rx dBm")], thr_optical_dbm(), false),
                    ts_p("Descartes & Erros BIP/FEC", "Erros de camada física PON", 8, 7, "pps", vec![tgt("A", "net.if.in.errors[ifInErrors.$pon_port]", "net.if.in.errors[ifInErrors.*]", "PON Errors")], thr_count_zero_good(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & ONUs com Sinal Atenuado".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline de Quedas / Flapping PON".into(),
                        description: "Estado operacional das portas PON e eventos massivos".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "olt.pon.onus_online[$pon_port]", "olt.pon.onus_online[*]", "{{pon_port}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Tabela de ONUs com Atenuação (< -25 dBm)".into(),
                        description: "Lista priorizada de clientes para manutenção preventiva em campo".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "dBm".into(),
                        targets: vec![tgt("A", "olt.onu.rx[*]", "olt.onu.rx[*]", "{{onu}}")],
                        thresholds: thr_optical_dbm(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
        DashboardPreset::IspDatacomRing => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (Anel Metro Datacom DmOS)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade Anel", "Disponibilidade dos nós DmOS no anel metro", "percent", vec![tgt("A", "avg(icmpping)*100", "icmpping", "SLA %")], thr_sla_high_good()),
                    stat_p("Estado EAPS / ERPS", "Proteção de anel (1=OK, 0=Aberto/Falha)", "short", vec![tgt("A", "dmos.ring.state", "dmos.ring.state", "Ring OK")], thr_count_zero_good()),
                    gauge_p("CPU DmOS", "Utilização de CPU da gerência DmOS", "percent", vec![tgt("A", "dmos.cpu.util", "dmos.cpu.util", "CPU %")], thr_pct_high_bad()),
                    gauge_p("Memória DmOS", "Uso de RAM do equipamento", "percent", vec![tgt("A", "dmos.mem.util", "dmos.mem.util", "Mem %")], thr_pct_high_bad()),
                    stat_p("Links DOM Atenuados", "Trechos metro abaixo de -22 dBm", "short", vec![tgt("A", "count(dmos.dom.rx<-22)", "dmos.dom.rx[*]", "DOM Warn")], thr_count_zero_good()),
                    stat_p("Erros CRC / FCS Ativos", "Interfaces do anel com erro físico", "short", vec![tgt("A", "count(net.if.in.errors>0)", "net.if.in.errors[*]", "CRC Links")], thr_count_zero_good()),
                ],
            },
            TierBlueprint {
                title: "2. Tráfego & Capacidade (Troncos 10G/40G/100G RX/TX)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Throughput Anel Metro (RX + / TX -)",
                        "Fluxo bidirecional espelhado nos enlaces do anel DmOS",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "net.if.in[ifHCInOctets.$interface]", "net.if.in[ifHCInOctets.*]", "RX {{interface}}"),
                            tgt("B", "net.if.out[ifHCOutOctets.$interface]", "net.if.out[ifHCOutOctets.*]", "TX {{interface}}"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Capacidade & P95 dos Enlaces Críticos",
                        "Curva de pico horário com P95 para planejamento de upgrade",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "net.if.in[ifHCInOctets.$interface]", "net.if.in[ifHCInOctets.*]", "RX Peak"),
                            tgt("B", "quantile(0.95, net.if.in[ifHCInOctets.$interface])", "net.if.in[ifHCInOctets.*]", "P95"),
                        ],
                        thr_pct_high_bad(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Camada Física, DOM Óptico & Buffer Drops".into(),
                collapsed: false,
                panels: vec![
                    ts_p("DOM Óptico SFP+/QSFP28 (dBm)", "Potência Rx nos trechos metro", 8, 7, "dBm", vec![tgt("A", "dmos.dom.rx[$interface]", "dmos.dom.rx[*]", "Rx dBm")], thr_optical_dbm(), false),
                    ts_p("Erros CRC / FCS por Porta", "Degradação de fibra/conector", 8, 7, "pps", vec![tgt("A", "net.if.in.errors[ifInErrors.$interface]", "net.if.in.errors[ifInErrors.*]", "CRC/s")], thr_count_zero_good(), false),
                    ts_p("Descartes de Saída (QoS Drops)", "Congestionamento em portas trunk", 8, 7, "pps", vec![tgt("A", "net.if.out.discards[$interface]", "net.if.out.discards[*]", "Drops/s")], thr_count_zero_good(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & Inventário de Portas do Anel".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline de Switchover EAPS/ERPS & Links".into(),
                        description: "Registro temporal de comutação de proteção do anel".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "net.if.status[ifOperStatus.$interface]", "net.if.status[ifOperStatus.*]", "{{interface}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Matriz Óptica & Saúde das Portas DmOS".into(),
                        description: "Tabela comparativa de Rx dBm, velocidade e erros".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "dBm".into(),
                        targets: vec![tgt("A", "dmos.dom.rx[*]", "dmos.dom.rx[*]", "{{interface}}")],
                        thresholds: thr_optical_dbm(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
        DashboardPreset::IspUbiquitiRf => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (Enlaces RF Ubiquiti)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade Enlace", "Disponibilidade ICMP/SNMP do rádio", "percent", vec![tgt("A", "avg(icmpping)*100", "icmpping", "SLA %")], thr_sla_high_good()),
                    stat_p("RSSI Atual", "Nível de sinal recebido (dBm)", "dBm", vec![tgt("A", "ubnt.rf.rssi", "ubnt.rf.rssi", "RSSI")], thr_rf_rssi_dbm()),
                    gauge_p("airMAX CCQ", "Qualidade de modulação RF (%)", "percent", vec![tgt("A", "ubnt.rf.ccq", "ubnt.rf.ccq", "CCQ %")], thr_sla_high_good()),
                    stat_p("Latência Média Enlace", "RTT ICMP ponta-a-ponta", "ms", vec![tgt("A", "icmppingsec*1000", "icmppingsec", "RTT ms")], thr_latency_ms()),
                    stat_p("Perda de Pacotes RF", "Perda ICMP no enlace", "percent", vec![tgt("A", "icmppingloss", "icmppingloss", "Loss %")], thr_pct_high_bad()),
                    stat_p("Noise Floor", "Piso de ruído no espectro (dBm)", "dBm", vec![tgt("A", "ubnt.rf.noise_floor", "ubnt.rf.noise_floor", "Noise")], thr_rf_rssi_dbm()),
                ],
            },
            TierBlueprint {
                title: "2. Capacidade RF & Throughput Ethernet (RX + / TX -)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Throughput Ethernet Rádio (RX + / TX -)",
                        "Tráfego real cursado na porta Ethernet do enlace",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "net.if.in[ifHCInOctets.$rf_link]", "net.if.in[ifHCInOctets.*]", "RX Ethernet"),
                            tgt("B", "net.if.out[ifHCOutOctets.$rf_link]", "net.if.out[ifHCOutOctets.*]", "TX Ethernet"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Capacidade Agregada RF (TX vs RX Capacity)",
                        "Capacidade nominal negociada pela modulação airFiber/airMAX",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "ubnt.rf.tx_capacity", "ubnt.rf.tx_capacity", "RF TX Cap"),
                            tgt("B", "ubnt.rf.rx_capacity", "ubnt.rf.rx_capacity", "RF RX Cap"),
                        ],
                        thr_pct_high_bad(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Qualidade Espectral, SNR & Chain Balance".into(),
                collapsed: false,
                panels: vec![
                    ts_p("RSSI & Noise Floor (dBm)", "Margem de sinal sobre ruído (SNR)", 8, 7, "dBm", vec![tgt("A", "ubnt.rf.rssi", "ubnt.rf.rssi", "RSSI"), tgt("B", "ubnt.rf.noise_floor", "ubnt.rf.noise_floor", "Noise")], thr_rf_rssi_dbm(), false),
                    ts_p("Desbalanceamento Chain0 / Chain1 (dB)", "Diferença entre polarizações H/V (> 5 dB indica obstrução/alinhamento)", 8, 7, "short", vec![tgt("A", "abs(ubnt.rf.chain0-ubnt.rf.chain1)", "ubnt.rf.chain_diff", "Chain Diff dB")], thr_count_zero_good(), false),
                    ts_p("Latência & Perda RF", "Estabilidade de transporte em chuva/interferência", 8, 7, "ms", vec![tgt("A", "icmppingsec*1000", "icmppingsec", "RTT ms")], thr_latency_ms(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & Enlaces Degradados".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline de Modulação / Quedas DFS".into(),
                        description: "Eventos de mudança de canal DFS ou queda de enlace".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "icmpping", "icmpping", "{{host}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Ranking de Enlaces RF por RSSI / CCQ".into(),
                        description: "Tabela comparativa para priorização de realinhamento".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "dBm".into(),
                        targets: vec![tgt("A", "ubnt.rf.rssi", "ubnt.rf.rssi", "{{host}}")],
                        thresholds: thr_rf_rssi_dbm(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
        DashboardPreset::LinuxSreHost => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (Linux SRE Host)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade Host", "Target UP via node_exporter", "percent", vec![tgt("A", "avg(up{instance=~\"$host\"})*100", "agent.ping", "UP %")], thr_sla_high_good()),
                    gauge_p("CPU Utilization", "Uso total de CPU (user+sys+iowait)", "percent", vec![tgt("A", "100 - (avg by(instance)(rate(node_cpu_seconds_total{mode=\"idle\",instance=~\"$host\"}[5m])) * 100)", "system.cpu.util", "CPU %")], thr_pct_high_bad()),
                    gauge_p("Memória Utilizada", "RAM em uso excluindo buffers/cache", "percent", vec![tgt("A", "(1 - (node_memory_MemAvailable_bytes{instance=~\"$host\"} / node_memory_MemTotal_bytes{instance=~\"$host\"})) * 100", "vm.memory.util", "RAM %")], thr_pct_high_bad()),
                    gauge_p("Disco Raiz /", "Ocupação do filesystem /", "percent", vec![tgt("A", "(1 - (node_filesystem_avail_bytes{mountpoint=\"/\",instance=~\"$host\"} / node_filesystem_size_bytes{mountpoint=\"/\",instance=~\"$host\"})) * 100", "vfs.fs.size[/,pused]", "Disk / %")], thr_pct_high_bad()),
                    stat_p("Load Average (1m)", "Fila de execução do kernel", "short", vec![tgt("A", "node_load1{instance=~\"$host\"}", "system.cpu.load[all,avg1]", "Load 1m")], thr_count_zero_good()),
                    stat_p("Systemd Units Failed", "Serviços systemd em estado failed", "short", vec![tgt("A", "sum(node_systemd_unit_state{state=\"failed\",instance=~\"$host\"})", "systemd.unit.failed", "Failed")], thr_count_zero_good()),
                ],
            },
            TierBlueprint {
                title: "2. Rede & Conexões (Throughput NIC RX/TX + P95)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Tráfego NICs (RX + / TX -)",
                        "Throughput de rede por interface em bits/s",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "rate(node_network_receive_bytes_total{instance=~\"$host\"}[5m])*8", "net.if.in[*]", "RX {{device}}"),
                            tgt("B", "rate(node_network_transmit_bytes_total{instance=~\"$host\"}[5m])*8", "net.if.out[*]", "TX {{device}}"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Conexões TCP & Softnet Backlog",
                        "Estados ESTABLISHED / TIME_WAIT e sockets ativos",
                        12,
                        8,
                        "short",
                        vec![
                            tgt("A", "node_netstat_Tcp_CurrEstab{instance=~\"$host\"}", "net.tcp.established", "Established"),
                            tgt("B", "node_sockstat_TCP_tw{instance=~\"$host\"}", "net.tcp.timewait", "TIME_WAIT"),
                        ],
                        thr_count_zero_good(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Qualidade Kernel, I/O Latency & Packet Drops".into(),
                collapsed: false,
                panels: vec![
                    ts_p("Latência de Disco (ms)", "Tempo médio de serviço I/O NVMe/SATA", 8, 7, "ms", vec![tgt("A", "rate(node_disk_io_time_seconds_total{instance=~\"$host\"}[5m])*1000", "vfs.dev.await", "Await ms")], thr_latency_ms(), false),
                    ts_p("Packet Drops NIC/Softnet", "Descartes de RX/TX no driver ou backlog", 8, 7, "pps", vec![tgt("A", "rate(node_network_receive_drop_total{instance=~\"$host\"}[5m])", "net.if.in.dropped", "RX Drops/s")], thr_count_zero_good(), false),
                    ts_p("PSI CPU/Mem/IO Stall (%)", "Pressão de contenção Linux Pressure Stall", 8, 7, "percent", vec![tgt("A", "rate(node_pressure_cpu_waiting_seconds_total{instance=~\"$host\"}[5m])*100", "kernel.psi.cpu", "CPU Stall %")], thr_pct_high_bad(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & Tabela de Filesystems".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline Disponibilidade & Serviços".into(),
                        description: "Série temporal de disponibilidade do host e systemd".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "up{instance=~\"$host\"}", "agent.ping", "{{instance}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Uso por Filesystem / Mountpoint".into(),
                        description: "Tabela de partições ordenadas por ocupação".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "percent".into(),
                        targets: vec![tgt("A", "(1 - (node_filesystem_avail_bytes{instance=~\"$host\"} / node_filesystem_size_bytes{instance=~\"$host\"})) * 100", "vfs.fs.size[*,pused]", "{{mountpoint}}")],
                        thresholds: thr_pct_high_bad(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
        DashboardPreset::NocExecutiveOverview | DashboardPreset::Custom => vec![
            TierBlueprint {
                title: "1. Saúde Crítica & KPIs Executivos (Visão Global NOC)".into(),
                collapsed: false,
                panels: vec![
                    stat_p("Disponibilidade Global SLA", "Percentual de hosts monitorados operacionais", "percent", vec![tgt("A", "avg(icmpping)*100", "icmpping", "SLA Global %")], thr_sla_high_good()),
                    stat_p("Incidentes Críticos (Alta/Desastre)", "Alertas ativos severidade >= 4", "short", vec![tgt("A", "zabbix.problems.high_disaster", "zabbix.problems[high_disaster]", "Críticos")], thr_count_zero_good()),
                    stat_p("Hosts Indisponíveis", "Equipamentos fora do ar no Zabbix", "short", vec![tgt("A", "zabbix.hosts.unavailable", "zabbix.hosts[unavailable]", "Offline")], thr_count_zero_good()),
                    stat_p("Tráfego Pico Backbone", "Somatório de entradas nos upstreams", "bps", vec![tgt("A", "sum(net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "Backbone bps")], thr_pct_high_bad()),
                    stat_p("Latência Média Core", "Latência média nos POPs de backbone", "ms", vec![tgt("A", "avg(icmppingsec)*1000", "icmppingsec", "RTT ms")], thr_latency_ms()),
                    stat_p("Perda de Pacotes Média", "Perda média ICMP nos POPs", "percent", vec![tgt("A", "avg(icmppingloss)", "icmppingloss", "Loss %")], thr_pct_high_bad()),
                ],
            },
            TierBlueprint {
                title: "2. Tráfego & Capacidade Global (RX + / TX - + P95)".into(),
                collapsed: false,
                panels: vec![
                    ts_p(
                        "Tráfego Agregado Trânsito & IX (RX + / TX -)",
                        "Capacidade consumida nos upstreams IP do provedor",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "sum(net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "Download (RX)"),
                            tgt("B", "sum(net.if.out[ifHCOutOctets.*])", "net.if.out[ifHCOutOctets.*]", "Upload (TX)"),
                        ],
                        thr_pct_high_bad(),
                        true,
                    ),
                    ts_p(
                        "Top POPs por Tráfego & Percentil 95",
                        "Comparativo entre POPs/Sites com P95 para expansão",
                        12,
                        8,
                        "bps",
                        vec![
                            tgt("A", "sum by(site)(net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "{{site}}"),
                            tgt("B", "quantile(0.95, net.if.in[ifHCInOctets.*])", "net.if.in[ifHCInOctets.*]", "P95 Global"),
                        ],
                        thr_pct_high_bad(),
                        false,
                    ),
                ],
            },
            TierBlueprint {
                title: "3. Qualidade Física & Degradação Preventiva".into(),
                collapsed: false,
                panels: vec![
                    ts_p("Perda de Pacotes por POP (%)", "Evolução de packet loss nos enlaces críticos", 8, 7, "percent", vec![tgt("A", "icmppingloss", "icmppingloss", "{{host}}")], thr_pct_high_bad(), false),
                    ts_p("Links com DOM/RF Atenuado", "Contagem de transceivers ou enlaces RF fora da faixa ideal", 8, 7, "short", vec![tgt("A", "count(dom.rx<-22)", "dom.rx[*]", "Links Atenuados")], thr_count_zero_good(), false),
                    ts_p("Taxa de Flapping BGP / Interfaces", "Mudanças de estado por hora", 8, 7, "short", vec![tgt("A", "changes(bgp.peer.state[1h])", "bgp.peer.state[*]", "Flaps/h")], thr_count_zero_good(), false),
                ],
            },
            TierBlueprint {
                title: "4. Estado Temporal & Top Ofensores NOC".into(),
                collapsed: false,
                panels: vec![
                    PanelBlueprint {
                        title: "Timeline de Incidentes Críticos".into(),
                        description: "Linha do tempo de indisponibilidades e mudanças anotadas".into(),
                        kind: PanelKind::StateTimeline,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "icmpping", "icmpping", "{{host}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                    PanelBlueprint {
                        title: "Top Hosts / POPs Ofensores".into(),
                        description: "Tabela executiva ordenada por incidentes ativos e SLA".into(),
                        kind: PanelKind::Table,
                        w: 12,
                        h: 8,
                        unit: "short".into(),
                        targets: vec![tgt("A", "zabbix.problems.by_host", "zabbix.problems[*]", "{{host}}")],
                        thresholds: thr_count_zero_good(),
                        mirrored_rx_tx: false,
                    },
                ],
            },
        ],
    }
}

/// Deterministic 24-column Grafana grid layout engine.
/// Guarantees zero overlapping panels and `x + w <= 24` for every panel.
fn layout_tiers(tiers: Vec<TierBlueprint>) -> Vec<PlacedTier> {
    let mut next_id = 1u32;
    let mut cur_y = 0u32;
    let mut placed_tiers = Vec::with_capacity(tiers.len());

    for tier in tiers {
        let row_id = next_id;
        next_id += 1;
        let row_grid = GridPos {
            x: 0,
            y: cur_y,
            w: 24,
            h: 1,
        };
        cur_y += 1;

        let mut cur_x = 0u32;
        let mut row_max_h = 0u32;
        let mut placed_panels = Vec::with_capacity(tier.panels.len());

        for mut spec in tier.panels {
            spec.w = spec.w.clamp(1, 24);
            spec.h = spec.h.max(1);
            if cur_x + spec.w > 24 {
                cur_y += row_max_h;
                cur_x = 0;
                row_max_h = 0;
            }
            let grid = GridPos {
                x: cur_x,
                y: cur_y,
                w: spec.w,
                h: spec.h,
            };
            cur_x += spec.w;
            row_max_h = row_max_h.max(spec.h);

            let id = next_id;
            next_id += 1;
            placed_panels.push(PlacedPanel { id, grid, spec });
        }

        if row_max_h > 0 {
            cur_y += row_max_h;
        }

        placed_tiers.push(PlacedTier {
            row_id,
            title: tier.title,
            collapsed: tier.collapsed,
            row_grid,
            panels: placed_panels,
        });
    }

    placed_tiers
}

// --- Renderers: Grafana v39 JSON & Portable NOC JSON ------------------------

fn render_grafana_json(model: &NocDashboardModel) -> Value {
    let mut panels_out = Vec::new();
    for tier in &model.tiers {
        panels_out.push(json!({
            "id": tier.row_id,
            "type": "row",
            "title": tier.title,
            "collapsed": tier.collapsed,
            "gridPos": {
                "x": tier.row_grid.x,
                "y": tier.row_grid.y,
                "w": tier.row_grid.w,
                "h": tier.row_grid.h
            },
            "panels": []
        }));
        for p in &tier.panels {
            panels_out.push(render_grafana_panel(p, &model.datasource));
        }
    }

    let templating_list: Vec<Value> = model
        .variables
        .iter()
        .map(|v| {
            let mut obj = json!({
                "name": v.name,
                "label": v.label,
                "type": v.kind,
                "multi": v.multi,
                "includeAll": v.include_all,
                "refresh": 1,
                "hide": 0
            });
            if v.kind == "datasource" {
                obj["query"] = json!(v.query);
            } else {
                obj["datasource"] =
                    json!({"type": model.datasource.kind, "uid": model.datasource.uid});
                obj["query"] = json!({"query": v.query, "refId": "StandardVariableQuery"});
            }
            obj
        })
        .collect();

    json!({
        "uid": model.uid,
        "title": model.title,
        "description": model.description,
        "tags": model.tags,
        "timezone": "browser",
        "editable": true,
        "schemaVersion": 39,
        "version": 1,
        "graphTooltip": 1,
        "refresh": format!("{}s", model.refresh_secs),
        "time": {
            "from": model.time_from,
            "to": model.time_to
        },
        "templating": {
            "list": templating_list
        },
        "annotations": {
            "list": [
                {
                    "builtIn": 1,
                    "datasource": {"type": "grafana", "uid": "-- Grafana --"},
                    "enable": true,
                    "hide": true,
                    "iconColor": "rgba(0, 211, 255, 1)",
                    "name": "Annotations & Alerts",
                    "type": "dashboard"
                },
                {
                    "datasource": {"type": "grafana", "uid": "-- Grafana --"},
                    "enable": true,
                    "hide": false,
                    "iconColor": "orange",
                    "name": "Mudanças & Manutenções NOC",
                    "tags": ["noc", "maintenance", "change"],
                    "type": "tags"
                }
            ]
        },
        "panels": panels_out
    })
}

fn render_grafana_panel(p: &PlacedPanel, ds: &DatasourceSpec) -> Value {
    let steps: Vec<Value> = p
        .spec
        .thresholds
        .iter()
        .map(|s| {
            let val = match s.value {
                Some(v) => json!(v),
                None => Value::Null,
            };
            json!({"color": s.color, "value": val})
        })
        .collect();

    let targets: Vec<Value> = p
        .spec
        .targets
        .iter()
        .map(|t| {
            if ds.kind.contains("zabbix") {
                json!({
                    "refId": t.ref_id,
                    "datasource": {"type": ds.kind, "uid": ds.uid},
                    "queryType": "0",
                    "group": {"filter": "$site"},
                    "host": {"filter": "$host"},
                    "item": {"filter": t.item_key},
                    "expr": t.expr,
                    "legendFormat": t.legend
                })
            } else {
                json!({
                    "refId": t.ref_id,
                    "datasource": {"type": ds.kind, "uid": ds.uid},
                    "expr": t.expr,
                    "legendFormat": t.legend
                })
            }
        })
        .collect();

    let mut overrides = Vec::new();
    if p.spec.mirrored_rx_tx {
        overrides.push(json!({
            "matcher": {"id": "byRegexp", "options": "/(?i)(tx|upload|out|egress)/"},
            "properties": [
                {"id": "custom.transform", "value": "negative-Y"},
                {"id": "color", "value": {"mode": "fixed", "fixedColor": "#5794F2"}}
            ]
        }));
        overrides.push(json!({
            "matcher": {"id": "byRegexp", "options": "/(?i)(rx|download|in|ingress)/"},
            "properties": [
                {"id": "color", "value": {"mode": "fixed", "fixedColor": "#73BF69"}}
            ]
        }));
    }

    let mut defaults = json!({
        "unit": p.spec.unit,
        "thresholds": {
            "mode": "absolute",
            "steps": steps
        },
        "color": {"mode": "thresholds"}
    });

    let options = match p.spec.kind {
        PanelKind::Stat => json!({
            "reduceOptions": {"values": false, "calcs": ["lastNotNull"], "fields": ""},
            "colorMode": "background",
            "graphMode": "area",
            "textMode": "auto"
        }),
        PanelKind::Gauge => json!({
            "reduceOptions": {"values": false, "calcs": ["lastNotNull"], "fields": ""},
            "showThresholdLabels": false,
            "showThresholdMarkers": true
        }),
        PanelKind::Timeseries => {
            defaults["color"] = json!({"mode": "palette-classic"});
            defaults["custom"] = json!({
                "drawStyle": "line",
                "lineInterpolation": "smooth",
                "lineWidth": 2,
                "fillOpacity": 18,
                "spanNulls": true
            });
            json!({
                "legend": {
                    "displayMode": "table",
                    "placement": "bottom",
                    "showLegend": true,
                    "calcs": ["lastNotNull", "mean", "max"]
                },
                "tooltip": {"mode": "multi", "sort": "desc"}
            })
        }
        PanelKind::Table => {
            defaults["custom"] = json!({
                "align": "auto",
                "cellOptions": {"type": "color-background"},
                "inspect": false
            });
            json!({
                "showHeader": true,
                "cellHeight": "sm",
                "footer": {"show": false}
            })
        }
        PanelKind::StateTimeline => {
            defaults["mappings"] = json!([
                {"type": "value", "options": {"0": {"text": "DOWN", "color": "red"}, "1": {"text": "UP", "color": "green"}}}
            ]);
            json!({
                "mergeValues": true,
                "showValue": "auto",
                "alignValue": "left"
            })
        }
        PanelKind::Logs => json!({
            "showTime": true,
            "wrapLogMessage": true,
            "enableLogDetails": true
        }),
    };

    json!({
        "id": p.id,
        "type": p.spec.kind.grafana_type(),
        "title": p.spec.title,
        "description": p.spec.description,
        "gridPos": {
            "x": p.grid.x,
            "y": p.grid.y,
            "w": p.grid.w,
            "h": p.grid.h
        },
        "datasource": {"type": ds.kind, "uid": ds.uid},
        "targets": targets,
        "fieldConfig": {
            "defaults": defaults,
            "overrides": overrides
        },
        "options": options
    })
}

fn render_noc_portable_json(model: &NocDashboardModel) -> Value {
    let variables: Vec<Value> = model
        .variables
        .iter()
        .map(|v| {
            json!({
                "name": v.name,
                "label": v.label,
                "type": v.kind,
                "query": v.query,
                "multi": v.multi,
                "include_all": v.include_all
            })
        })
        .collect();

    let rows: Vec<Value> = model
        .tiers
        .iter()
        .map(|tier| {
            let panels: Vec<Value> = tier
                .panels
                .iter()
                .map(|p| {
                    let targets: Vec<Value> = p
                        .spec
                        .targets
                        .iter()
                        .map(|t| {
                            json!({
                                "refId": t.ref_id,
                                "expr": t.expr,
                                "item_key": t.item_key,
                                "legend": t.legend
                            })
                        })
                        .collect();
                    let steps: Vec<Value> = p
                        .spec
                        .thresholds
                        .iter()
                        .map(|s| {
                            json!({
                                "value": s.value,
                                "color": s.color,
                                "label": s.label
                            })
                        })
                        .collect();
                    json!({
                        "id": p.id,
                        "type": p.spec.kind.portable_type(),
                        "title": p.spec.title,
                        "description": p.spec.description,
                        "gridPos": {
                            "x": p.grid.x,
                            "y": p.grid.y,
                            "w": p.grid.w,
                            "h": p.grid.h
                        },
                        "datasource": {
                            "name": model.datasource.name,
                            "type": model.datasource.kind,
                            "uid": model.datasource.uid
                        },
                        "targets": targets,
                        "unit": p.spec.unit,
                        "thresholds": {
                            "mode": "absolute",
                            "steps": steps
                        },
                        "options": {
                            "mirrored_rx_tx": p.spec.mirrored_rx_tx
                        }
                    })
                })
                .collect();

            json!({
                "id": tier.row_id,
                "title": tier.title,
                "collapsed": tier.collapsed,
                "gridPos": {
                    "x": tier.row_grid.x,
                    "y": tier.row_grid.y,
                    "w": tier.row_grid.w,
                    "h": tier.row_grid.h
                },
                "panels": panels
            })
        })
        .collect();

    json!({
        "format": "zerith-noc-portable/v1",
        "uid": model.uid,
        "title": model.title,
        "description": model.description,
        "tags": model.tags,
        "refresh_secs": model.refresh_secs,
        "time": {
            "from": model.time_from,
            "to": model.time_to
        },
        "grid": {
            "columns": 24,
            "row_height_px": 30
        },
        "datasource": {
            "name": model.datasource.name,
            "type": model.datasource.kind,
            "uid": model.datasource.uid
        },
        "variables": variables,
        "rows": rows
    })
}

fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(
        ".grafana-dash.{}.{seq}.{nanos}.zerith-tmp",
        std::process::id()
    ));
    let res = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, target)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn pretty_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string_pretty(v).map_err(|e| ToolError::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use zerith_providers::Cancel;
    use zerith_security::{preset, Policy};

    #[derive(Debug)]
    struct RecordedReq {
        method: String,
        path: String,
        auth: String,
        body: Value,
    }

    fn fake_grafana(
        answer: impl Fn(&str, &str, &Value) -> (u16, Value) + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<RecordedReq>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<RecordedReq>>> = Arc::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut rd = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                if rd.read_line(&mut req_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = req_line.split_whitespace();
                let method = parts.next().unwrap_or("GET").to_string();
                let path = parts.next().unwrap_or("/").to_string();

                let mut len = 0usize;
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    } else if let Some(v) = line.split_once(':') {
                        if v.0.eq_ignore_ascii_case("authorization") {
                            auth = v.1.trim().to_string();
                        }
                    }
                }
                let mut body_bytes = vec![0; len];
                if len > 0 {
                    rd.read_exact(&mut body_bytes).unwrap();
                }
                let body: Value = if body_bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body_bytes).unwrap_or(Value::Null)
                };
                let (code, resp_json) = answer(&method, &path, &body);
                log.lock().unwrap().push(RecordedReq {
                    method,
                    path,
                    auth,
                    body,
                });
                let resp_str = resp_json.to_string();
                let status_text = if code == 200 { "OK" } else { "Error" };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_str}",
                    resp_str.len()
                );
            }
        });
        (url, seen)
    }

    fn assert_no_grid_collisions(panels: &[Value], context: &str) {
        let mut boxes: Vec<(u32, u32, u32, u32, String)> = Vec::new();
        for p in panels {
            let ptype = p["type"].as_str().unwrap_or("");
            let gp = &p["gridPos"];
            let x = gp["x"].as_u64().unwrap() as u32;
            let y = gp["y"].as_u64().unwrap() as u32;
            let w = gp["w"].as_u64().unwrap() as u32;
            let h = gp["h"].as_u64().unwrap() as u32;
            assert!((1..=24).contains(&w), "{context}: invalid width {w}");
            assert!(
                x + w <= 24,
                "{context}: panel overflows 24 cols (x={x}, w={w})"
            );
            assert!(h >= 1, "{context}: invalid height {h}");
            if ptype == "row" {
                assert_eq!(x, 0, "{context}: row must start at x=0");
                assert_eq!(w, 24, "{context}: row must span 24 cols");
            }
            for (bx, by, bw, bh, btitle) in &boxes {
                let x_overlap = x < bx + bw && *bx < x + w;
                let y_overlap = y < by + bh && *by < y + h;
                assert!(
                    !(x_overlap && y_overlap),
                    "{context}: overlap between '{}' ({x},{y},{w},{h}) and '{btitle}' ({bx},{by},{bw},{bh})",
                    p["title"]
                );
            }
            boxes.push((x, y, w, h, p["title"].as_str().unwrap_or("").to_string()));
        }
    }

    #[test]
    fn all_presets_generate_valid_24col_grafana_and_portable_layouts() {
        let presets = [
            "isp_backbone_cisco_asr",
            "isp_olt_gpon",
            "isp_datacom_ring",
            "isp_ubiquiti_rf",
            "linux_sre_host",
            "noc_executive_overview",
        ];

        for preset_name in presets {
            let bundle = generate_dashboard_bundle(&json!({
                "action": "generate_dashboard",
                "preset": preset_name,
                "format": "both"
            }))
            .unwrap();

            let graf = &bundle.output_value["grafana_json"];
            assert_eq!(graf["schemaVersion"], 39, "{preset_name}");
            assert_eq!(graf["editable"], true, "{preset_name}");
            assert_eq!(graf["refresh"], "30s", "{preset_name}");
            assert_eq!(graf["time"]["from"], "now-6h", "{preset_name}");
            assert_eq!(graf["time"]["to"], "now", "{preset_name}");
            assert!(
                graf["tags"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("zerith-noc")),
                "{preset_name}: missing zerith-noc tag"
            );

            let vars = graf["templating"]["list"].as_array().unwrap();
            assert!(vars.iter().any(|v| v["name"] == "datasource"));
            assert!(vars.iter().any(|v| v["name"] == "site"));
            assert!(vars.iter().any(|v| v["name"] == "host"));

            let panels = graf["panels"].as_array().unwrap();
            let row_count = panels.iter().filter(|p| p["type"] == "row").count();
            assert_eq!(row_count, 4, "{preset_name}: expected 4 NOC tier rows");
            assert_no_grid_collisions(panels, preset_name);

            // Verify noc_portable_json structure
            let port = &bundle.output_value["noc_portable_json"];
            assert_eq!(port["format"], "zerith-noc-portable/v1");
            assert_eq!(port["grid"]["columns"], 24);
            assert_eq!(port["refresh_secs"], 30);
            let rows = port["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 4);
            assert!(!port["variables"].as_array().unwrap().is_empty());

            for r in rows {
                for p in r["panels"].as_array().unwrap() {
                    let gp = &p["gridPos"];
                    assert!(gp["x"].as_u64().unwrap() + gp["w"].as_u64().unwrap() <= 24);
                    assert!(!p["targets"].as_array().unwrap().is_empty());
                    assert_eq!(p["thresholds"]["mode"], "absolute");
                    assert!(!p["thresholds"]["steps"].as_array().unwrap().is_empty());
                }
            }
        }
    }

    #[test]
    fn custom_preset_and_offline_atomic_write_work_without_token_secret() {
        let dir = TempDir::new().unwrap();
        let pol = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Default,
            dir.path(),
        );
        let ctx = CallCtx {
            workspace: dir.path(),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let cancel = Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: std::time::Instant::now() + Duration::from_secs(10),
            timeout: Duration::from_secs(10),
            host: &host,
        };
        let tool = NetGrafanaTool::new();
        assert!(tool.concurrency_safe(&json!({
            "action": "generate_dashboard",
            "preset": "custom"
        })));
        assert!(!tool.concurrency_safe(&json!({
            "action": "generate_dashboard",
            "preset": "custom",
            "publish": true
        })));

        let out = tool
            .run(
                &json!({
                    "action": "generate_dashboard",
                    "preset": "custom",
                    "title": "NOC Custom Pop",
                    "format": "noc_portable_json",
                    "panels": [
                        {"title": "KPI A", "type": "stat", "w": 6, "h": 4, "unit": "percent", "expr": "up*100"},
                        {"title": "Throughput", "type": "timeseries", "w": 18, "h": 8, "unit": "bps", "mirrored_rx_tx": true}
                    ],
                    "output_path": "dashboards/custom-noc.json"
                }),
                &ctx,
                &exec,
            )
            .unwrap();
        assert!(out.contains("\"saved\": true"), "{out}");

        let written =
            std::fs::read_to_string(dir.path().join("dashboards/custom-noc.json")).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["format"], "zerith-noc-portable/v1");
        assert_eq!(parsed["title"], "NOC Custom Pop");
    }

    #[test]
    fn secrets_are_enforced_and_redacted() {
        // Plaintext token refused
        let err = resolve_url_and_token(
            &json!({"url": "https://graf.local", "token": "glsa_plain", "token_secret": "g"}),
            |_| Ok("ok".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("texto puro"), "{err}");

        // Missing token_secret refused for API calls
        let err = resolve_url_and_token(&json!({"url": "https://graf.local"}), |_| Ok("ok".into()))
            .unwrap_err();
        assert!(err.to_string().contains("token_secret"), "{err}");

        // Redaction masks full secret and Bearer header
        let redacted = redact_secrets(
            "HTTP 401 Authorization: Bearer glsa_super_secret_123 failed",
            &[
                "Bearer glsa_super_secret_123".into(),
                "glsa_super_secret_123".into(),
            ],
        );
        assert!(!redacted.contains("glsa_super_secret_123"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn loopback_http_mock_covers_health_search_save_and_annotations() {
        let (url, seen) = fake_grafana(|method, path, body| match (method, path) {
            ("GET", "/api/health") => (200, json!({"database": "ok", "version": "11.1.0"})),
            ("GET", "/api/org") => (200, json!({"id": 1, "name": "ISP NOC"})),
            ("GET", p) if p.starts_with("/api/search") => (
                200,
                json!([{"uid": "noc-core", "title": "Backbone ASR", "type": "dash-db"}]),
            ),
            ("POST", "/api/dashboards/db") => (
                200,
                json!({
                    "id": 42,
                    "uid": body["dashboard"]["uid"],
                    "status": "success",
                    "url": "/d/noc-core/backbone-asr"
                }),
            ),
            ("POST", "/api/annotations") => (200, json!({"id": 99, "message": "Annotation added"})),
            ("GET", p) if p.starts_with("/api/annotations") => {
                (200, json!([{"id": 99, "text": "Troca SFP Pop Sul"}]))
            }
            _ => (404, json!({"message": "not found"})),
        });

        let client = GrafanaClient::new(
            &url,
            "glsa_secret_token_xyz".into(),
            Duration::from_secs(5),
            false,
        )
        .unwrap();

        let health = execute_with_client(&client, &json!({"action": "health"})).unwrap();
        assert!(health.contains("11.1.0"), "{health}");
        assert!(health.contains("ISP NOC"), "{health}");

        let search =
            execute_with_client(&client, &json!({"action": "search", "query": "Backbone"}))
                .unwrap();
        assert!(search.contains("noc-core"), "{search}");

        let bundle = generate_dashboard_bundle(&json!({
            "action": "generate_dashboard",
            "preset": "isp_backbone_cisco_asr"
        }))
        .unwrap();
        let saved = execute_with_client(
            &client,
            &json!({
                "action": "save_dashboard",
                "dashboard": bundle.grafana_json,
                "message": "Deploy NOC v1"
            }),
        )
        .unwrap();
        assert!(saved.contains("\"status\": \"success\""), "{saved}");

        let ann = execute_with_client(
            &client,
            &json!({
                "action": "annotations",
                "op": "create",
                "text": "Troca SFP Pop Sul",
                "tags": ["noc", "fibra"]
            }),
        )
        .unwrap();
        assert!(ann.contains("Annotation added"), "{ann}");

        // Verify Authorization Bearer header and secret redaction on error
        let reqs = seen.lock().unwrap();
        assert!(reqs
            .iter()
            .all(|r| r.auth == "Bearer glsa_secret_token_xyz"));
        let post_dash = reqs
            .iter()
            .find(|r| r.path == "/api/dashboards/db")
            .unwrap();
        assert_eq!(post_dash.method, "POST");
        assert_eq!(post_dash.body["overwrite"], true);
        assert_eq!(post_dash.body["dashboard"]["schemaVersion"], 39);
    }

    #[test]
    fn concurrency_and_policy_classify_grafana_actions() {
        let tool = NetGrafanaTool::new();
        assert!(tool.concurrency_safe(&json!({"action": "health"})));
        assert!(tool.concurrency_safe(&json!({"action": "datasources"})));
        assert!(tool.concurrency_safe(&json!({"action": "search"})));
        assert!(tool.concurrency_safe(&json!({"action": "get_dashboard"})));
        assert!(tool.concurrency_safe(&json!({"action": "alerts"})));
        assert!(tool.concurrency_safe(&json!({"action": "query"})));
        assert!(tool.concurrency_safe(&json!({"action": "folders"})));
        assert!(tool.concurrency_safe(&json!({"action": "annotations"})));
        assert!(!tool.concurrency_safe(&json!({"action": "folders", "op": "create"})));
        assert!(!tool.concurrency_safe(&json!({"action": "annotations", "op": "create"})));
        assert!(!tool.concurrency_safe(&json!({"action": "annotations", "text": "Mudança"})));
        assert!(!tool.concurrency_safe(&json!({"action": "save_dashboard"})));
        assert!(!tool.concurrency_safe(&json!({"action": "delete_dashboard"})));

        let ws = std::path::Path::new(".");
        let plan_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Plan, ws);
        let plan_ctx = CallCtx {
            workspace: ws,
            policy: &plan_pol,
            memory: None,
            scope: None,
        };
        assert!(tool
            .action(&json!({"action": "health", "url": "https://g"}), &plan_ctx)
            .is_ok());
        assert!(matches!(
            tool.action(
                &json!({"action": "save_dashboard", "url": "https://g"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            tool.action(
                &json!({"action": "generate_dashboard", "publish": true, "url": "https://g"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
    }
}
