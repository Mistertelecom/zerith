//! Ferramenta `net_zabbix`: consulta e operação na API JSON-RPC do Zabbix (7.0 LTS+)
//! e gerador determinístico de templates Zabbix 7.0 LTS para ISPs/NOC.
//!
//! Reutiliza `zerith_workflows::zabbix::Client` (com suporte a `insecure_tls` e
//! `auth_mode: header | auth_field`). Tokens em texto puro são recusados: apenas
//! segredos do cofre (`token_secret`, e opcionalmente `url_secret`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use zerith_security::{Action, Mode, Sandbox};
use zerith_workflows::zabbix::{severity_name, AuthMode, Client};

use crate::fntool::opt_str;
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

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
                description: "Consulta e opera o Zabbix via JSON-RPC (7.0 LTS+) e gera templates \
                    ISP/NOC: `problems` (incidentes ativos), `hosts_unavailable` (ICMP/agente/SNMP \
                    fora do ar), `hosts`, `items`, `history`, `trends` (séries horárias 7d/30d/90d), \
                    `triggers`, `templates`, `host_summary` (triagem composta em 1 chamada), \
                    `sla_report` (relatório executivo NOC), `maintenance` (list/create/delete), \
                    `ack` (reconhece/comenta/fecha evento), `raw` (qualquer método JSON-RPC) e \
                    `generate_template` (gerador offline Zabbix 7.0 YAML/JSON com LLD e histerese). \
                    Exige `token_secret` no cofre para chamadas de API; texto puro é recusado."
                    .into(),
                parameters: {
                    let mut props = serde_json::Map::new();
                    let base = json!({
                        "action": {
                            "type": "string",
                            "enum": [
                                "problems",
                                "hosts_unavailable",
                                "hosts",
                                "items",
                                "history",
                                "ack",
                                "raw",
                                "triggers",
                                "trends",
                                "templates",
                                "maintenance",
                                "host_summary",
                                "sla_report",
                                "generate_template"
                            ],
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
                            "description": "nome do segredo no cofre com o token da API do Zabbix (obrigatório em chamadas de API; dispensado em generate_template offline)"
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
                        "groupids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de groupids (ex.: maintenance create)"
                        },
                        "host": {
                            "type": "string",
                            "description": "filtro por nome do host (ou alvo em host_summary)"
                        },
                        "hostids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de hostids"
                        },
                        "itemids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de itemids (obrigatório em history e trends)"
                        },
                        "eventids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de eventids (obrigatório em ack)"
                        },
                        "templateids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "templates: lista de templateids"
                        },
                        "maintenanceids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "maintenance: lista de maintenanceids (list/delete)"
                        },
                        "severities": {
                            "type": "array",
                            "items": {"type": "integer"},
                            "description": "lista de severidades 0..5"
                        },
                        "min_severity": {
                            "type": "integer",
                            "description": "severidade mínima 1..5"
                        }
                    });
                    let noc = json!({
                        "only_true": {
                            "type": "boolean",
                            "description": "triggers: quando true, lista apenas triggers em estado PROBLEM (value=1)"
                        },
                        "patterns": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "padrões de busca para hosts_unavailable, items, triggers, templates ou maintenance"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "limite máximo de registros"
                        },
                        "time_from": {
                            "description": "history/trends: timestamp UNIX ou janela relativa (\"24h\", \"7d\", \"30d\", \"90d\")"
                        },
                        "time_till": {
                            "description": "history/trends: timestamp UNIX ou relativo (\"now\", \"1h\")"
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
                        "op": {
                            "type": "string",
                            "enum": ["list", "create", "delete"],
                            "description": "maintenance: operação desejada (padrão list)"
                        },
                        "name": {
                            "type": "string",
                            "description": "maintenance create: nome da janela de manutenção"
                        },
                        "description": {
                            "type": "string",
                            "description": "maintenance create: descrição da mudança/manutenção"
                        },
                        "active_since": {
                            "description": "maintenance create: início (UNIX ou relativo; padrão agora)"
                        },
                        "active_till": {
                            "description": "maintenance create: fim (UNIX ou relativo)"
                        },
                        "duration_mins": {
                            "type": "integer",
                            "description": "maintenance create: duração em minutos (padrão 60 se active_till omitido)"
                        },
                        "no_data": {
                            "type": "boolean",
                            "description": "maintenance create: sem coleta de dados (maintenance_type=1; padrão false)"
                        },
                        "method": {
                            "type": "string",
                            "description": "raw: método JSON-RPC (ex.: host.get, maintenance.create)"
                        },
                        "params": {
                            "type": "object",
                            "description": "raw: objeto de parâmetros JSON-RPC"
                        },
                        "preset": {
                            "type": "string",
                            "enum": ["cisco_asr_1001x", "datacom_dmos", "ubiquiti_rf", "gpon_olt"],
                            "description": "generate_template: preset ISP/NOC para geração de template Zabbix 7.0 LTS"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["yaml", "json"],
                            "description": "generate_template: formato de saída (yaml padrão ou json)"
                        },
                        "template_name": {
                            "type": "string",
                            "description": "generate_template: nome customizado do template"
                        },
                        "template_group": {
                            "type": "string",
                            "description": "generate_template: grupo de template (padrão Templates/ISP)"
                        },
                        "output_path": {
                            "type": "string",
                            "description": "generate_template: caminho relativo opcional no workspace para salvar o template"
                        }
                    });
                    if let Value::Object(m) = base {
                        props.extend(m);
                    }
                    if let Value::Object(m) = noc {
                        props.extend(m);
                    }
                    json!({
                        "type": "object",
                        "properties": Value::Object(props)
                    })
                },
                returns: "JSON formatado com problemas, hosts, itens, histórico, trends, triagem NOC, relatório SLA, manutenção, confirmação do ack ou template Zabbix 7.0",
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

fn maintenance_op(args: &Value) -> &str {
    opt_str(args, "op").unwrap_or("list")
}

fn is_readonly_call(args: &Value) -> bool {
    match zabbix_action(args) {
        "problems" | "hosts_unavailable" | "hosts" | "items" | "history" | "triggers"
        | "trends" | "templates" | "host_summary" | "sla_report" | "generate_template" => true,
        "maintenance" => maintenance_op(args).eq_ignore_ascii_case("list"),
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
        if act == "generate_template" {
            if let Some(out_path) = opt_str(args, "output_path") {
                if ctx.policy.preset.sandbox == Sandbox::ReadOnly || ctx.policy.mode() == Mode::Plan
                {
                    return Err(ToolError::Denied(
                        "gravação de template em disco (`output_path`) bloqueada no modo plan/read-only".into(),
                    ));
                }
                return Ok(Some(Action::Write(ctx.resolve(out_path))));
            }
            return Ok(None);
        }

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
        } else if act == "maintenance" {
            let op = maintenance_op(args);
            format!("zabbix maintenance:{op} {target}")
        } else {
            format!("zabbix {act} {target}")
        };
        Ok(Some(Action::Network(label)))
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        if zabbix_action(args) == "generate_template" {
            return execute_generate_template(args, ctx, exec);
        }

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

fn val_str(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_relative_duration_secs(spec: &str) -> Option<i64> {
    let trimmed = spec.trim().to_ascii_lowercase();
    let trimmed = trimmed.strip_prefix("now-").unwrap_or(&trimmed);
    if trimmed.len() < 2 {
        return None;
    }
    let (num_part, unit) = trimmed.split_at(trimmed.len() - 1);
    let qty: i64 = num_part.trim().parse().ok()?;
    if qty < 0 {
        return None;
    }
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    qty.checked_mul(mult)
}

fn parse_time_bound(
    val: Option<&Value>,
    now_secs: i64,
    future_relative: bool,
) -> Result<Option<i64>> {
    let Some(v) = val else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    if let Some(n) = v.as_i64() {
        return Ok(Some(n));
    }
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.eq_ignore_ascii_case("now") {
            return Ok(Some(now_secs));
        }
        if let Ok(ts) = trimmed.parse::<i64>() {
            return Ok(Some(ts));
        }
        if let Some(delta) = parse_relative_duration_secs(trimmed) {
            let ts = if future_relative {
                now_secs + delta
            } else {
                now_secs - delta
            };
            return Ok(Some(ts));
        }
        return Err(ToolError::BadArguments(format!(
            "tempo inválido '{trimmed}': use timestamp UNIX ou janela relativa (ex.: '24h', '7d', '30d', 'now')"
        )));
    }
    Err(ToolError::BadArguments(
        "tempo inválido: informe timestamp UNIX inteiro ou string relativa ('24h', '7d', '30d')"
            .into(),
    ))
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
        "problems" => execute_problems(client, args, limit),
        "hosts_unavailable" => execute_hosts_unavailable(client, args, limit),
        "hosts" => execute_hosts(client, args, limit),
        "items" => execute_items(client, args, limit),
        "history" => execute_history(client, args),
        "ack" => execute_ack(client, args),
        "raw" => execute_raw(client, args),
        "triggers" => execute_triggers(client, args, limit),
        "trends" => execute_trends(client, args),
        "templates" => execute_templates(client, args, limit),
        "maintenance" => execute_maintenance(client, args, limit),
        "host_summary" => execute_host_summary(client, args, limit),
        "sla_report" => execute_sla_report(client, args),
        "generate_template" => {
            let content = render_template_for_args(args)?;
            Ok(content)
        }
        other => Err(ToolError::BadArguments(format!(
            "ação Zabbix desconhecida: '{other}' (use problems, hosts_unavailable, hosts, items, \
             history, triggers, trends, templates, maintenance, host_summary, sla_report, ack, raw ou generate_template)"
        ))),
    }
}

fn execute_problems(client: &Client, args: &Value, limit: usize) -> Result<String> {
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

fn execute_hosts_unavailable(client: &Client, args: &Value, limit: usize) -> Result<String> {
    let group_names = string_list(args, "groups", ',');
    let gids = client
        .group_ids(&group_names)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let mut pats: Vec<String> = string_list(args, "patterns", '|')
        .into_iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();
    if pats.is_empty() {
        pats = vec!["unavailable by icmp ping".into(), "is not available".into()];
    }
    let mut out = client
        .hosts_unavailable(gids.as_deref(), &pats)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    out.truncate(limit);
    pretty_json(&out)
}

fn execute_hosts(client: &Client, args: &Value, limit: usize) -> Result<String> {
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

fn execute_items(client: &Client, args: &Value, limit: usize) -> Result<String> {
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

fn execute_history(client: &Client, args: &Value) -> Result<String> {
    let itemids = string_list(args, "itemids", ',');
    if itemids.is_empty() {
        return Err(ToolError::BadArguments(
            "history exige `itemids` (use `action: \"items\"` antes para achar os itemids)".into(),
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
    let now = now_unix_secs();
    if let Some(t) = parse_time_bound(args.get("time_from"), now, false)? {
        params["time_from"] = json!(t);
    }
    if let Some(t) = parse_time_bound(args.get("time_till"), now, false)? {
        params["time_till"] = json!(t);
    }
    let res = client
        .call("history.get", params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    pretty_json(&res)
}

fn execute_ack(client: &Client, args: &Value) -> Result<String> {
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
    let close_flag = args.get("close").and_then(Value::as_bool).unwrap_or(false);
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

fn execute_raw(client: &Client, args: &Value) -> Result<String> {
    let method = opt_str(args, "method").ok_or_else(|| {
        ToolError::BadArguments("action=raw exige `method` (ex.: host.get)".into())
    })?;
    let params = match args.get("params") {
        Some(Value::String(s)) if !s.trim().is_empty() => serde_json::from_str(s)
            .map_err(|e| ToolError::BadArguments(format!("params JSON inválido: {e}")))?,
        Some(Value::Null) | None => json!({}),
        Some(other) => other.clone(),
    };
    let res = client
        .call(method, params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    pretty_json(&res)
}

fn execute_triggers(client: &Client, args: &Value, limit: usize) -> Result<String> {
    let group_names = string_list(args, "groups", ',');
    let gids = client
        .group_ids(&group_names)
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let mut params = json!({
        "output": [
            "triggerid",
            "description",
            "expression",
            "priority",
            "value",
            "status",
            "lastchange",
            "comments",
            "error",
            "state"
        ],
        "selectHosts": ["hostid", "host", "name"],
        "selectLastEvent": ["eventid", "clock", "acknowledged", "ns"],
        "expandDescription": true,
        "sortfield": "priority",
        "sortorder": "DESC",
        "limit": limit
    });

    if let Some(g) = gids {
        params["groupids"] = json!(g);
    }
    if let Some(hids) = resolve_hostids(client, args)? {
        if hids.is_empty() {
            return Ok("[]".into());
        }
        params["hostids"] = json!(hids);
    }
    if let Some(min_sev) = args
        .get("min_severity")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|n| *n <= 5)
    {
        params["min_severity"] = json!(min_sev);
    }
    let mut filter = Map::new();
    if args.get("only_true").and_then(Value::as_bool) == Some(true) {
        filter.insert("value".into(), json!(1));
    }
    if let Some(arr) = args.get("severities").and_then(Value::as_array) {
        let sevs: Vec<u8> = arr
            .iter()
            .filter_map(|x| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()))
            .filter(|n| *n <= 5)
            .map(|n| n as u8)
            .collect();
        if !sevs.is_empty() {
            filter.insert("priority".into(), json!(sevs));
        }
    }
    if !filter.is_empty() {
        params["filter"] = Value::Object(filter);
    }
    let pats = string_list(args, "patterns", '|');
    if let Some(first) = pats.first() {
        params["search"] = json!({"description": first});
    }

    let res = client
        .call("trigger.get", params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    pretty_json(&res)
}

fn execute_trends(client: &Client, args: &Value) -> Result<String> {
    let itemids = string_list(args, "itemids", ',');
    if itemids.is_empty() {
        return Err(ToolError::BadArguments(
            "trends exige `itemids` (use `action: \"items\"` antes para achar os itemids)".into(),
        ));
    }
    let trend_limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(168)
        .clamp(1, 2000);
    let now = now_unix_secs();
    let mut params = json!({
        "output": ["itemid", "clock", "num", "value_min", "value_avg", "value_max"],
        "itemids": itemids,
        "limit": trend_limit
    });
    if let Some(t) = parse_time_bound(args.get("time_from"), now, false)? {
        params["time_from"] = json!(t);
    }
    if let Some(t) = parse_time_bound(args.get("time_till"), now, false)? {
        params["time_till"] = json!(t);
    }
    let res = client
        .call("trend.get", params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    pretty_json(&res)
}

fn execute_templates(client: &Client, args: &Value, limit: usize) -> Result<String> {
    let mut params = json!({
        "output": ["templateid", "host", "name", "description"],
        "selectItems": "count",
        "selectTriggers": "count",
        "limit": limit
    });
    let tids = string_list(args, "templateids", ',');
    if !tids.is_empty() {
        params["templateids"] = json!(tids);
    }
    let pats = string_list(args, "patterns", '|');
    if let Some(first) = pats.first() {
        params["search"] = json!({"host": first, "name": first});
        params["searchByAny"] = json!(true);
    } else if let Some(h) = opt_str(args, "host") {
        params["search"] = json!({"host": h, "name": h});
        params["searchByAny"] = json!(true);
    }
    let res = client
        .call("template.get", params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    pretty_json(&res)
}

fn execute_maintenance(client: &Client, args: &Value, limit: usize) -> Result<String> {
    let op = maintenance_op(args).to_ascii_lowercase();
    match op.as_str() {
        "list" => {
            let group_names = string_list(args, "groups", ',');
            let gids = client
                .group_ids(&group_names)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let mut params = json!({
                "output": [
                    "maintenanceid",
                    "name",
                    "active_since",
                    "active_till",
                    "maintenance_type",
                    "description"
                ],
                "selectHosts": ["hostid", "host"],
                "selectHostGroups": ["groupid", "name"],
                "limit": limit
            });
            let mids = string_list(args, "maintenanceids", ',');
            if !mids.is_empty() {
                params["maintenanceids"] = json!(mids);
            }
            if let Some(g) = gids {
                params["groupids"] = json!(g);
            }
            if let Some(hids) = resolve_hostids(client, args)? {
                if !hids.is_empty() {
                    params["hostids"] = json!(hids);
                }
            }
            let pats = string_list(args, "patterns", '|');
            if let Some(first) = pats.first() {
                params["search"] = json!({"name": first});
            }
            let res = client
                .call("maintenance.get", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "create" => {
            let name = opt_str(args, "name").ok_or_else(|| {
                ToolError::BadArguments(
                    "maintenance op=\"create\" exige `name` (nome da janela)".into(),
                )
            })?;
            let mut hostids = resolve_hostids(client, args)?.unwrap_or_default();
            hostids.sort();
            hostids.dedup();

            let mut groupids = string_list(args, "groupids", ',');
            let group_names = string_list(args, "groups", ',');
            if let Some(resolved_gids) = client
                .group_ids(&group_names)
                .map_err(|e| ToolError::Failed(e.to_string()))?
            {
                groupids.extend(resolved_gids);
            }
            groupids.sort();
            groupids.dedup();

            if hostids.is_empty() && groupids.is_empty() {
                return Err(ToolError::BadArguments(
                    "maintenance op=\"create\" exige ao menos `hostids`/`host` ou `groupids`/`groups`".into(),
                ));
            }

            let now = now_unix_secs();
            let active_since =
                parse_time_bound(args.get("active_since"), now, true)?.unwrap_or(now);
            let active_till = match parse_time_bound(args.get("active_till"), active_since, true)? {
                Some(t) => t,
                None => {
                    let mins = args
                        .get("duration_mins")
                        .and_then(Value::as_i64)
                        .unwrap_or(60)
                        .clamp(1, 43_200);
                    active_since + mins * 60
                }
            };
            if active_till <= active_since {
                return Err(ToolError::BadArguments(
                    "`active_till` precisa ser maior que `active_since`".into(),
                ));
            }
            let period = (active_till - active_since).max(60);
            let m_type = if args.get("no_data").and_then(Value::as_bool) == Some(true) {
                1
            } else {
                0
            };
            let description = opt_str(args, "description").unwrap_or("");

            let hosts_obj: Vec<Value> = hostids.iter().map(|id| json!({"hostid": id})).collect();
            let groups_obj: Vec<Value> = groupids.iter().map(|id| json!({"groupid": id})).collect();

            let mut params = json!({
                "name": name,
                "active_since": active_since,
                "active_till": active_till,
                "maintenance_type": m_type,
                "description": description,
                "timeperiods": [
                    {
                        "timeperiod_type": 0,
                        "start_date": active_since,
                        "period": period
                    }
                ]
            });
            if !hosts_obj.is_empty() {
                params["hosts"] = json!(hosts_obj);
            }
            if !groups_obj.is_empty() {
                params["groups"] = json!(groups_obj);
            }

            let res = client
                .call("maintenance.create", params)
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        "delete" => {
            let mids = string_list(args, "maintenanceids", ',');
            if mids.is_empty() {
                return Err(ToolError::BadArguments(
                    "maintenance op=\"delete\" exige `maintenanceids`".into(),
                ));
            }
            let res = client
                .call("maintenance.delete", json!(mids))
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            pretty_json(&res)
        }
        other => Err(ToolError::BadArguments(format!(
            "op inválido em maintenance: '{other}' (use list, create ou delete)"
        ))),
    }
}

fn execute_host_summary(client: &Client, args: &Value, limit: usize) -> Result<String> {
    let explicit_ids = string_list(args, "hostids", ',');
    let host_query = opt_str(args, "host");
    if explicit_ids.is_empty() && host_query.is_none() {
        return Err(ToolError::BadArguments(
            "host_summary exige `host` ou `hostids`".into(),
        ));
    }

    let mut params = json!({
        "output": ["hostid", "host", "name", "status", "maintenance_status", "maintenance_from"],
        "selectInterfaces": ["interfaceid", "ip", "dns", "port", "type", "main", "available", "error", "errors_from"],
        "selectParentTemplates": ["templateid", "name"],
        "selectGroups": ["groupid", "name"],
        "limit": 10
    });
    if !explicit_ids.is_empty() {
        params["hostids"] = json!(explicit_ids);
    } else if let Some(q) = host_query {
        params["search"] = json!({"host": q, "name": q});
        params["searchByAny"] = json!(true);
    }

    let hosts = client
        .call_list("host.get", params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let selected = pick_best_host(&hosts, host_query).ok_or_else(|| {
        let label = host_query.unwrap_or("hostid");
        ToolError::Failed(format!("host '{label}' não encontrado no Zabbix"))
    })?;

    let hostid = val_str(selected, "hostid");
    let problems = client
        .call_list(
            "problem.get",
            json!({
                "output": ["eventid", "objectid", "name", "severity", "clock", "acknowledged"],
                "hostids": [&hostid],
                "recent": false,
                "suppressed": false,
                "sortfield": ["eventid"],
                "sortorder": "DESC",
                "limit": 25
            }),
        )
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let items = client
        .call_list(
            "item.get",
            json!({
                "output": ["itemid", "name", "key_", "value_type", "lastvalue", "units", "lastclock", "state", "status", "error"],
                "hostids": [&hostid],
                "filter": {"status": 0},
                "limit": 100
            }),
        )
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let ifaces = selected
        .get("interfaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ifaces_up = ifaces
        .iter()
        .filter(|i| val_str(i, "available") == "1")
        .count();
    let ifaces_down = ifaces
        .iter()
        .filter(|i| val_str(i, "available") == "2")
        .count();

    let mut error_items = Vec::new();
    let mut key_items = Vec::new();
    let mut fallback_items = Vec::new();

    for it in &items {
        let state = val_str(it, "state");
        let err = val_str(it, "error");
        if state == "1" || !err.is_empty() {
            error_items.push(it.clone());
        }
        let key_low = val_str(it, "key_").to_ascii_lowercase();
        let name_low = val_str(it, "name").to_ascii_lowercase();
        if is_noc_key_item(&key_low, &name_low) {
            key_items.push(it.clone());
        } else if fallback_items.len() < 10 {
            fallback_items.push(it.clone());
        }
    }
    let max_items = limit.min(30);
    if key_items.is_empty() {
        key_items = fallback_items;
    }
    key_items.truncate(max_items);
    error_items.truncate(max_items);

    let active_problems: Vec<Value> = problems
        .iter()
        .map(|p| {
            let sev_code = val_str(p, "severity");
            json!({
                "eventid": val_str(p, "eventid"),
                "objectid": val_str(p, "objectid"),
                "name": val_str(p, "name"),
                "severity_code": sev_code,
                "severity": severity_name(&sev_code),
                "clock": val_str(p, "clock"),
                "acknowledged": val_str(p, "acknowledged") == "1"
            })
        })
        .collect();

    let summary = json!({
        "host": {
            "hostid": hostid,
            "host": val_str(selected, "host"),
            "name": val_str(selected, "name"),
            "status": if val_str(selected, "status") == "0" { "enabled" } else { "disabled" },
            "in_maintenance": val_str(selected, "maintenance_status") == "1"
        },
        "availability": {
            "interfaces_total": ifaces.len(),
            "interfaces_up": ifaces_up,
            "interfaces_down": ifaces_down,
            "interfaces": ifaces
        },
        "groups": selected.get("groups").cloned().unwrap_or_else(|| json!([])),
        "templates": selected.get("parentTemplates").cloned().unwrap_or_else(|| json!([])),
        "active_problems_count": active_problems.len(),
        "active_problems": active_problems,
        "error_items_count": error_items.len(),
        "error_items": error_items,
        "key_items": key_items
    });
    pretty_json(&summary)
}

fn pick_best_host<'a>(hosts: &'a [Value], query: Option<&str>) -> Option<&'a Value> {
    if let Some(q) = query {
        let q_low = q.trim().to_ascii_lowercase();
        if let Some(exact) = hosts.iter().find(|h| {
            val_str(h, "host").to_ascii_lowercase() == q_low
                || val_str(h, "name").to_ascii_lowercase() == q_low
        }) {
            return Some(exact);
        }
    }
    hosts.first()
}

fn is_noc_key_item(key_low: &str, name_low: &str) -> bool {
    const TOKENS: &[&str] = &[
        "icmpping",
        "system.cpu",
        "vm.memory",
        "net.if",
        "ifhcin",
        "ifhcout",
        "bgp",
        "ospf",
        "temp",
        "optical",
        "dom",
        "rxpower",
        "txpower",
        "pon",
        "onu",
        "loss",
        "uptime",
        "rssi",
        "snr",
    ];
    TOKENS
        .iter()
        .any(|t| key_low.contains(t) || name_low.contains(t))
}

fn execute_sla_report(client: &Client, args: &Value) -> Result<String> {
    let group_names = string_list(args, "groups", ',');
    let gids = client
        .group_ids(&group_names)
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let mut host_params = json!({
        "output": ["hostid", "host", "name", "status", "maintenance_status"],
        "selectInterfaces": ["ip", "type", "main", "available", "error"],
        "monitored_hosts": true
    });
    if let Some(ref g) = gids {
        host_params["groupids"] = json!(g);
    }
    let hosts = client
        .call_list("host.get", host_params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let mut hosts_up = 0usize;
    let mut hosts_down = 0usize;
    let mut hosts_unknown = 0usize;
    let mut hosts_in_maintenance = 0usize;

    for h in &hosts {
        if val_str(h, "maintenance_status") == "1" {
            hosts_in_maintenance += 1;
        }
        let ifaces = h
            .get("interfaces")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let has_down = ifaces.iter().any(|i| val_str(i, "available") == "2");
        let has_up = ifaces.iter().any(|i| val_str(i, "available") == "1");
        if has_down {
            hosts_down += 1;
        } else if has_up {
            hosts_up += 1;
        } else {
            hosts_unknown += 1;
        }
    }

    let known_total = hosts_up + hosts_down;
    let availability_known_pct = if known_total > 0 {
        ((hosts_up as f64 / known_total as f64) * 10000.0).round() / 100.0
    } else {
        100.0
    };
    let availability_total_pct = if !hosts.is_empty() {
        (((hosts.len() - hosts_down) as f64 / hosts.len() as f64) * 10000.0).round() / 100.0
    } else {
        100.0
    };

    let mut prob_params = json!({
        "output": ["eventid", "objectid", "name", "severity", "clock"],
        "recent": false,
        "suppressed": false,
        "sortfield": ["eventid"],
        "sortorder": "DESC",
        "limit": 500
    });
    if let Some(ref g) = gids {
        prob_params["groupids"] = json!(g);
    }
    if let Some(sevs) = parse_severities(args) {
        prob_params["severities"] = json!(sevs);
    }
    let problems = client
        .call_list("problem.get", prob_params)
        .map_err(|e| ToolError::Failed(e.to_string()))?;

    let trig_ids: Vec<String> = problems
        .iter()
        .map(|p| val_str(p, "objectid"))
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut trigger_host_map: HashMap<String, String> = HashMap::new();
    if !trig_ids.is_empty() {
        let trigs = client
            .call_list(
                "trigger.get",
                json!({
                    "triggerids": trig_ids,
                    "output": ["triggerid"],
                    "selectHosts": ["hostid", "host", "name"]
                }),
            )
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        for t in trigs {
            let tid = val_str(&t, "triggerid");
            if let Some(h) = t
                .get("hosts")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
            {
                let name = val_str(h, "host");
                if !tid.is_empty() && !name.is_empty() {
                    trigger_host_map.insert(tid, name);
                }
            }
        }
    }

    let mut sev_counts = [0usize; 6];
    let mut host_offenders: BTreeMap<String, (usize, u8)> = BTreeMap::new();
    let mut trigger_offenders: BTreeMap<String, (usize, u8)> = BTreeMap::new();

    for p in &problems {
        let sev_u8: u8 = val_str(p, "severity").parse::<u8>().unwrap_or(0).min(5);
        sev_counts[sev_u8 as usize] += 1;

        let oid = val_str(p, "objectid");
        let host_name = trigger_host_map
            .get(&oid)
            .cloned()
            .unwrap_or_else(|| "desconhecido".into());
        let h_entry = host_offenders.entry(host_name).or_insert((0, 0));
        h_entry.0 += 1;
        h_entry.1 = h_entry.1.max(sev_u8);

        let prob_name = val_str(p, "name");
        if !prob_name.is_empty() {
            let t_entry = trigger_offenders.entry(prob_name).or_insert((0, 0));
            t_entry.0 += 1;
            t_entry.1 = t_entry.1.max(sev_u8);
        }
    }

    let mut top_hosts: Vec<(String, usize, u8)> = host_offenders
        .into_iter()
        .map(|(h, (c, s))| (h, c, s))
        .collect();
    top_hosts.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    top_hosts.truncate(10);

    let mut top_triggers: Vec<(String, usize, u8)> = trigger_offenders
        .into_iter()
        .map(|(t, (c, s))| (t, c, s))
        .collect();
    top_triggers.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    top_triggers.truncate(10);

    let report = json!({
        "generated_at_unix": now_unix_secs(),
        "groups_filter": group_names,
        "hosts": {
            "total_monitored": hosts.len(),
            "available_up": hosts_up,
            "unavailable_down": hosts_down,
            "unknown": hosts_unknown,
            "in_maintenance": hosts_in_maintenance,
            "availability_known_pct": availability_known_pct,
            "availability_total_pct": availability_total_pct
        },
        "problems": {
            "total_active": problems.len(),
            "by_severity": {
                "0": sev_counts[0],
                "1": sev_counts[1],
                "2": sev_counts[2],
                "3": sev_counts[3],
                "4": sev_counts[4],
                "5": sev_counts[5]
            },
            "by_severity_named": {
                "not_classified": sev_counts[0],
                "information": sev_counts[1],
                "warning": sev_counts[2],
                "average": sev_counts[3],
                "high": sev_counts[4],
                "disaster": sev_counts[5]
            }
        },
        "top_offending_hosts": top_hosts
            .into_iter()
            .map(|(host, count, max_sev)| json!({
                "host": host,
                "problem_count": count,
                "max_severity_code": max_sev,
                "max_severity": severity_name(&max_sev.to_string())
            }))
            .collect::<Vec<_>>(),
        "top_offending_triggers": top_triggers
            .into_iter()
            .map(|(trigger, count, max_sev)| json!({
                "trigger": trigger,
                "count": count,
                "severity_code": max_sev,
                "severity": severity_name(&max_sev.to_string())
            }))
            .collect::<Vec<_>>()
    });

    pretty_json(&report)
}

// --- Offline Zabbix 7.0 LTS Template Generator -------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplatePreset {
    CiscoAsr1001x,
    DatacomDmos,
    UbiquitiRf,
    GponOlt,
}

impl TemplatePreset {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cisco_asr_1001x" => Ok(Self::CiscoAsr1001x),
            "datacom_dmos" => Ok(Self::DatacomDmos),
            "ubiquiti_rf" => Ok(Self::UbiquitiRf),
            "gpon_olt" => Ok(Self::GponOlt),
            other => Err(ToolError::BadArguments(format!(
                "preset inválido em generate_template: '{other}' (use cisco_asr_1001x, datacom_dmos, ubiquiti_rf ou gpon_olt)"
            ))),
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Self::CiscoAsr1001x => "Template Cisco ASR 1001-X ISP NOC",
            Self::DatacomDmos => "Template Datacom DmOS Metro Ring NOC",
            Self::UbiquitiRf => "Template Ubiquiti airFiber/airMAX RF NOC",
            Self::GponOlt => "Template GPON OLT ISP NOC",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::CiscoAsr1001x => {
                "Template Zabbix 7.0 LTS para roteadores Cisco ASR 1001-X (IOS-XE): ICMP, CPU/QFP, memória, temperatura, LLD IF-MIB 64-bit, DOM óptico e peers BGP com histerese."
            }
            Self::DatacomDmos => {
                "Template Zabbix 7.0 LTS para switches Datacom DmOS (anel metro): ICMP, CPU, memória, saúde de fonte/cooler, LLD IF-MIB 64-bit, DOM SFP+/QSFP28 e status EAPS/ERPS."
            }
            Self::UbiquitiRf => {
                "Template Zabbix 7.0 LTS para enlaces Ubiquiti airFiber/airMAX: RSSI, Noise Floor, SNR, Chain Balance, CCQ/Capacidade RF, latência ICMP e LLD Ethernet 64-bit."
            }
            Self::GponOlt => {
                "Template Zabbix 7.0 LTS para OLT GPON/XGS-PON: CPU, temperatura, LLD de portas PON (potência Tx e ONUs ativas), LLD de ONUs (Rx/Tx dBm, estado operacional) e uplinks 64-bit."
            }
        }
    }
}

fn deterministic_uuid(seed: &str) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for b in seed.as_bytes() {
        h1 ^= u64::from(*b);
        h1 = h1.wrapping_mul(0x0100_0000_01b3);
        h2 = h2.wrapping_mul(0x0100_0000_01b3) ^ u64::from(*b).rotate_left(3);
    }
    // Format as RFC 4122 v4-compatible 32-char lowercase hex (Zabbix 7.0 format).
    let b = ((h1 as u128) << 64) | (h2 as u128);
    let b = (b & !(0xfu128 << 76)) | (0x4u128 << 76);
    let b = (b & !(0x3u128 << 62)) | (0x2u128 << 62);
    format!("{b:032x}")
}

fn execute_generate_template(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let rendered = render_template_for_args(args)?;
    let Some(out_path) = opt_str(args, "output_path") else {
        return Ok(rendered);
    };
    let path = ctx.resolve(out_path);
    exec.host.check_editable(&path)?;
    exec.host.before_write(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&path, rendered.as_bytes())?;
    exec.host.note_read(&path);
    pretty_json(&json!({
        "saved": true,
        "path": path.display().to_string(),
        "bytes": rendered.len(),
        "format": opt_str(args, "format").unwrap_or("yaml").to_ascii_lowercase(),
        "preset": opt_str(args, "preset").unwrap_or("cisco_asr_1001x")
    }))
}

fn render_template_for_args(args: &Value) -> Result<String> {
    let preset_raw = opt_str(args, "preset").ok_or_else(|| {
        ToolError::BadArguments(
            "generate_template exige `preset` (cisco_asr_1001x, datacom_dmos, ubiquiti_rf ou gpon_olt)".into(),
        )
    })?;
    let preset = TemplatePreset::parse(preset_raw)?;
    let fmt = opt_str(args, "format")
        .unwrap_or("yaml")
        .to_ascii_lowercase();
    if !matches!(fmt.as_str(), "yaml" | "json") {
        return Err(ToolError::BadArguments(format!(
            "format inválido em generate_template: '{fmt}' (use yaml ou json)"
        )));
    }
    let tpl_name = opt_str(args, "template_name")
        .unwrap_or_else(|| preset.default_name())
        .to_string();
    let tpl_group = opt_str(args, "template_group")
        .unwrap_or("Templates/ISP")
        .to_string();

    let export = build_zabbix_export(preset, &tpl_name, &tpl_group);
    if fmt == "json" {
        pretty_json(&export)
    } else {
        Ok(json_to_yaml(&export))
    }
}

#[allow(clippy::too_many_arguments)]
fn mk_item(
    tpl: &str,
    name: &str,
    item_type: &str,
    snmp_oid: Option<&str>,
    key: &str,
    delay: &str,
    value_type: &str,
    units: &str,
    valuemap: Option<&str>,
    preprocessing: Vec<Value>,
    triggers: Vec<Value>,
) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "uuid".into(),
        json!(deterministic_uuid(&format!("{tpl}:item:{key}"))),
    );
    obj.insert("name".into(), json!(name));
    obj.insert("type".into(), json!(item_type));
    if let Some(oid) = snmp_oid {
        obj.insert("snmp_oid".into(), json!(oid));
    }
    obj.insert("key".into(), json!(key));
    obj.insert("delay".into(), json!(delay));
    obj.insert("history".into(), json!("7d"));
    obj.insert("trends".into(), json!("90d"));
    obj.insert("value_type".into(), json!(value_type));
    if !units.is_empty() {
        obj.insert("units".into(), json!(units));
    }
    if let Some(vm) = valuemap {
        obj.insert("valuemap".into(), json!({"name": vm}));
    }
    if !preprocessing.is_empty() {
        obj.insert("preprocessing".into(), Value::Array(preprocessing));
    }
    if !triggers.is_empty() {
        obj.insert("triggers".into(), Value::Array(triggers));
    }
    Value::Object(obj)
}

fn mk_trigger(
    seed: &str,
    name: &str,
    expression: &str,
    recovery_expression: &str,
    priority: &str,
    description: &str,
) -> Value {
    json!({
        "uuid": deterministic_uuid(seed),
        "expression": expression,
        "recovery_mode": "RECOVERY_EXPRESSION",
        "recovery_expression": recovery_expression,
        "name": name,
        "priority": priority,
        "description": description,
        "manual_close": "YES"
    })
}

fn pp_discard_heartbeat(seconds: &str) -> Value {
    json!({
        "type": "DISCARD_UNCHANGED_HEARTBEAT",
        "parameters": [seconds]
    })
}

fn pp_change_per_sec() -> Value {
    json!({"type": "CHANGE_PER_SECOND"})
}

fn pp_multiplier(factor: &str) -> Value {
    json!({
        "type": "MULTIPLIER",
        "parameters": [factor]
    })
}

fn base_icmp_items(tpl: &str) -> Vec<Value> {
    vec![
        mk_item(
            tpl,
            "ICMP ping",
            "SIMPLE",
            None,
            "icmpping",
            "30s",
            "UNSIGNED",
            "",
            Some("Service state"),
            vec![pp_discard_heartbeat("10m")],
            vec![mk_trigger(
                &format!("{tpl}:trig:icmpping"),
                "Host inacessível via ICMP Ping",
                &format!("max(/{tpl}/icmpping,#3)=0"),
                &format!("min(/{tpl}/icmpping,#3)=1"),
                "HIGH",
                "Disparo após 3 falhas consecutivas; recuperação exige 3 pings OK consecutivos (histerese anti-flap).",
            )],
        ),
        mk_item(
            tpl,
            "ICMP perda de pacotes",
            "SIMPLE",
            None,
            "icmppingloss",
            "1m",
            "FLOAT",
            "%",
            None,
            vec![],
            vec![mk_trigger(
                &format!("{tpl}:trig:icmploss"),
                "Perda de pacotes ICMP elevada",
                &format!("min(/{tpl}/icmppingloss,5m)>{{$ICMP_LOSS_WARN}}"),
                &format!("max(/{tpl}/icmppingloss,5m)<({{$ICMP_LOSS_WARN}}-5)"),
                "WARNING",
                "Perda acima do limiar por 5m; recupera 5 pontos abaixo do limiar.",
            )],
        ),
        mk_item(
            tpl,
            "ICMP latência média",
            "SIMPLE",
            None,
            "icmppingsec",
            "1m",
            "FLOAT",
            "s",
            None,
            vec![],
            vec![],
        ),
        mk_item(
            tpl,
            "SNMP Uptime (sysUpTime)",
            "SNMP_AGENT",
            Some("1.3.6.1.2.1.1.3.0"),
            "system.net.uptime[sysUpTime.0]",
            "1m",
            "UNSIGNED",
            "uptime",
            None,
            vec![pp_multiplier("0.01")],
            vec![mk_trigger(
                &format!("{tpl}:trig:uptime"),
                "Equipamento reiniciado recentemente (< 10m)",
                &format!("last(/{tpl}/system.net.uptime[sysUpTime.0])<10m"),
                &format!("last(/{tpl}/system.net.uptime[sysUpTime.0])>=15m"),
                "INFO",
                "Detecta reboot inesperado ou queda de energia.",
            )],
        ),
    ]
}

fn if_mib_64bit_lld(tpl: &str) -> Value {
    json!({
        "uuid": deterministic_uuid(&format!("{tpl}:lld:ifmib")),
        "name": "Descoberta de Interfaces IF-MIB 64-bit",
        "type": "SNMP_AGENT",
        "snmp_oid": "discovery[{#SNMPVALUE},1.3.6.1.2.1.2.2.1.2,{#IFALIAS},1.3.6.1.2.1.31.1.1.1.18,{#IFOPERSTATUS},1.3.6.1.2.1.2.2.1.8]",
        "key": "net.if.discovery",
        "delay": "1h",
        "lifetime": "30d",
        "item_prototypes": [
            mk_item(
                tpl,
                "Interface {#SNMPVALUE} ({#IFALIAS}): Tráfego Entrada (bps)",
                "SNMP_AGENT",
                Some("1.3.6.1.2.1.31.1.1.1.6.{#SNMPINDEX}"),
                "net.if.in[ifHCInOctets.{#SNMPINDEX}]",
                "1m",
                "UNSIGNED",
                "bps",
                None,
                vec![pp_change_per_sec(), pp_multiplier("8")],
                vec![],
            ),
            mk_item(
                tpl,
                "Interface {#SNMPVALUE} ({#IFALIAS}): Tráfego Saída (bps)",
                "SNMP_AGENT",
                Some("1.3.6.1.2.1.31.1.1.1.10.{#SNMPINDEX}"),
                "net.if.out[ifHCOutOctets.{#SNMPINDEX}]",
                "1m",
                "UNSIGNED",
                "bps",
                None,
                vec![pp_change_per_sec(), pp_multiplier("8")],
                vec![],
            ),
            mk_item(
                tpl,
                "Interface {#SNMPVALUE} ({#IFALIAS}): Velocidade (Mbps)",
                "SNMP_AGENT",
                Some("1.3.6.1.2.1.31.1.1.1.15.{#SNMPINDEX}"),
                "net.if.speed[ifHighSpeed.{#SNMPINDEX}]",
                "5m",
                "UNSIGNED",
                "Mbps",
                None,
                vec![pp_discard_heartbeat("1h")],
                vec![],
            ),
            mk_item(
                tpl,
                "Interface {#SNMPVALUE} ({#IFALIAS}): Erros de Entrada/s",
                "SNMP_AGENT",
                Some("1.3.6.1.2.1.2.2.1.14.{#SNMPINDEX}"),
                "net.if.in.errors[ifInErrors.{#SNMPINDEX}]",
                "1m",
                "FLOAT",
                "pps",
                None,
                vec![pp_change_per_sec()],
                vec![],
            ),
            mk_item(
                tpl,
                "Interface {#SNMPVALUE} ({#IFALIAS}): Status Operacional",
                "SNMP_AGENT",
                Some("1.3.6.1.2.1.2.2.1.8.{#SNMPINDEX}"),
                "net.if.status[ifOperStatus.{#SNMPINDEX}]",
                "1m",
                "UNSIGNED",
                "",
                Some("Interface oper status"),
                vec![pp_discard_heartbeat("15m")],
                vec![],
            )
        ],
        "trigger_prototypes": [
            mk_trigger(
                &format!("{tpl}:trig_proto:ifdown"),
                "Interface {#SNMPVALUE} ({#IFALIAS}): Link Down",
                &format!("last(/{tpl}/net.if.status[ifOperStatus.{{#SNMPINDEX}}])=2"),
                &format!("min(/{tpl}/net.if.status[ifOperStatus.{{#SNMPINDEX}}],#2)=1"),
                "AVERAGE",
                "Interface caiu; recuperação exige 2 coletas consecutivas UP.",
            ),
            mk_trigger(
                &format!("{tpl}:trig_proto:if_sat"),
                "Interface {#SNMPVALUE} ({#IFALIAS}): Saturação RX > {$IF_UTIL_WARN}%",
                &format!(
                    "avg(/{tpl}/net.if.in[ifHCInOctets.{{#SNMPINDEX}}],5m)>(last(/{tpl}/net.if.speed[ifHighSpeed.{{#SNMPINDEX}}])*1000000*{{$IF_UTIL_WARN}}/100) and last(/{tpl}/net.if.speed[ifHighSpeed.{{#SNMPINDEX}}])>0"
                ),
                &format!(
                    "avg(/{tpl}/net.if.in[ifHCInOctets.{{#SNMPINDEX}}],5m)<(last(/{tpl}/net.if.speed[ifHighSpeed.{{#SNMPINDEX}}])*1000000*({{$IF_UTIL_WARN}}-10)/100)"
                ),
                "WARNING",
                "Banda RX média em 5m acima do limiar; recupera com histerese de 10%.",
            ),
            mk_trigger(
                &format!("{tpl}:trig_proto:if_err"),
                "Interface {#SNMPVALUE} ({#IFALIAS}): Erros CRC/Entrada elevados",
                &format!("min(/{tpl}/net.if.in.errors[ifInErrors.{{#SNMPINDEX}}],5m)>{{$IF_ERR_WARN}}"),
                &format!("max(/{tpl}/net.if.in.errors[ifInErrors.{{#SNMPINDEX}}],5m)=0"),
                "WARNING",
                "Erros físicos sustentados por 5m; recupera apenas após zerar por 5m.",
            )
        ]
    })
}

fn build_zabbix_export(preset: TemplatePreset, tpl_name: &str, tpl_group: &str) -> Value {
    let mut items = base_icmp_items(tpl_name);
    let mut discoveries = vec![if_mib_64bit_lld(tpl_name)];
    let mut macros = vec![
        json!({"macro": "{$SNMP_COMMUNITY}", "value": "public"}),
        json!({"macro": "{$ICMP_LOSS_WARN}", "value": "15"}),
        json!({"macro": "{$IF_UTIL_WARN}", "value": "80"}),
        json!({"macro": "{$IF_ERR_WARN}", "value": "2"}),
        json!({"macro": "{$CPU_HIGH}", "value": "85"}),
        json!({"macro": "{$DOM_RX_WARN}", "value": "-22"}),
        json!({"macro": "{$DOM_RX_CRIT}", "value": "-26"}),
    ];

    let mut valuemaps = vec![
        json!({
            "uuid": deterministic_uuid(&format!("{tpl_name}:vm:svc")),
            "name": "Service state",
            "mappings": [
                {"value": "0", "newvalue": "Down"},
                {"value": "1", "newvalue": "Up"}
            ]
        }),
        json!({
            "uuid": deterministic_uuid(&format!("{tpl_name}:vm:ifoper")),
            "name": "Interface oper status",
            "mappings": [
                {"value": "1", "newvalue": "up"},
                {"value": "2", "newvalue": "down"},
                {"value": "3", "newvalue": "testing"}
            ]
        }),
    ];

    match preset {
        TemplatePreset::CiscoAsr1001x => {
            items.push(mk_item(
                tpl_name,
                "Cisco RP CPU Utilization (5m)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.9.9.109.1.1.1.1.8.1"),
                "cisco.cpu.util[cpmCPUTotal5minRev]",
                "1m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:cpu"),
                    "Cisco ASR CPU RP elevada (> {$CPU_HIGH}%)",
                    &format!(
                        "avg(/{tpl_name}/cisco.cpu.util[cpmCPUTotal5minRev],5m)>{{$CPU_HIGH}}"
                    ),
                    &format!(
                        "avg(/{tpl_name}/cisco.cpu.util[cpmCPUTotal5minRev],5m)<({{$CPU_HIGH}}-10)"
                    ),
                    "HIGH",
                    "CPU do Route Processor acima do limiar por 5m; recupera 10% abaixo.",
                )],
            ));
            items.push(mk_item(
                tpl_name,
                "Cisco QFP/ESP Processing Load",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.9.9.109.1.1.1.1.8.2"),
                "cisco.qfp.util[cpmCPUTotal5minRev.2]",
                "1m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:qfp"),
                    "Cisco ASR QFP Load elevado (> {$CPU_HIGH}%)",
                    &format!("avg(/{tpl_name}/cisco.qfp.util[cpmCPUTotal5minRev.2],5m)>{{$CPU_HIGH}}"),
                    &format!("avg(/{tpl_name}/cisco.qfp.util[cpmCPUTotal5minRev.2],5m)<({{$CPU_HIGH}}-10)"),
                    "DISASTER",
                    "QFP/Forwarding Engine próximo à saturação; risco de QFP drops.",
                )],
            ));
            discoveries.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:lld:bgp")),
                "name": "Descoberta de Peers BGP4-MIB",
                "type": "SNMP_AGENT",
                "snmp_oid": "discovery[{#BGP_PEER},1.3.6.1.2.1.15.3.1.7,{#BGP_AS},1.3.6.1.2.1.15.3.1.9]",
                "key": "cisco.bgp.discovery",
                "delay": "15m",
                "lifetime": "30d",
                "item_prototypes": [
                    mk_item(
                        tpl_name,
                        "BGP Peer {#BGP_PEER} (AS {#BGP_AS}): Estado",
                        "SNMP_AGENT",
                        Some("1.3.6.1.2.1.15.3.1.2.{#SNMPINDEX}"),
                        "bgp.peer.state[{#BGP_PEER}]",
                        "1m",
                        "UNSIGNED",
                        "",
                        Some("BGP peer state"),
                        vec![pp_discard_heartbeat("10m")],
                        vec![],
                    )
                ],
                "trigger_prototypes": [
                    mk_trigger(
                        &format!("{tpl_name}:trig_proto:bgp"),
                        "Sessão BGP {#BGP_PEER} (AS {#BGP_AS}) fora de Established",
                        &format!("last(/{tpl_name}/bgp.peer.state[{{#BGP_PEER}}])<>6"),
                        &format!("min(/{tpl_name}/bgp.peer.state[{{#BGP_PEER}}],#2)=6"),
                        "HIGH",
                        "Estado BGP diferente de Established(6); recuperação exige 2 amostras em Established.",
                    )
                ]
            }));
            discoveries.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:lld:dom")),
                "name": "Descoberta Óptica DOM (CISCO-ENTITY-SENSOR-MIB)",
                "type": "SNMP_AGENT",
                "snmp_oid": "discovery[{#SENSOR_NAME},1.3.6.1.2.1.47.1.1.1.1.7]",
                "key": "cisco.dom.discovery",
                "delay": "1h",
                "lifetime": "30d",
                "item_prototypes": [
                    mk_item(
                        tpl_name,
                        "DOM Óptico {#SENSOR_NAME}: Potência Rx",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.9.9.91.1.1.1.1.4.{#SNMPINDEX}"),
                        "cisco.dom.rx[{#SNMPINDEX}]",
                        "2m",
                        "FLOAT",
                        "dBm",
                        None,
                        vec![pp_multiplier("0.1")],
                        vec![],
                    )
                ],
                "trigger_prototypes": [
                    mk_trigger(
                        &format!("{tpl_name}:trig_proto:dom"),
                        "DOM {#SENSOR_NAME}: Potência óptica Rx baixa (< {$DOM_RX_WARN} dBm)",
                        &format!("avg(/{tpl_name}/cisco.dom.rx[{{#SNMPINDEX}}],5m)<{{$DOM_RX_WARN}}"),
                        &format!("avg(/{tpl_name}/cisco.dom.rx[{{#SNMPINDEX}}],5m)>({{$DOM_RX_WARN}}+2)"),
                        "AVERAGE",
                        "Atenuação preventiva em transceiver SFP+/QSFP; histerese de +2 dB na recuperação.",
                    )
                ]
            }));
            valuemaps.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:vm:bgp")),
                "name": "BGP peer state",
                "mappings": [
                    {"value": "1", "newvalue": "idle"},
                    {"value": "2", "newvalue": "connect"},
                    {"value": "3", "newvalue": "active"},
                    {"value": "4", "newvalue": "opensent"},
                    {"value": "5", "newvalue": "openconfirm"},
                    {"value": "6", "newvalue": "established"}
                ]
            }));
        }
        TemplatePreset::DatacomDmos => {
            items.push(mk_item(
                tpl_name,
                "Datacom DmOS CPU Utilization",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.3709.3.6.2.1.1.1.0"),
                "dmos.cpu.util",
                "1m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:cpu"),
                    "Datacom DmOS CPU elevada (> {$CPU_HIGH}%)",
                    &format!("avg(/{tpl_name}/dmos.cpu.util,5m)>{{$CPU_HIGH}}"),
                    &format!("avg(/{tpl_name}/dmos.cpu.util,5m)<({{$CPU_HIGH}}-10)"),
                    "HIGH",
                    "CPU do switch metro acima do limiar por 5m com histerese de 10%.",
                )],
            ));
            items.push(mk_item(
                tpl_name,
                "Datacom DmOS Memória Utilizada (%)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.3709.3.6.2.1.2.1.0"),
                "dmos.mem.util",
                "2m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![],
            ));
            discoveries.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:lld:dom")),
                "name": "Descoberta DOM SFP+/QSFP28 Datacom",
                "type": "SNMP_AGENT",
                "snmp_oid": "discovery[{#IFNAME},1.3.6.1.2.1.31.1.1.1.1]",
                "key": "dmos.dom.discovery",
                "delay": "1h",
                "lifetime": "30d",
                "item_prototypes": [
                    mk_item(
                        tpl_name,
                        "DOM {#IFNAME}: Potência Rx (dBm)",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.3709.3.6.4.1.1.4.{#SNMPINDEX}"),
                        "dmos.dom.rx[{#SNMPINDEX}]",
                        "2m",
                        "FLOAT",
                        "dBm",
                        None,
                        vec![],
                        vec![],
                    )
                ],
                "trigger_prototypes": [
                    mk_trigger(
                        &format!("{tpl_name}:trig_proto:dom"),
                        "Datacom {#IFNAME}: Sinal óptico Rx crítico (< {$DOM_RX_CRIT} dBm)",
                        &format!("avg(/{tpl_name}/dmos.dom.rx[{{#SNMPINDEX}}],5m)<{{$DOM_RX_CRIT}}"),
                        &format!("avg(/{tpl_name}/dmos.dom.rx[{{#SNMPINDEX}}],5m)>({{$DOM_RX_CRIT}}+2)"),
                        "HIGH",
                        "Risco iminente de abertura do anel metro por atenuação óptica; histerese de +2 dB.",
                    )
                ]
            }));
        }
        TemplatePreset::UbiquitiRf => {
            macros.push(json!({"macro": "{$RF_RSSI_WARN}", "value": "-68"}));
            macros.push(json!({"macro": "{$RF_CHAIN_DIFF_MAX}", "value": "5"}));
            items.push(mk_item(
                tpl_name,
                "Ubiquiti RF RSSI (dBm)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.41112.1.4.5.1.5.1"),
                "ubnt.rf.rssi",
                "1m",
                "FLOAT",
                "dBm",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:rssi"),
                    "Enlace RF RSSI degradado (< {$RF_RSSI_WARN} dBm)",
                    &format!("avg(/{tpl_name}/ubnt.rf.rssi,5m)<{{$RF_RSSI_WARN}}"),
                    &format!("avg(/{tpl_name}/ubnt.rf.rssi,5m)>({{$RF_RSSI_WARN}}+3)"),
                    "AVERAGE",
                    "Desalinhamento ou chuva/obstrução degradando RSSI; recupera +3 dB acima.",
                )],
            ));
            items.push(mk_item(
                tpl_name,
                "Ubiquiti RF Noise Floor (dBm)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.41112.1.4.5.1.8.1"),
                "ubnt.rf.noise_floor",
                "1m",
                "FLOAT",
                "dBm",
                None,
                vec![],
                vec![],
            ));
            items.push(mk_item(
                tpl_name,
                "Ubiquiti RF Capacidade TX (bps)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.41112.1.4.5.1.9.1"),
                "ubnt.rf.tx_capacity",
                "1m",
                "UNSIGNED",
                "bps",
                None,
                vec![],
                vec![],
            ));
            items.push(mk_item(
                tpl_name,
                "Ubiquiti RF Capacidade RX (bps)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.41112.1.4.5.1.10.1"),
                "ubnt.rf.rx_capacity",
                "1m",
                "UNSIGNED",
                "bps",
                None,
                vec![],
                vec![],
            ));
            items.push(mk_item(
                tpl_name,
                "Ubiquiti airMAX CCQ (%)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.41112.1.4.5.1.7.1"),
                "ubnt.rf.ccq",
                "1m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:ccq"),
                    "Enlace RF CCQ abaixo de 80%",
                    &format!("avg(/{tpl_name}/ubnt.rf.ccq,5m)<80"),
                    &format!("avg(/{tpl_name}/ubnt.rf.ccq,5m)>85"),
                    "WARNING",
                    "Queda de qualidade de modulação RF; histerese de 5% na recuperação.",
                )],
            ));
        }
        TemplatePreset::GponOlt => {
            macros.push(json!({"macro": "{$ONU_RX_WARN}", "value": "-25"}));
            macros.push(json!({"macro": "{$ONU_RX_CRIT}", "value": "-28"}));
            items.push(mk_item(
                tpl_name,
                "OLT CPU Utilization (%)",
                "SNMP_AGENT",
                Some("1.3.6.1.4.1.2011.6.3.5.1.1.2.0"),
                "olt.cpu.util",
                "1m",
                "FLOAT",
                "%",
                None,
                vec![],
                vec![mk_trigger(
                    &format!("{tpl_name}:trig:cpu"),
                    "OLT GPON CPU elevada (> {$CPU_HIGH}%)",
                    &format!("avg(/{tpl_name}/olt.cpu.util,5m)>{{$CPU_HIGH}}"),
                    &format!("avg(/{tpl_name}/olt.cpu.util,5m)<({{$CPU_HIGH}}-10)"),
                    "HIGH",
                    "CPU da OLT elevada; verificar tempestade de auth/TR-069 ou loop PON.",
                )],
            ));
            discoveries.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:lld:pon")),
                "name": "Descoberta de Portas PON (OLT)",
                "type": "SNMP_AGENT",
                "snmp_oid": "discovery[{#PON_PORT},1.3.6.1.2.1.31.1.1.1.1]",
                "key": "olt.pon.discovery",
                "delay": "1h",
                "lifetime": "30d",
                "item_prototypes": [
                    mk_item(
                        tpl_name,
                        "PON {#PON_PORT}: Potência Óptica Tx (dBm)",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.2011.6.128.1.1.2.21.1.4.{#SNMPINDEX}"),
                        "olt.pon.tx_power[{#SNMPINDEX}]",
                        "5m",
                        "FLOAT",
                        "dBm",
                        None,
                        vec![pp_multiplier("0.01")],
                        vec![],
                    ),
                    mk_item(
                        tpl_name,
                        "PON {#PON_PORT}: ONUs Online",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.2011.6.128.1.1.2.21.1.36.{#SNMPINDEX}"),
                        "olt.pon.onus_online[{#SNMPINDEX}]",
                        "2m",
                        "UNSIGNED",
                        "",
                        None,
                        vec![],
                        vec![],
                    )
                ],
                "trigger_prototypes": [
                    mk_trigger(
                        &format!("{tpl_name}:trig_proto:pon_mass"),
                        "Porta PON {#PON_PORT}: Queda massiva de ONUs (>50% em 5m)",
                        &format!("change(/{tpl_name}/olt.pon.onus_online[{{#SNMPINDEX}}])<-(last(/{tpl_name}/olt.pon.onus_online[{{#SNMPINDEX}}],#2)*0.5) and last(/{tpl_name}/olt.pon.onus_online[{{#SNMPINDEX}}],#2)>=4"),
                        &format!("last(/{tpl_name}/olt.pon.onus_online[{{#SNMPINDEX}}])>=last(/{tpl_name}/olt.pon.onus_online[{{#SNMPINDEX}}],#3)*0.9"),
                        "DISASTER",
                        "Possível rompimento de tronco/splitter primário na porta PON {#PON_PORT}.",
                    )
                ]
            }));
            discoveries.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:lld:onu")),
                "name": "Descoberta de ONUs GPON",
                "type": "SNMP_AGENT",
                "snmp_oid": "discovery[{#ONU_ID},1.3.6.1.4.1.2011.6.128.1.1.2.43.1.3]",
                "key": "olt.onu.discovery",
                "delay": "30m",
                "lifetime": "14d",
                "item_prototypes": [
                    mk_item(
                        tpl_name,
                        "ONU {#ONU_ID}: Sinal Rx (dBm)",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.2011.6.128.1.1.2.51.1.4.{#SNMPINDEX}"),
                        "olt.onu.rx[{#SNMPINDEX}]",
                        "5m",
                        "FLOAT",
                        "dBm",
                        None,
                        vec![pp_multiplier("0.01")],
                        vec![],
                    ),
                    mk_item(
                        tpl_name,
                        "ONU {#ONU_ID}: Estado Operacional",
                        "SNMP_AGENT",
                        Some("1.3.6.1.4.1.2011.6.128.1.1.2.46.1.15.{#SNMPINDEX}"),
                        "olt.onu.state[{#SNMPINDEX}]",
                        "5m",
                        "UNSIGNED",
                        "",
                        Some("ONU operational state"),
                        vec![pp_discard_heartbeat("30m")],
                        vec![],
                    )
                ],
                "trigger_prototypes": [
                    mk_trigger(
                        &format!("{tpl_name}:trig_proto:onu_rx"),
                        "ONU {#ONU_ID}: Sinal Rx atenuado (< {$ONU_RX_WARN} dBm)",
                        &format!("avg(/{tpl_name}/olt.onu.rx[{{#SNMPINDEX}}],15m)<{{$ONU_RX_WARN}}"),
                        &format!("avg(/{tpl_name}/olt.onu.rx[{{#SNMPINDEX}}],15m)>({{$ONU_RX_WARN}}+1.5)"),
                        "WARNING",
                        "ONU com atenuação óptica; histerese de +1.5 dB na recuperação.",
                    )
                ]
            }));
            valuemaps.push(json!({
                "uuid": deterministic_uuid(&format!("{tpl_name}:vm:onu")),
                "name": "ONU operational state",
                "mappings": [
                    {"value": "1", "newvalue": "online"},
                    {"value": "2", "newvalue": "offline"}
                ]
            }));
        }
    }

    json!({
        "zabbix_export": {
            "version": "7.0",
            "template_groups": [
                {
                    "uuid": deterministic_uuid(&format!("group:{tpl_group}")),
                    "name": tpl_group
                }
            ],
            "templates": [
                {
                    "uuid": deterministic_uuid(&format!("tpl:{tpl_name}")),
                    "template": tpl_name,
                    "name": tpl_name,
                    "description": preset.description(),
                    "groups": [
                        {"name": tpl_group}
                    ],
                    "items": items,
                    "discovery_rules": discoveries,
                    "macros": macros,
                    "valuemaps": valuemaps
                }
            ]
        }
    })
}

fn json_to_yaml(val: &Value) -> String {
    let mut out = String::new();
    write_yaml_value(&mut out, val, 0);
    out
}

fn write_yaml_value(out: &mut String, val: &Value, indent: usize) {
    match val {
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}\n");
                return;
            }
            for (k, v) in map {
                push_indent(out, indent);
                out.push_str(&yaml_scalar_key(k));
                match v {
                    Value::Object(m) if !m.is_empty() => {
                        out.push_str(":\n");
                        write_yaml_value(out, v, indent + 2);
                    }
                    Value::Array(a) if !a.is_empty() => {
                        out.push_str(":\n");
                        write_yaml_value(out, v, indent + 2);
                    }
                    Value::Object(_) => out.push_str(": {}\n"),
                    Value::Array(_) => out.push_str(": []\n"),
                    _ => {
                        out.push_str(": ");
                        out.push_str(&yaml_scalar_val(v));
                        out.push('\n');
                    }
                }
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                push_indent(out, indent);
                out.push_str("[]\n");
                return;
            }
            for item in arr {
                match item {
                    Value::Object(map) if !map.is_empty() => {
                        let mut first = true;
                        for (k, v) in map {
                            if first {
                                push_indent(out, indent);
                                out.push_str("- ");
                                first = false;
                            } else {
                                push_indent(out, indent + 2);
                            }
                            out.push_str(&yaml_scalar_key(k));
                            match v {
                                Value::Object(m) if !m.is_empty() => {
                                    out.push_str(":\n");
                                    write_yaml_value(out, v, indent + 4);
                                }
                                Value::Array(a) if !a.is_empty() => {
                                    out.push_str(":\n");
                                    write_yaml_value(out, v, indent + 4);
                                }
                                Value::Object(_) => out.push_str(": {}\n"),
                                Value::Array(_) => out.push_str(": []\n"),
                                _ => {
                                    out.push_str(": ");
                                    out.push_str(&yaml_scalar_val(v));
                                    out.push('\n');
                                }
                            }
                        }
                    }
                    Value::Array(sub) if !sub.is_empty() => {
                        push_indent(out, indent);
                        out.push_str("-\n");
                        write_yaml_value(out, item, indent + 2);
                    }
                    _ => {
                        push_indent(out, indent);
                        out.push_str("- ");
                        out.push_str(&yaml_scalar_val(item));
                        out.push('\n');
                    }
                }
            }
        }
        _ => {
            push_indent(out, indent);
            out.push_str(&yaml_scalar_val(val));
            out.push('\n');
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn yaml_scalar_key(k: &str) -> String {
    if k.is_empty()
        || !k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        yaml_single_quote(k)
    } else {
        k.to_string()
    }
}

fn yaml_scalar_val(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => yaml_quote_if_needed(s),
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

fn yaml_quote_if_needed(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let low = s.to_ascii_lowercase();
    let looks_special = matches!(
        low.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
    ) || s.parse::<f64>().is_ok()
        || s.starts_with([
            ' ', '-', '?', ':', '{', '}', '[', ']', ',', '&', '*', '#', '!', '|', '>', '\'', '"',
            '%', '@', '`',
        ])
        || s.ends_with(' ')
        || s.contains('\n')
        || s.contains(": ")
        || s.contains(" #");
    if looks_special {
        yaml_single_quote(s)
    } else {
        s.to_string()
    }
}

fn yaml_single_quote(s: &str) -> String {
    if s.contains('\n') {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("\"{escaped}\"")
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(
        ".zbx-tpl.{}.{seq}.{nanos}.zerith-tmp",
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
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use zerith_providers::Cancel;
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
        assert!(tool.concurrency_safe(&json!({"action": "triggers"})));
        assert!(tool.concurrency_safe(&json!({"action": "trends"})));
        assert!(tool.concurrency_safe(&json!({"action": "templates"})));
        assert!(tool.concurrency_safe(&json!({"action": "host_summary"})));
        assert!(tool.concurrency_safe(&json!({"action": "sla_report"})));
        assert!(tool.concurrency_safe(&json!({"action": "generate_template"})));
        assert!(tool.concurrency_safe(&json!({"action": "maintenance"})));
        assert!(tool.concurrency_safe(&json!({"action": "maintenance", "op": "list"})));
        assert!(!tool.concurrency_safe(&json!({"action": "maintenance", "op": "create"})));
        assert!(!tool.concurrency_safe(&json!({"action": "maintenance", "op": "delete"})));
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
        for ro_act in [
            "problems",
            "triggers",
            "trends",
            "templates",
            "host_summary",
            "sla_report",
            "generate_template",
        ] {
            assert!(
                tool.action(&json!({"action": ro_act, "url": "https://z"}), &plan_ctx)
                    .is_ok(),
                "{ro_act} should be allowed in plan mode"
            );
        }
        assert!(tool
            .action(
                &json!({"action": "maintenance", "op": "list", "url": "https://z"}),
                &plan_ctx
            )
            .is_ok());
        assert!(matches!(
            tool.action(
                &json!({"action": "maintenance", "op": "create", "url": "https://z"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            tool.action(
                &json!({"action": "maintenance", "op": "delete", "url": "https://z"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            tool.action(
                &json!({"action": "generate_template", "preset": "gpon_olt", "output_path": "tpl.yaml"}),
                &plan_ctx
            ),
            Err(ToolError::Denied(_))
        ));
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

    #[test]
    fn triggers_trends_templates_round_trip() {
        let (url, seen) = fake_zabbix(|method, _| {
            let result = match method {
                "hostgroup.get" => json!([{"groupid": "10", "name": "Backbone"}]),
                "host.get" => json!([{"hostid": "101", "host": "asr-core-01"}]),
                "trigger.get" => json!([{
                    "triggerid": "8801",
                    "description": "BGP Peer 10.0.0.2 down",
                    "priority": "4",
                    "value": "1",
                    "hosts": [{"hostid": "101", "host": "asr-core-01"}]
                }]),
                "trend.get" => json!([{
                    "itemid": "5001",
                    "clock": "1700000000",
                    "num": "60",
                    "value_min": "120000000",
                    "value_avg": "450000000",
                    "value_max": "890000000"
                }]),
                "template.get" => json!([{
                    "templateid": "10500",
                    "host": "Template Cisco ASR",
                    "items": "42",
                    "triggers": "14"
                }]),
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

        let trigs = execute_with_client(
            &client,
            &json!({
                "action": "triggers",
                "groups": ["Backbone"],
                "min_severity": 3,
                "only_true": true
            }),
        )
        .unwrap();
        assert!(trigs.contains("BGP Peer 10.0.0.2 down"), "{trigs}");

        let trends = execute_with_client(
            &client,
            &json!({
                "action": "trends",
                "itemids": ["5001"],
                "time_from": "7d",
                "time_till": "now"
            }),
        )
        .unwrap();
        assert!(trends.contains("890000000"), "{trends}");

        let tpls = execute_with_client(
            &client,
            &json!({"action": "templates", "patterns": ["Cisco"]}),
        )
        .unwrap();
        assert!(tpls.contains("Template Cisco ASR"), "{tpls}");

        let reqs = seen.lock().unwrap();
        let t_req = reqs.iter().find(|r| r["method"] == "trigger.get").unwrap();
        assert_eq!(t_req["params"]["expandDescription"], true);
        assert_eq!(t_req["params"]["filter"]["value"], 1);
        assert_eq!(t_req["params"]["min_severity"], 3);

        let tr_req = reqs.iter().find(|r| r["method"] == "trend.get").unwrap();
        assert!(
            tr_req["params"]["time_from"].as_i64().unwrap()
                < tr_req["params"]["time_till"].as_i64().unwrap()
        );
    }

    #[test]
    fn host_summary_and_sla_report_composite_triage() {
        let (url, _seen) = fake_zabbix(|method, params| {
            let result = match method {
                "host.get" => {
                    if params["params"].get("monitored_hosts").is_some() {
                        json!([
                            {
                                "hostid": "101",
                                "host": "asr-core-01",
                                "status": "0",
                                "maintenance_status": "0",
                                "interfaces": [{"ip": "10.0.0.1", "available": "1"}]
                            },
                            {
                                "hostid": "102",
                                "host": "olt-pop-02",
                                "status": "0",
                                "maintenance_status": "1",
                                "interfaces": [{"ip": "10.0.0.2", "available": "2", "error": "SNMP timeout"}]
                            }
                        ])
                    } else {
                        json!([{
                            "hostid": "101",
                            "host": "asr-core-01",
                            "name": "ASR Core 01",
                            "status": "0",
                            "maintenance_status": "0",
                            "interfaces": [{"interfaceid": "1", "ip": "10.0.0.1", "available": "1", "error": ""}],
                            "parentTemplates": [{"templateid": "10", "name": "Cisco ASR"}],
                            "groups": [{"groupid": "20", "name": "Backbone"}]
                        }])
                    }
                }
                "problem.get" => json!([
                    {"eventid": "701", "objectid": "901", "name": "QFP Load > 85%", "severity": "5", "clock": "1700000100", "acknowledged": "0"},
                    {"eventid": "702", "objectid": "902", "name": "SNMP timeout", "severity": "4", "clock": "1700000050", "acknowledged": "1"}
                ]),
                "trigger.get" => json!([
                    {"triggerid": "901", "hosts": [{"hostid": "101", "host": "asr-core-01"}]},
                    {"triggerid": "902", "hosts": [{"hostid": "102", "host": "olt-pop-02"}]}
                ]),
                "item.get" => json!([
                    {"itemid": "1", "name": "ICMP ping", "key_": "icmpping", "lastvalue": "1", "state": "0", "error": ""},
                    {"itemid": "2", "name": "DOM Rx", "key_": "cisco.dom.rx[1]", "lastvalue": "", "state": "1", "error": "OID not supported"}
                ]),
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

        let summary_raw = execute_with_client(
            &client,
            &json!({"action": "host_summary", "host": "asr-core-01"}),
        )
        .unwrap();
        let summary: Value = serde_json::from_str(&summary_raw).unwrap();
        assert_eq!(summary["host"]["host"], "asr-core-01");
        assert_eq!(summary["availability"]["interfaces_up"], 1);
        assert_eq!(summary["active_problems_count"], 2);
        assert_eq!(summary["error_items_count"], 1);
        assert_eq!(summary["error_items"][0]["key_"], "cisco.dom.rx[1]");

        let sla_raw = execute_with_client(&client, &json!({"action": "sla_report"})).unwrap();
        let sla: Value = serde_json::from_str(&sla_raw).unwrap();
        assert_eq!(sla["hosts"]["total_monitored"], 2);
        assert_eq!(sla["hosts"]["available_up"], 1);
        assert_eq!(sla["hosts"]["unavailable_down"], 1);
        assert_eq!(sla["hosts"]["availability_known_pct"], 50.0);
        assert_eq!(sla["problems"]["total_active"], 2);
        assert_eq!(sla["problems"]["by_severity"]["5"], 1);
        assert_eq!(sla["problems"]["by_severity"]["4"], 1);
        assert_eq!(sla["top_offending_hosts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn maintenance_list_create_delete_round_trip() {
        let (url, seen) = fake_zabbix(|method, body| {
            let result = match method {
                "hostgroup.get" => json!([{"groupid": "20", "name": "Backbone"}]),
                "maintenance.get" => json!([{"maintenanceid": "301", "name": "Janela Fibra"}]),
                "maintenance.create" => json!({"maintenanceids": ["302"]}),
                "maintenance.delete" => json!({"maintenanceids": body["params"]}),
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

        let listed = execute_with_client(&client, &json!({"action": "maintenance"})).unwrap();
        assert!(listed.contains("Janela Fibra"), "{listed}");

        let created = execute_with_client(
            &client,
            &json!({
                "action": "maintenance",
                "op": "create",
                "name": "Troca de GBIC Pop Sul",
                "groups": ["Backbone"],
                "duration_mins": 45,
                "description": "Mudança programada"
            }),
        )
        .unwrap();
        assert!(created.contains("302"), "{created}");

        let deleted = execute_with_client(
            &client,
            &json!({"action": "maintenance", "op": "delete", "maintenanceids": ["302"]}),
        )
        .unwrap();
        assert!(deleted.contains("302"), "{deleted}");

        let reqs = seen.lock().unwrap();
        let cr = reqs
            .iter()
            .find(|r| r["method"] == "maintenance.create")
            .unwrap();
        assert_eq!(cr["params"]["groups"][0]["groupid"], "20");
        assert_eq!(cr["params"]["timeperiods"][0]["period"], 2700);
    }

    #[test]
    fn generate_template_covers_all_presets_yaml_and_json_and_disk_save() {
        for preset_name in ["cisco_asr_1001x", "datacom_dmos", "ubiquiti_rf", "gpon_olt"] {
            let yaml = render_template_for_args(&json!({
                "action": "generate_template",
                "preset": preset_name,
                "format": "yaml"
            }))
            .unwrap();
            assert!(
                yaml.contains("zabbix_export:"),
                "{preset_name}: missing root"
            );
            assert!(
                yaml.contains("version: '7.0'"),
                "{preset_name}: missing 7.0 version"
            );
            assert!(
                yaml.contains("discovery_rules:"),
                "{preset_name}: missing LLD"
            );
            assert!(
                yaml.contains("recovery_mode: RECOVERY_EXPRESSION"),
                "{preset_name}: missing hysteresis"
            );
            assert!(
                yaml.contains("valuemaps:"),
                "{preset_name}: missing valuemaps"
            );

            let json_str = render_template_for_args(&json!({
                "action": "generate_template",
                "preset": preset_name,
                "format": "json"
            }))
            .unwrap();
            let parsed: Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(parsed["zabbix_export"]["version"], "7.0");
            let tpl = &parsed["zabbix_export"]["templates"][0];
            assert!(!tpl["items"].as_array().unwrap().is_empty());
            assert!(!tpl["discovery_rules"].as_array().unwrap().is_empty());
        }

        // Verify offline run() with output_path requires no token_secret and writes atomically.
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
        let tool = NetZabbixTool::new();
        let out = tool
            .run(
                &json!({
                    "action": "generate_template",
                    "preset": "gpon_olt",
                    "output_path": "templates/olt-gpon.yaml"
                }),
                &ctx,
                &exec,
            )
            .unwrap();
        assert!(out.contains("\"saved\": true"), "{out}");
        let disk_text =
            std::fs::read_to_string(dir.path().join("templates/olt-gpon.yaml")).unwrap();
        assert!(disk_text.contains("zabbix_export:"));
        assert!(disk_text.contains("olt.pon.discovery"));
    }
}
