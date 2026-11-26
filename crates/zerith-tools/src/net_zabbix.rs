//! Ferramenta `net_zabbix`: consulta e operação na API JSON-RPC do Zabbix (7.0 LTS+).
//!
//! Reutiliza `zerith_workflows::zabbix::Client` (com suporte a `insecure_tls` e
//! `auth_mode: header | auth_field`). Tokens em texto puro são recusados: apenas
//! segredos do cofre (`token_secret`, e opcionalmente `url_secret`).

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::{Action, Mode, Sandbox};
use zerith_workflows::zabbix::{AuthMode, Client};

use crate::fntool::opt_str;
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(NetZabbixTool::new())]
}

struct NetZabbixTool {
    spec: ToolSpec,
}

impl NetZabbixTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: "net_zabbix".into(),
                description: "Consulta e opera o Zabbix via JSON-RPC (7.0 LTS+): \
                    `problems` (incidentes ativos), `hosts_unavailable` (ICMP/agente/SNMP fora do ar), \
                    `hosts`, `items`, `history`, `ack` (reconhece/comenta/fecha evento) e `raw` \
                    (qualquer método JSON-RPC). Exige `token_secret` no cofre; texto puro é recusado."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["problems", "hosts_unavailable", "hosts", "items", "history", "ack", "raw"],
                            "description": "operação no Zabbix (padrão: problems)"
                        },
                        "url": {
                            "type": "string",
                            "description": "URL base do Zabbix (ex.: https://zabbix.empresa/zabbix)"
                        },
                        "url_secret": {
                            "type": "string",
                            "description": "opcional: nome do segredo no cofre com a URL do Zabbix"
                        },
                        "token_secret": {
                            "type": "string",
                            "description": "nome do segredo no cofre com o token da API do Zabbix"
                        },
                        "auth_mode": {
                            "type": "string",
                            "enum": ["header", "auth_field"],
                            "description": "header (Bearer, padrão) | auth_field (Zabbix < 6.4)"
                        },
                        "insecure_tls": {
                            "type": "boolean",
                            "description": "aceita certificado HTTPS autoassinado/interno (padrão false)"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "timeout em segundos (padrão 30, máx 120)"
                        },
                        "groups": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "nomes exatos de host groups no Zabbix"
                        },
                        "host": {
                            "type": "string",
                            "description": "filtro por nome do host"
                        },
                        "hostids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de hostids"
                        },
                        "itemids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de itemids (obrigatório em history)"
                        },
                        "eventids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de eventids (obrigatório em ack)"
                        },
                        "severities": {
                            "type": "array",
                            "items": {"type": "integer"},
                            "description": "lista de severidades 0..5"
                        },
                        "min_severity": {
                            "type": "integer",
                            "description": "severidade mínima 1..5"
                        },
                        "patterns": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "padrões de busca para hosts_unavailable ou items"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "limite máximo de registros"
                        },
                        "message": {
                            "type": "string",
                            "description": "ack: mensagem do reconhecimento"
                        },
                        "acknowledge": {
                            "type": "boolean",
                            "description": "ack: marca como reconhecido (padrão true)"
                        },
                        "close": {
                            "type": "boolean",
                            "description": "ack: tenta fechar o problema (padrão false)"
                        },
                        "severity": {
                            "type": "integer",
                            "description": "ack: altera severidade para 0..5"
                        },
                        "history_type": {
                            "type": "integer",
                            "description": "history: 0=float, 1=str, 2=log, 3=uint, 4=text (auto se omitido)"
                        },
                        "method": {
                            "type": "string",
                            "description": "raw: método JSON-RPC (ex.: host.get, maintenance.create)"
                        },
                        "params": {
                            "type": "object",
                            "description": "raw: objeto de parâmetros JSON-RPC"
                        }
                    },
                    "required": ["token_secret"]
                }),
                returns: "JSON formatado com problemas, hosts, itens, histórico ou confirmação do ack",
                permission: Permission::Network,
                timeout: Duration::from_secs(60),
                cancellable: true,
                local: false,
                source: "built-in".into(),
            },
        }
    }
}

fn zabbix_action(args: &Value) -> &str {
    opt_str(args, "action").unwrap_or("problems")
}

pub(crate) fn is_readonly_zabbix_method(method: &str) -> bool {
    let low = method.trim().to_ascii_lowercase();
    low.ends_with(".get") || low == "apiinfo.version"
}

fn is_readonly_call(args: &Value) -> bool {
    match zabbix_action(args) {
        "problems" | "hosts_unavailable" | "hosts" | "items" | "history" => true,
        "raw" => opt_str(args, "method").is_some_and(is_readonly_zabbix_method),
        _ => false,
    }
}

impl Tool for NetZabbixTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        is_readonly_call(args)
    }

    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        let act = zabbix_action(args);
        if !is_readonly_call(args)
            && (ctx.policy.preset.sandbox == Sandbox::ReadOnly || ctx.policy.mode() == Mode::Plan)
        {
            return Err(ToolError::Denied(format!(
                "operação de escrita no Zabbix (`{act}`) bloqueada no modo plan/read-only"
            )));
        }
        let target = opt_str(args, "url")
            .or_else(|| opt_str(args, "url_secret"))
            .unwrap_or("zabbix");
        let label = if act == "raw" {
            let m = opt_str(args, "method").unwrap_or("raw");
            format!("zabbix raw {m} {target}")
        } else {
            format!("zabbix {act} {target}")
        };
        Ok(Some(Action::Network(label)))
    }

    fn run(&self, args: &Value, _ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        let (url, token) = resolve_url_and_token(args, |name| {
            let v = zerith_core::secrets::get_secret(name).map_err(|e| {
                ToolError::Failed(format!(
                    "segredo '{name}' não encontrado no cofre: {e} (grave com `zerith secret set {name}`)"
                ))
            })?;
            exec.host.note_secret(&v);
            Ok(v)
        })?;

        let auth = match opt_str(args, "auth_mode") {
            Some("auth_field" | "body" | "auth") => AuthMode::Body,
            _ => AuthMode::Header,
        };
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 120);
        let insecure = args
            .get("insecure_tls")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let client = Client::new(
            &url,
            token,
            auth,
            Duration::from_secs(timeout_secs),
            insecure,
        )
        .map_err(|e| ToolError::Failed(e.to_string()))?;

        let args_owned = args.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = execute_with_client(&client, &args_owned);
            let _ = tx.send(res);
        });

        loop {
            exec.check()?;
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(res) => return res,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ToolError::Failed(
                        "chamada ao Zabbix interrompida inesperadamente".into(),
                    ))
                }
            }
        }
    }
}

fn resolve_url_and_token(
    args: &Value,
    mut lookup_secret: impl FnMut(&str) -> Result<String>,
) -> Result<(String, String)> {
    if args.get("token").is_some_and(|v| !v.is_null()) {
        return Err(ToolError::BadArguments(
            "token do Zabbix em texto puro não é aceito: grave no cofre (`zerith secret set <nome>`) e informe `token_secret`".into(),
        ));
    }
    let token_secret = opt_str(args, "token_secret").ok_or_else(|| {
        ToolError::BadArguments(
            "token_secret é obrigatório (nome do segredo no cofre com o token da API do Zabbix)"
                .into(),
        )
    })?;
    let token = lookup_secret(token_secret)?.trim().to_string();
    if token.is_empty() {
        return Err(ToolError::Failed(format!(
            "segredo '{token_secret}' está vazio no cofre"
        )));
    }

    let url = if let Some(u) = opt_str(args, "url") {
        u.to_string()
    } else if let Some(u_sec) = opt_str(args, "url_secret") {
        lookup_secret(u_sec)?.trim().to_string()
    } else {
        return Err(ToolError::BadArguments(
            "informe `url` (ex.: https://zabbix.empresa/zabbix) ou `url_secret`".into(),
        ));
    };

    Ok((url, token))
}

fn string_list(args: &Value, key: &str, sep: char) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::trim)
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(sep)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_severities(args: &Value) -> Option<Vec<u8>> {
    if let Some(arr) = args.get("severities").and_then(Value::as_array) {
        let v: Vec<u8> = arr
            .iter()
            .filter_map(|x| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()))
            .filter(|n| *n <= 5)
            .map(|n| n as u8)
            .collect();
        return (!v.is_empty()).then_some(v);
    }
    args.get("min_severity")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|n| *n > 0 && *n <= 5)
        .map(|n| (n as u8..=5).collect())
}

fn resolve_hostids(client: &Client, args: &Value) -> Result<Option<Vec<String>>> {
    let ids = string_list(args, "hostids", ',');
    if !ids.is_empty() {
        return Ok(Some(ids));
    }
    let Some(host) = opt_str(args, "host") else {
        return Ok(None);
    };
    let found = client
        .call_list(
            "host.get",
            json!({
                "output": ["hostid", "host"],
                "search": {"host": host, "name": host},
                "searchByAny": true,
                "limit": 50
            }),
        )
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let resolved: Vec<String> = found
        .into_iter()
        .filter_map(|h| {
            h.get("hostid").and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| Some(v.to_string()))
            })
        })
        .collect();
    Ok(Some(resolved))
}

pub(crate) fn execute_with_client(client: &Client, args: &Value) -> Result<String> {
    let action = zabbix_action(args);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;

    match action {
        "problems" => {
            let group_names = string_list(args, "groups", ',');
            let gids = client
                .group_ids(&group_names)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let sevs = parse_severities(args);
            let mut probs = client
                .problems(gids.as_deref(), sevs.as_deref())
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            if let Some(host_filter) = opt_str(args, "host") {
                let needle = host_filter.to_ascii_lowercase();
                probs.retain(|p| p.host.to_ascii_lowercase().contains(&needle));
            }
            probs.truncate(limit);
            pretty_json(&probs)
        }
        "hosts_unavailable" => {
            let group_names = string_list(args, "groups", ',');
            let gids = client
                .group_ids(&group_names)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let mut pats: Vec<String> = string_list(args, "patterns", '|')
                .into_iter()
                .map(|p| p.to_ascii_lowercase())
                .collect();
            if pats.is_empty() {
                pats = vec![
                    "unavailable by icmp ping".into(),
                    "is not available".into(),
                ];
            }
            let mut out = client
                .hosts_unavailable(gids.as_deref(), &pats)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            out.truncate(limit);
            pretty_json(&out)
        }
        "hosts" => {
            let group_names = string_list(args, "groups", ',');
            let gids = client
                .group_ids(&group_names)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let mut params = json!({
                "output": ["hostid", "host", "name", "status", "maintenance_status"],
                "selectInterfaces": ["ip", "dns", "port", "type", "main", "available", "error"],
                "selectGroups": ["groupid", "name"],
                "limit": limit
            });
            if let Some(g) = gids {
                params["groupids"] = json!(g);
            }
            let hids = string_list(args, "hostids", ',');
            if !hids.is_empty() {
                params["hostids"] = json!(hids);
            }
            if let Some(h) = opt_str(args, "host") {
                params["search"] = json!({"host": h, "name": h});
                params["searchByAny"] = json!(true);
            }
            let res = client
                .call("host.get", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "items" => {
            let mut params = json!({
                "output": ["itemid", "hostid", "name", "key_", "value_type", "lastvalue", "units", "lastclock", "state", "status"],
                "selectHosts": ["hostid", "host"],
                "limit": limit
            });
            if let Some(hids) = resolve_hostids(client, args)? {
                if hids.is_empty() {
                    return Ok("[]".into());
                }
                params["hostids"] = json!(hids);
            }
            let pats = string_list(args, "patterns", '|');
            if let Some(first) = pats.first() {
                params["search"] = json!({"name": first, "key_": first});
                params["searchByAny"] = json!(true);
            }
            let res = client
                .call("item.get", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "history" => {
            let itemids = string_list(args, "itemids", ',');
            if itemids.is_empty() {
                return Err(ToolError::BadArguments(
                    "history exige `itemids` (use `action: \"items\"` antes para achar os itemids)"
                        .into(),
                ));
            }
            let history_type = match args.get("history_type").and_then(Value::as_u64) {
                Some(t) if t <= 4 => t,
                _ => {
                    // Auto-detect item value_type (0=float, 1=str, 2=log, 3=uint, 4=text)
                    let items = client
                        .call_list(
                            "item.get",
                            json!({"itemids": [&itemids[0]], "output": ["itemid", "value_type"]}),
                        )
                        .map_err(|e| ToolError::Failed(e.to_string()))?;
                    items
                        .first()
                        .and_then(|it| it.get("value_type"))
                        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
                        .filter(|t| *t <= 4)
                        .unwrap_or(3)
                }
            };
            let hist_limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 500);
            let mut params = json!({
                "output": "extend",
                "history": history_type,
                "itemids": itemids,
                "sortfield": "clock",
                "sortorder": "DESC",
                "limit": hist_limit
            });
            if let Some(t) = args.get("time_from").and_then(Value::as_i64) {
                params["time_from"] = json!(t);
            }
            if let Some(t) = args.get("time_till").and_then(Value::as_i64) {
                params["time_till"] = json!(t);
            }
            let res = client
                .call("history.get", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "ack" => {
            let eventids: Vec<String> = string_list(args, "eventids", ',')
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if eventids.is_empty() {
                return Err(ToolError::BadArguments(
                    "ack exige `eventids` (lista ou string de IDs de evento)".into(),
                ));
            }
            let ack_flag = args
                .get("acknowledge")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let close_flag = args
                .get("close")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let message = opt_str(args, "message");
            let new_severity = args
                .get("severity")
                .and_then(Value::as_u64)
                .filter(|s| *s <= 5);

            // Zabbix `event.acknowledge` action bitmask:
            // 1 = close, 2 = acknowledge, 4 = add message, 8 = change severity.
            let mut bitmask = 0u64;
            if close_flag {
                bitmask |= 1;
            }
            if ack_flag {
                bitmask |= 2;
            }
            if message.is_some() {
                bitmask |= 4;
            }
            if new_severity.is_some() {
                bitmask |= 8;
            }
            if bitmask == 0 {
                bitmask = 2;
            }

            let mut params = json!({
                "eventids": eventids,
                "action": bitmask
            });
            if let Some(msg) = message {
                params["message"] = json!(msg);
            }
            if let Some(sev) = new_severity {
                params["severity"] = json!(sev);
            }
            let res = client
                .call("event.acknowledge", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "raw" => {
            let method = opt_str(args, "method").ok_or_else(|| {
                ToolError::BadArguments("action=raw exige `method` (ex.: host.get)".into())
            })?;
            let params = match args.get("params") {
                Some(Value::String(s)) if !s.trim().is_empty() => serde_json::from_str(s)
                    .map_err(|e| {
                        ToolError::BadArguments(format!("params JSON inválido: {e}"))
                    })?,
                Some(Value::Null) | None => json!({}),
                Some(other) => other.clone(),
            };
            let res = client
                .call(method, params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        other => Err(ToolError::BadArguments(format!(
            "ação Zabbix desconhecida: '{other}' (use problems, hosts_unavailable, hosts, items, history, ack, raw)"
        ))),
    }
}

fn pretty_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string_pretty(v).map_err(|e| ToolError::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use zerith_security::{preset, Policy};

    fn fake_zabbix(
        answer: impl Fn(&str, &Value) -> Value + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Value>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/zabbix", l.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut rd = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                rd.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = body["method"].as_str().unwrap_or("").to_string();
                let resp = answer(&method, &body).to_string();
                log.lock().unwrap().push(body);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            }
        });
        (url, seen)
    }

    #[test]
    fn plaintext_token_is_refused_and_secret_lookup_works() {
        let err = resolve_url_and_token(
            &json!({"url": "https://zbx/zabbix", "token": "plain-123", "token_secret": "zbx"}),
            |_| Ok("ok".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("texto puro"), "{err}");

        let (url, tok) = resolve_url_and_token(
            &json!({"url_secret": "zbx_url", "token_secret": "zbx_tok"}),
            |k| match k {
                "zbx_url" => Ok("https://zbx.local/zabbix".into()),
                "zbx_tok" => Ok("secret-tok".into()),
                _ => Err(ToolError::Failed("missing".into())),
            },
        )
        .unwrap();
        assert_eq!(url, "https://zbx.local/zabbix");
        assert_eq!(tok, "secret-tok");
    }

    #[test]
    fn concurrency_and_policy_classify_read_vs_ack_and_raw() {
        let tool = NetZabbixTool::new();
        assert!(tool.concurrency_safe(&json!({"action": "problems"})));
        assert!(tool.concurrency_safe(&json!({"action": "hosts_unavailable"})));
        assert!(tool.concurrency_safe(&json!({"action": "hosts"})));
        assert!(tool.concurrency_safe(&json!({"action": "items"})));
        assert!(tool.concurrency_safe(&json!({"action": "history"})));
        assert!(tool.concurrency_safe(&json!({"action": "raw", "method": "host.get"})));
        assert!(!tool.concurrency_safe(&json!({"action": "ack"})));
        assert!(!tool.concurrency_safe(&json!({"action": "raw", "method": "maintenance.create"})));

        let ws = std::path::Path::new(".");
        let plan_pol = Policy::new(preset("workspace-write").unwrap(), Mode::Plan, ws);
        let plan_ctx = CallCtx {
            workspace: ws,
            policy: &plan_pol,
            memory: None,
            scope: None,
        };
        assert!(tool
            .action(
                &json!({"action": "problems", "url": "https://z"}),
                &plan_ctx
            )
            .is_ok());
        assert!(matches!(
            tool.action(&json!({"action": "ack", "url": "https://z"}), &plan_ctx),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            tool.action(
                &json!({"action": "raw", "method": "host.delete", "url": "https://z"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
    }

    #[test]
    fn ack_history_items_and_hosts_round_trip() {
        let (url, seen) = fake_zabbix(|method, body| {
            let result = match method {
                "host.get" => json!([{"hostid": "101", "host": "olt-01"}]),
                "item.get" => {
                    json!([{"itemid": "5001", "hostid": "101", "value_type": "0", "lastvalue": "-21.4"}])
                }
                "history.get" => {
                    json!([{"itemid": "5001", "clock": "1700000000", "value": "-21.4"}])
                }
                "event.acknowledge" => json!({"eventids": body["params"]["eventids"]}),
                _ => json!([]),
            };
            json!({"jsonrpc": "2.0", "result": result, "id": 1})
        });
        let client = Client::new(
            &url,
            "tok-secret".into(),
            AuthMode::Header,
            Duration::from_secs(5),
            false,
        )
        .unwrap();

        let hosts =
            execute_with_client(&client, &json!({"action": "hosts", "host": "olt"})).unwrap();
        assert!(hosts.contains("olt-01"), "{hosts}");

        let hist = execute_with_client(&client, &json!({"action": "history", "itemids": ["5001"]}))
            .unwrap();
        assert!(hist.contains("-21.4"), "{hist}");

        let ack = execute_with_client(
            &client,
            &json!({
                "action": "ack",
                "eventids": ["9001"],
                "message": "Fibra rompida em verificação",
                "close": true,
                "severity": 4
            }),
        )
        .unwrap();
        assert!(ack.contains("9001"), "{ack}");

        let reqs = seen.lock().unwrap();
        let ack_req = reqs
            .iter()
            .find(|r| r["method"] == "event.acknowledge")
            .unwrap();
        // 1 (close) + 2 (ack) + 4 (message) + 8 (severity) = 15
        assert_eq!(ack_req["params"]["action"], 15);
        assert_eq!(ack_req["params"]["severity"], 4);
        assert_eq!(ack_req["params"]["message"], "Fibra rompida em verificação");
        let hist_req = reqs.iter().find(|r| r["method"] == "history.get").unwrap();
        assert_eq!(hist_req["params"]["history"], 0);
    }
}
