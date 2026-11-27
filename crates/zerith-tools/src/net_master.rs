//! Ferramenta `net_audit`: auditoria determinística em Rust para Cisco ASR/IOS-XE/XR/NX-OS,
//! Datacom (DmOS e DmSwitch), Ubiquiti RF (airOS/airMAX/LTU/UniFi), Linux SRE e
//! construção de topologia + blast radius (`topology_build`).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use serde_json::{json, Value};

use crate::fntool::{make, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make(
        "net_audit",
        "Auditoria determinística e planejamento de mudança segura para Cisco ASR/IOS-XE/XR/NX-OS, \
         Datacom (DmOS e DmSwitch), Ubiquiti RF (airOS/airMAX/LTU/UniFi), Linux SRE e \
         construção de topologia com blast radius (`topology_build`). \
         Em `mode=\"commands\"` (padrão sem `output`), gera lotes read-only por modelo; \
         em `mode=\"parse\"` (automático quando há `output`/`outputs`), analisa saídas sem regex \
         frágil e retorna JSON estruturado com achados (`critical`/`warning`/`ok`); \
         em `mode=\"plan_change\"`, gera receita transacional com rollback automático.",
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "cisco_asr_audit",
                        "datacom_audit",
                        "ubiquiti_rf_audit",
                        "linux_sre_audit",
                        "topology_build"
                    ],
                    "description": "domínio da auditoria ou construção de topologia"
                },
                "mode": {
                    "type": "string",
                    "enum": ["commands", "parse", "plan_change"],
                    "description": "commands (gera comandos), parse (analisa output/outputs), plan_change (receita com rollback)"
                },
                "model": {
                    "type": "string",
                    "description": "família/modelo: asr1001, asr1001x, asr1001hx, asr1002x, iosxe, iosxr, nxos, dmos, dmswitch, airos, airmax_ac, ltu, unifi, linux"
                },
                "output": {
                    "type": "string",
                    "description": "saída bruta (CLI ou JSON) para análise determinística"
                },
                "outputs": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "mapa {comando: saída_bruta} para análise combinada"
                },
                "revert_minutes": {
                    "type": "integer",
                    "description": "minutos para revert timer no IOS-XE (padrão 5)"
                },
                "revert_seconds": {
                    "type": "integer",
                    "description": "segundos para commit confirmed / dead-man's switch (padrão 120)"
                },
                "freq_mhz": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: frequência em MHz para cálculo de FSPL"
                },
                "distance_km": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: distância do enlace em km para cálculo de FSPL"
                },
                "tx_power_dbm": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: potência TX em dBm"
                },
                "antenna_gain_dbi": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: ganho da antena em dBi (aplicado nos dois lados se rx_antenna_gain_dbi for omitido)"
                },
                "tx_antenna_gain_dbi": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: ganho TX explícito em dBi"
                },
                "rx_antenna_gain_dbi": {
                    "type": "number",
                    "description": "ubiquiti_rf_audit: ganho RX explícito em dBi"
                },
                "devices": {
                    "type": "array",
                    "description": "topology_build: lista de nós/dispositivos"
                },
                "lldp": {
                    "description": "topology_build: vizinhos LLDP (array JSON ou texto CLI)"
                },
                "cdp": {
                    "description": "topology_build: vizinhos CDP (array JSON ou texto CLI)"
                },
                "bgp": {
                    "description": "topology_build: sessões BGP (array JSON ou texto CLI)"
                },
                "ospf": {
                    "description": "topology_build: vizinhos OSPF (array JSON ou texto CLI)"
                },
                "rf_links": {
                    "type": "array",
                    "description": "topology_build: enlaces RF (array de objetos {from, to, ...})"
                },
                "target_node": {
                    "type": "string",
                    "description": "topology_build: nó alvo para calcular blast_radius"
                },
                "target_interface": {
                    "type": "string",
                    "description": "topology_build: interface opcional do nó alvo para blast_radius granular"
                }
            }
        }),
        "JSON estruturado com comandos de auditoria, achados (critical/warning/ok), métricas, Mermaid ou receita de mudança segura",
        Permission::Read,
        30,
        true,
        run_net_audit,
    )]
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct Finding {
    severity: &'static str,
    category: &'static str,
    finding: String,
    evidence: String,
    next_step: String,
}

impl Finding {
    fn new(
        severity: &'static str,
        category: &'static str,
        finding: impl Into<String>,
        evidence: impl Into<String>,
        next_step: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            category,
            finding: finding.into(),
            evidence: evidence.into(),
            next_step: next_step.into(),
        }
    }
}

fn to_pretty_json(val: &Value) -> Result<String> {
    serde_json::to_string_pretty(val).map_err(|e| ToolError::Failed(e.to_string()))
}

fn has_topology_inputs(args: &Value) -> bool {
    ["devices", "lldp", "cdp", "bgp", "ospf", "rf_links"]
        .iter()
        .any(|k| args.get(k).is_some_and(|v| !v.is_null()))
}

fn resolve_mode<'a>(args: &'a Value, action: &str) -> Result<&'a str> {
    if let Some(m) = opt_str(args, "mode") {
        let low = m.trim();
        if matches!(low, "commands" | "parse" | "plan_change") {
            return Ok(low);
        }
        return Err(ToolError::BadArguments(format!(
            "mode inválido '{m}': use commands, parse ou plan_change"
        )));
    }
    let has_raw = opt_str(args, "output").is_some()
        || args
            .get("outputs")
            .and_then(Value::as_object)
            .is_some_and(|m| !m.is_empty());
    if has_raw || (action == "topology_build" && has_topology_inputs(args)) {
        Ok("parse")
    } else {
        Ok("commands")
    }
}

fn collect_raw_inputs(args: &Value) -> (String, BTreeMap<String, String>) {
    let mut by_cmd = BTreeMap::new();
    let mut sections = Vec::new();
    if let Some(out) = opt_str(args, "output") {
        sections.push(out.to_string());
    }
    if let Some(map) = args.get("outputs").and_then(Value::as_object) {
        for (cmd, val) in map {
            if let Some(text) = val.as_str() {
                by_cmd.insert(cmd.to_ascii_lowercase(), text.to_string());
                sections.push(format!("### {cmd}\n{text}"));
            }
        }
    }
    (sections.join("\n"), by_cmd)
}

fn pick_section<'a>(
    by_cmd: &'a BTreeMap<String, String>,
    needle: &str,
    combined: &'a str,
) -> &'a str {
    for (cmd, text) in by_cmd {
        if cmd.contains(needle) {
            return text.as_str();
        }
    }
    combined
}

fn run_net_audit(args: &Value, _ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    exec.check()?;
    let action = str_arg(args, "action")?;
    let mode = resolve_mode(args, action)?;
    let model = opt_str(args, "model")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match action {
        "cisco_asr_audit" => match mode {
            "commands" => to_pretty_json(&cisco_asr_commands(&model)),
            "plan_change" => to_pretty_json(&cisco_asr_plan_change(args, &model)),
            "parse" => to_pretty_json(&cisco_asr_parse(args, &model)?),
            _ => unreachable!(),
        },
        "datacom_audit" => match mode {
            "commands" => to_pretty_json(&datacom_commands(&model)),
            "plan_change" => to_pretty_json(&datacom_plan_change(args, &model)),
            "parse" => to_pretty_json(&datacom_parse(args, &model)?),
            _ => unreachable!(),
        },
        "ubiquiti_rf_audit" => match mode {
            "commands" => to_pretty_json(&ubiquiti_commands(&model)),
            "plan_change" => to_pretty_json(&ubiquiti_plan_change(&model)),
            "parse" => to_pretty_json(&ubiquiti_parse(args, &model)?),
            _ => unreachable!(),
        },
        "linux_sre_audit" => match mode {
            "commands" => to_pretty_json(&linux_sre_commands()),
            "plan_change" => to_pretty_json(&linux_sre_plan_change(args)),
            "parse" => to_pretty_json(&linux_sre_parse(args)?),
            _ => unreachable!(),
        },
        "topology_build" => to_pretty_json(&topology_build(args)?),
        other => Err(ToolError::BadArguments(format!(
            "action desconhecida '{other}' (use cisco_asr_audit, datacom_audit, ubiquiti_rf_audit, linux_sre_audit ou topology_build)"
        ))),
    }
}

// ============================================================================
// 1. Cisco ASR / IOS-XE / IOS-XR / NX-OS
// ============================================================================

fn cisco_family(model: &str) -> &'static str {
    if model.contains("xr") || model.contains("asr9") || model.contains("ncs") {
        "iosxr"
    } else if model.contains("nx") || model.contains("nexus") {
        "nxos"
    } else {
        "iosxe"
    }
}

fn cisco_asr_commands(model: &str) -> Value {
    let family = cisco_family(model);
    let commands: Vec<&'static str> = match family {
        "iosxr" => vec![
            "show version",
            "show platform",
            "show processes cpu | exclude 0%      0%      0%",
            "show memory summary",
            "show bgp ipv4 unicast summary",
            "show bgp ipv6 unicast summary",
            "show isis neighbors",
            "show ospf neighbor",
            "show subscriber session all summary",
            "show lpts pifib hardware police location all",
            "show controllers optics all",
            "show configuration commit list 10",
        ],
        "nxos" => vec![
            "show version",
            "show system resources",
            "show hardware capacity forwarding",
            "show policy-map interface control-plane",
            "show ip bgp summary",
            "show bgp all summary",
            "show ip ospf neighbors",
            "show vpc",
            "show port-channel summary",
            "show interface transceiver details",
            "show checkpoint summary",
        ],
        _ => vec![
            "show version",
            "show platform hardware throughput level",
            "show license summary",
            "show platform hardware qfp active datapath utilization",
            "show platform hardware qfp active statistics drop",
            "show processes cpu sorted | exclude 0.00",
            "show memory platform",
            "show policy-map control-plane",
            "show pppoe summary",
            "show subscriber session summary",
            "show ip nat statistics",
            "show ip bgp summary",
            "show bgp all summary",
            "show ip ospf neighbor",
            "show interfaces transceiver detail",
            "show archive",
        ],
    };

    json!({
        "action": "cisco_asr_audit",
        "mode": "commands",
        "family": family,
        "model": if model.is_empty() { "asr1001x/iosxe" } else { model },
        "read_only": true,
        "commands": commands,
        "notes": [
            "Desabilite paginação antes com `terminal length 0` (net_connect já envia automaticamente).",
            "Em ASR 1001-X/HX, sempre confira `show platform hardware throughput level` para detectar cap de licença (2.5G/5G/10G)."
        ]
    })
}

fn cisco_asr_plan_change(args: &Value, model: &str) -> Value {
    let family = cisco_family(model);
    let revert_mins = args
        .get("revert_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 60);
    let revert_secs = args
        .get("revert_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(300)
        .clamp(30, 3600);

    match family {
        "iosxr" => json!({
            "action": "cisco_asr_audit",
            "mode": "plan_change",
            "family": "iosxr",
            "steps": [
                "1. Backup prévio via `net_backup` ou `show running-config`.",
                "2. Entrar em configuração: `configure`",
                "3. Revisar mudanças pendentes antes de aplicar: `show configuration`",
                format!("4. Aplicar com rollback automático: `commit confirmed {revert_secs}`"),
                "5. Validar acesso, BGP (`show bgp ipv4 unicast summary`) e LPTS (`show lpts pifib hardware police location all`).",
                "6. Confirmar definitivamente: `commit` (se não confirmar, reverte sozinho)."
            ],
            "rollback_commands": [
                "show configuration commit list",
                "rollback configuration last 1"
            ]
        }),
        "nxos" => json!({
            "action": "cisco_asr_audit",
            "mode": "plan_change",
            "family": "nxos",
            "steps": [
                "1. Backup prévio via `net_backup`.",
                "2. Criar checkpoint antes da mudança: `checkpoint pre_mudanca`",
                "3. Aplicar alteração em janela pequena.",
                "4. Validar vPC/BGP/OSPF/interfaces.",
                "5. Persistir: `copy running-config startup-config`."
            ],
            "rollback_commands": [
                "rollback running-config checkpoint pre_mudanca"
            ]
        }),
        _ => json!({
            "action": "cisco_asr_audit",
            "mode": "plan_change",
            "family": "iosxe",
            "prerequisite": "O `archive` precisa estar configurado (`archive` -> `path bootflash:archive`) para `configure terminal revert timer` funcionar.",
            "steps": [
                "1. Verificar se archive está ativo: `show archive`",
                "2. Backup prévio via `net_backup`.",
                format!("3. Abrir transação segura com revert automático: `configure terminal revert timer {revert_mins}`"),
                "4. Aplicar comandos de configuração e sair com `end`.",
                "5. Validar acesso, `show ip bgp summary`, `show platform hardware qfp active statistics drop`.",
                "6. Confirmar antes do timer expirar: `configure confirm`",
                "7. Persistir na NVRAM: `write memory`"
            ],
            "rollback_commands": [
                "configure revert now",
                "configure replace bootflash:<arquivo-archive> force"
            ]
        }),
    }
}

fn parse_first_f64_after(text: &str) -> Option<f64> {
    for tok in text.split(|c: char| c.is_whitespace() || matches!(c, '%' | ',' | ';' | ')')) {
        let cleaned = tok.trim_start_matches('(');
        if !cleaned.is_empty()
            && cleaned
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        {
            if let Ok(v) = cleaned.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_u64_token(tok: &str) -> Option<u64> {
    let cleaned = tok.trim_matches(|c: char| !c.is_ascii_digit());
    if cleaned.is_empty() {
        None
    } else {
        cleaned.parse::<u64>().ok()
    }
}

#[derive(Debug, Default, Serialize)]
struct QfpUtilSummary {
    processing_5s_pct: Option<f64>,
    processing_1m_pct: Option<f64>,
    processing_5m_pct: Option<f64>,
    processing_1h_pct: Option<f64>,
    total_pps: Option<u64>,
    total_bps: Option<u64>,
}

fn parse_qfp_utilization(text: &str) -> QfpUtilSummary {
    let mut out = QfpUtilSummary::default();
    let mut col_order: Vec<&'static str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if low.contains("5 secs") || low.contains("5secs") {
            col_order.clear();
            if low.contains("5 secs") || low.contains("5secs") {
                col_order.push("5s");
            }
            if low.contains("1 min") || low.contains("1min") {
                col_order.push("1m");
            }
            if low.contains("5 min") || low.contains("5min") {
                col_order.push("5m");
            }
            if low.contains("1 hour") || low.contains("1hr") {
                col_order.push("1h");
            }
            continue;
        }
        if low.starts_with("processing:") || low.starts_with("processing ") {
            let nums: Vec<f64> = line
                .split_whitespace()
                .filter_map(|t| t.trim_end_matches('%').parse::<f64>().ok())
                .collect();
            if nums.len() >= 4 {
                out.processing_5s_pct = Some(nums[0]);
                out.processing_1m_pct = Some(nums[1]);
                out.processing_5m_pct = Some(nums[2]);
                out.processing_1h_pct = Some(nums[3]);
            } else if !nums.is_empty() {
                for (idx, val) in nums.iter().enumerate() {
                    match col_order.get(idx).copied() {
                        Some("5s") => out.processing_5s_pct = Some(*val),
                        None if idx == 0 => out.processing_5s_pct = Some(*val),
                        Some("1m") => out.processing_1m_pct = Some(*val),
                        Some("5m") => out.processing_5m_pct = Some(*val),
                        Some("1h") => out.processing_1h_pct = Some(*val),
                        _ => {}
                    }
                }
            }
        } else if low.starts_with("total")
            && (low.contains("pps") || line.split_whitespace().count() >= 3)
        {
            let ints: Vec<u64> = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .collect();
            if !ints.is_empty() && out.total_pps.is_none() {
                out.total_pps = Some(ints[0]);
                if ints.len() >= 2 {
                    out.total_bps = Some(ints[1]);
                }
            }
        } else if low.contains("packet/sec") || low.contains("packets/sec") {
            let nums: Vec<u64> = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .collect();
            if let Some(&pps) = nums.first() {
                out.total_pps = Some(pps);
            }
        } else if low.contains("bit/sec") || low.contains("bits/sec") {
            let nums: Vec<u64> = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .collect();
            if let Some(&bps) = nums.first() {
                out.total_bps = Some(bps);
            }
        }
    }
    out
}

fn is_critical_qfp_drop(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    [
        "taildrop",
        "natnoport",
        "ipsec",
        "ipv4noadj",
        "ipv4noroute",
        "ipv6noadj",
        "ipv6noroute",
        "policer",
        "puntlimit",
        "forus",
        "mtuexceed",
        "badipchecksum",
        "nat",
        "wred",
        "qfp",
    ]
    .iter()
    .any(|k| low.contains(k))
        && !low.contains("notforus")
}

fn is_benign_qfp_drop(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    [
        "arpnotforus",
        "l2mc",
        "stpnotforwarding",
        "ipv6ndnotforus",
        "disabled",
        "unconfigured",
        "ess",
        "macfilter",
    ]
    .iter()
    .any(|k| low.contains(k))
}

#[derive(Debug, Default, Serialize)]
struct QfpDropSummary {
    critical_drops: BTreeMap<String, u64>,
    benign_drops: BTreeMap<String, u64>,
    other_drops: BTreeMap<String, u64>,
}

fn parse_qfp_drops(text: &str) -> QfpDropSummary {
    let mut out = QfpDropSummary::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('-')
            || line.starts_with('=')
            || line.to_ascii_lowercase().contains("drop cause")
            || line.to_ascii_lowercase().contains("packets")
        {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 {
            continue;
        }
        let name = cols[0];
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        let Some(packets) = parse_u64_token(cols[1]) else {
            continue;
        };
        if packets == 0 {
            continue;
        }
        if is_benign_qfp_drop(name) {
            out.benign_drops.insert(name.to_string(), packets);
        } else if is_critical_qfp_drop(name) {
            out.critical_drops.insert(name.to_string(), packets);
        } else {
            out.other_drops.insert(name.to_string(), packets);
        }
    }
    out
}

#[derive(Debug, Default, Serialize)]
struct ThroughputLicenseInfo {
    level_mbps: Option<u64>,
    level_label: Option<String>,
    capped: bool,
}

fn parse_throughput_level(text: &str) -> ThroughputLicenseInfo {
    let mut info = ThroughputLicenseInfo::default();
    for raw in text.lines() {
        let low = raw.trim().to_ascii_lowercase();
        if !(low.contains("throughput") || low.contains("license")) {
            continue;
        }
        for tok in raw.split_whitespace() {
            let t = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
            let tl = t.to_ascii_lowercase();
            if let Some(gbps_str) = tl.strip_suffix('g').or_else(|| tl.strip_suffix("gbps")) {
                if let Ok(g) = gbps_str.parse::<f64>() {
                    let mbps = (g * 1000.0).round() as u64;
                    info.level_mbps = Some(mbps);
                    info.level_label = Some(format!("{g}G"));
                    info.capped = mbps <= 10_000;
                    return info;
                }
            }
            if let Ok(bps) = t.parse::<u64>() {
                if bps >= 1_000_000_000 {
                    let mbps = bps / 1_000_000;
                    info.level_mbps = Some(mbps);
                    info.level_label = Some(format!("{}G", mbps as f64 / 1000.0));
                    info.capped = mbps <= 10_000;
                    return info;
                }
            }
        }
    }
    info
}

#[derive(Debug, Default, Serialize)]
struct CoppClass {
    class_name: String,
    conformed_pkts: u64,
    exceeded_pkts: u64,
    violated_pkts: u64,
    dropped_pkts: u64,
}

fn parse_copp(text: &str) -> Vec<CoppClass> {
    let mut classes = Vec::new();
    let mut cur: Option<CoppClass> = None;

    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if let Some(idx) = low.find("class-map:") {
            if let Some(c) = cur.take() {
                classes.push(c);
            }
            let name = line[idx + "class-map:".len()..]
                .split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_string();
            cur = Some(CoppClass {
                class_name: name,
                ..Default::default()
            });
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        if let Some(pos) = low.find("conformed ") {
            if let Some(n) = line[pos + "conformed ".len()..]
                .split_whitespace()
                .next()
                .and_then(parse_u64_token)
            {
                c.conformed_pkts += n;
            }
        }
        if let Some(pos) = low.find("exceeded ") {
            if let Some(n) = line[pos + "exceeded ".len()..]
                .split_whitespace()
                .next()
                .and_then(parse_u64_token)
            {
                c.exceeded_pkts += n;
            }
        }
        if let Some(pos) = low.find("violated ") {
            if let Some(n) = line[pos + "violated ".len()..]
                .split_whitespace()
                .next()
                .and_then(parse_u64_token)
            {
                c.violated_pkts += n;
            }
        }
        if let Some(pos) = low.find("dropped ") {
            if let Some(n) = line[pos + "dropped ".len()..]
                .split_whitespace()
                .next()
                .and_then(parse_u64_token)
            {
                c.dropped_pkts += n;
            }
        }
    }
    if let Some(c) = cur {
        classes.push(c);
    }
    classes
}

#[derive(Debug, Default, Serialize)]
struct BngNatSummary {
    pppoe_total: Option<u64>,
    pppoe_pta: Option<u64>,
    pppoe_trans: Option<u64>,
    subscriber_total: Option<u64>,
    nat_active_translations: Option<u64>,
    nat_hits: Option<u64>,
    nat_misses: Option<u64>,
    nat_miss_ratio_pct: Option<f64>,
    nat_pool_exhausted: bool,
}

fn parse_bng_nat(text: &str) -> BngNatSummary {
    let mut s = BngNatSummary::default();
    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if low.starts_with("total")
            && s.pppoe_total.is_none()
            && line.split_whitespace().count() >= 3
        {
            let nums: Vec<u64> = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .collect();
            if nums.len() >= 3 {
                s.pppoe_total = Some(nums[0]);
                s.pppoe_pta = Some(nums[1]);
                s.pppoe_trans = Some(nums[2]);
            }
        } else if low.contains("pppoe") && low.contains("total") && s.subscriber_total.is_none() {
            if let Some(n) = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .next_back()
            {
                s.subscriber_total = Some(n);
            }
        } else if low.contains("total active translations") {
            s.nat_active_translations = line
                .split(':')
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(parse_u64_token);
        } else if low.contains("hits:") {
            let toks: Vec<&str> = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|t| !t.is_empty())
                .collect();
            for pair in toks.windows(2) {
                let kl = pair[0].to_ascii_lowercase();
                if kl == "hits:" {
                    s.nat_hits = parse_u64_token(pair[1]);
                } else if kl == "misses:" {
                    s.nat_misses = parse_u64_token(pair[1]);
                }
            }
        }
        if low.contains("pool")
            && (low.contains("exhaust") || low.contains("no port") || low.contains("100%"))
        {
            s.nat_pool_exhausted = true;
        }
    }
    if let (Some(h), Some(m)) = (s.nat_hits, s.nat_misses) {
        let tot = h.saturating_add(m);
        if tot > 0 {
            s.nat_miss_ratio_pct = Some((m as f64 * 100.0) / tot as f64);
        }
    }
    s
}

#[derive(Debug, Clone, Serialize)]
struct BgpPeerRow {
    neighbor: String,
    remote_as: String,
    in_q: u64,
    out_q: u64,
    state_or_pfx: String,
    up_down: String,
}

fn looks_like_ip(tok: &str) -> bool {
    let dot_parts: Vec<&str> = tok.split('.').collect();
    if dot_parts.len() == 4 && dot_parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        return true;
    }
    tok.contains(':') && tok.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
}

fn parse_bgp_summary(text: &str) -> Vec<BgpPeerRow> {
    let mut peers = Vec::new();
    for raw in text.lines() {
        let cols: Vec<&str> = raw.split_whitespace().collect();
        if cols.len() < 9 || !looks_like_ip(cols[0]) {
            continue;
        }
        let in_q = parse_u64_token(cols[6]).unwrap_or(0);
        let out_q = parse_u64_token(cols[7]).unwrap_or(0);
        let up_down = cols[8].to_string();
        let state_or_pfx = cols[9..].join(" ");
        peers.push(BgpPeerRow {
            neighbor: cols[0].to_string(),
            remote_as: cols[2].to_string(),
            in_q,
            out_q,
            state_or_pfx,
            up_down,
        });
    }
    peers
}

#[derive(Debug, Clone, Serialize)]
struct OspfNeighborRow {
    neighbor_id: String,
    state: String,
    interface: String,
}

fn parse_ospf_neighbors(text: &str) -> Vec<OspfNeighborRow> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let cols: Vec<&str> = raw.split_whitespace().collect();
        if cols.len() < 5 || !looks_like_ip(cols[0]) {
            continue;
        }
        // Typical IOS: NeighborID  Pri  State  DeadTime  Address  Interface
        // Check Pri is numeric and cols[4] is an IP
        if cols[1].parse::<u32>().is_err() {
            continue;
        }
        let state = cols[2].trim_end_matches('/').to_string();
        let interface = cols.last().copied().unwrap_or("").to_string();
        rows.push(OspfNeighborRow {
            neighbor_id: cols[0].to_string(),
            state,
            interface,
        });
    }
    rows
}

#[derive(Debug, Clone, Serialize)]
struct OpticalDomRow {
    interface: String,
    tx_dbm: Option<f64>,
    rx_dbm: Option<f64>,
}

fn parse_optical_dom(text: &str) -> Vec<OpticalDomRow> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        let cols: Vec<&str> = line.split_whitespace().collect();
        // Compact table format: Gi0/0/0  35.0  3.30  15.2  -2.4  -6.8
        if cols.len() >= 6
            && (cols[0].starts_with("Gi")
                || cols[0].starts_with("Te")
                || cols[0].starts_with("Hu")
                || cols[0].starts_with("Fo")
                || cols[0].starts_with("Twe")
                || cols[0].starts_with("Eth")
                || cols[0].contains('/'))
            && cols.iter().filter_map(|c| c.parse::<f64>().ok()).count() >= 4
        {
            let floats: Vec<f64> = cols.iter().filter_map(|c| c.parse::<f64>().ok()).collect();
            let n = floats.len();
            rows.push(OpticalDomRow {
                interface: cols[0].to_string(),
                tx_dbm: Some(floats[n - 2]),
                rx_dbm: Some(floats[n - 1]),
            });
            continue;
        }
        // Key-value line format: "Interface: Te0/0/0 ... Tx Power: -2.1 dBm Rx Power: -23.5 dBm"
        if low.contains("rx") && low.contains("dbm") {
            let iface = cols
                .iter()
                .find(|c| c.contains('/'))
                .map_or("unknown", |s| s.trim_end_matches(':'))
                .to_string();
            let tx_dbm = low
                .find("tx")
                .and_then(|i| parse_first_f64_after(&line[i + 2..]));
            let rx_dbm = low
                .find("rx")
                .and_then(|i| parse_first_f64_after(&line[i + 2..]));
            if tx_dbm.is_some() || rx_dbm.is_some() {
                rows.push(OpticalDomRow {
                    interface: iface,
                    tx_dbm,
                    rx_dbm,
                });
            }
        }
    }
    rows
}

fn cisco_asr_parse(args: &Value, model: &str) -> Result<Value> {
    let (combined, by_cmd) = collect_raw_inputs(args);
    if combined.trim().is_empty() {
        return Err(ToolError::BadArguments(
            "mode=\"parse\" requer `output` ou `outputs` não vazios".into(),
        ));
    }

    let qfp_util = parse_qfp_utilization(pick_section(&by_cmd, "datapath utilization", &combined));
    let qfp_drops = parse_qfp_drops(pick_section(&by_cmd, "statistics drop", &combined));
    let throughput = parse_throughput_level(pick_section(&by_cmd, "throughput", &combined));
    let copp = parse_copp(pick_section(&by_cmd, "control-plane", &combined));
    let bng_nat = parse_bng_nat(&combined);
    let bgp_peers = parse_bgp_summary(pick_section(&by_cmd, "bgp", &combined));
    let ospf_nbrs = parse_ospf_neighbors(pick_section(&by_cmd, "ospf", &combined));
    let dom_rows = parse_optical_dom(pick_section(&by_cmd, "transceiver", &combined));

    let mut findings = Vec::new();

    // QFP utilization checks
    if let Some(load) = qfp_util
        .processing_5m_pct
        .or(qfp_util.processing_1m_pct)
        .or(qfp_util.processing_5s_pct)
    {
        if load >= 90.0 {
            findings.push(Finding::new(
                "critical",
                "qfp_utilization",
                format!("QFP Processing Load crítico ({load:.1}%) — risco de packet loss no datapath"),
                format!("5s={:?}%, 1m={:?}%, 5m={:?}%", qfp_util.processing_5s_pct, qfp_util.processing_1m_pct, qfp_util.processing_5m_pct),
                "Inspecionar `show platform hardware qfp active feature nat/qos` e distribuir tráfego ou upgrade de ESP.",
            ));
        } else if load >= 75.0 {
            findings.push(Finding::new(
                "warning",
                "qfp_utilization",
                format!("QFP Processing Load elevado ({load:.1}%)"),
                format!("5s={:?}%, 1m={:?}%, 5m={:?}%", qfp_util.processing_5s_pct, qfp_util.processing_1m_pct, qfp_util.processing_5m_pct),
                "Monitorar tendência em horário de pico e revisar features pesadas no QFP (CGNAT/ACL/QoS).",
            ));
        } else {
            findings.push(Finding::new(
                "ok",
                "qfp_utilization",
                format!("QFP Processing Load saudável ({load:.1}%)"),
                format!(
                    "5s={:?}%, 1m={:?}%, 5m={:?}%",
                    qfp_util.processing_5s_pct,
                    qfp_util.processing_1m_pct,
                    qfp_util.processing_5m_pct
                ),
                "Sem ação imediata.",
            ));
        }
    }

    // QFP drops checks
    if !qfp_drops.critical_drops.is_empty() {
        let detail = qfp_drops
            .critical_drops
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        findings.push(Finding::new(
            "critical",
            "qfp_drops",
            "Drops críticos não-nulos no datapath QFP",
            detail,
            "Investigar causa específica (TailDrop -> QoS/congestionamento; NatNoPort -> exaustão de pool CGNAT/PBA; Ipv4NoRoute -> blackhole).",
        ));
    } else if !qfp_drops.benign_drops.is_empty() {
        findings.push(Finding::new(
            "ok",
            "qfp_drops",
            "Apenas drops benignos de controle L2/ARP/ND no QFP",
            format!("{} contadores benignos", qfp_drops.benign_drops.len()),
            "Ignorar drops esperados de ARP/ND/L2MC.",
        ));
    }

    // Throughput license cap
    if throughput.capped {
        let lbl = throughput.level_label.as_deref().unwrap_or("capped");
        findings.push(Finding::new(
            "warning",
            "throughput_license",
            format!("Licença de throughput limitada detectada ({lbl})"),
            format!("level_mbps={:?}", throughput.level_mbps),
            "Se o tráfego agregado estiver perto do teto da licença, validar `show platform hardware throughput level` e planejar upgrade de licença.",
        ));
    }

    // CoPP drops
    for c in &copp {
        if c.violated_pkts > 0 || c.dropped_pkts > 0 {
            findings.push(Finding::new(
                "warning",
                "copp",
                format!("CoPP descartando pacotes na classe `{}`", c.class_name),
                format!("conformed={}, exceeded={}, violated={}, dropped={}", c.conformed_pkts, c.exceeded_pkts, c.violated_pkts, c.dropped_pkts),
                "Se a classe for BGP/OSPF/SSH/RADIUS, ajustar CIR/BC do CoPP para não derrubar sessões sob carga.",
            ));
        }
    }

    // NAT misses
    if let Some(miss_pct) = bng_nat.nat_miss_ratio_pct {
        if miss_pct >= 10.0 || bng_nat.nat_pool_exhausted {
            findings.push(Finding::new(
                "critical",
                "cgnat",
                format!("CGNAT/NAT com taxa de miss elevada ({miss_pct:.2}%)"),
                format!(
                    "hits={:?}, misses={:?}",
                    bng_nat.nat_hits, bng_nat.nat_misses
                ),
                "Verificar alocação de blocos PBA, pool de IPs públicos e `NatNoPort` no QFP.",
            ));
        }
    }

    // BGP peers
    for p in &bgp_peers {
        let st_low = p.state_or_pfx.to_ascii_lowercase();
        let non_estab = [
            "idle",
            "active",
            "connect",
            "opensent",
            "openconfirm",
            "admin",
        ]
        .iter()
        .any(|k| st_low.contains(k));
        if non_estab {
            findings.push(Finding::new(
                "critical",
                "bgp",
                format!(
                    "Peer BGP {} (AS {}) não estabelecido: {}",
                    p.neighbor, p.remote_as, p.state_or_pfx
                ),
                format!("Up/Down={}, InQ={}, OutQ={}", p.up_down, p.in_q, p.out_q),
                "Checar conectividade TCP/179, timers, MD5, ACL e `show ip bgp neighbors <ip>`.",
            ));
        } else if p.in_q > 100 || p.out_q > 100 {
            findings.push(Finding::new(
                "warning",
                "bgp",
                format!("Fila BGP elevada no peer {}", p.neighbor),
                format!("InQ={}, OutQ={}", p.in_q, p.out_q),
                "Verificar saturação de CPU/TCP ou convergência em curso.",
            ));
        }
    }

    // OSPF neighbors
    for o in &ospf_nbrs {
        let up = o.state.to_ascii_uppercase();
        if !up.starts_with("FULL") && !up.starts_with("2WAY") {
            findings.push(Finding::new(
                "critical",
                "ospf",
                format!(
                    "Vizinho OSPF {} em estado anômalo ({}) na {}",
                    o.neighbor_id, o.state, o.interface
                ),
                format!("state={}", o.state),
                "Validar MTU nas duas pontas (`ip mtu`), hello/dead timers e autenticação.",
            ));
        }
    }

    // Optical DOM
    for d in &dom_rows {
        let rx_bad = d.rx_dbm.is_some_and(|v| v < -20.0 || v > 1.0);
        let tx_bad = d.tx_dbm.is_some_and(|v| v < -20.0 || v > 1.0);
        if rx_bad || tx_bad {
            findings.push(Finding::new(
                "critical",
                "optical_dom",
                format!("Sinal óptico fora da faixa na interface {}", d.interface),
                format!("TX={:?} dBm, RX={:?} dBm", d.tx_dbm, d.rx_dbm),
                "Inspecionar limpeza de cordões, atenuação/dobra de fibra e módulo SFP/XFP/QSFP.",
            ));
        }
    }

    Ok(json!({
        "action": "cisco_asr_audit",
        "mode": "parse",
        "family": cisco_family(model),
        "metrics": {
            "qfp_utilization": qfp_util,
            "qfp_drops": qfp_drops,
            "throughput_license": throughput,
            "copp": copp,
            "bng_nat": bng_nat,
            "bgp_peers": bgp_peers,
            "ospf_neighbors": ospf_nbrs,
            "optical_dom": dom_rows,
        },
        "findings": findings,
    }))
}

// ============================================================================
// 2. Datacom (DmOS & DmSwitch)
// ============================================================================

fn detect_datacom_platform(model: &str, text: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    let t = text.to_ascii_lowercase();
    if m.contains("dmswitch")
        || m.contains("dm2104")
        || m.contains("dm3000")
        || m.contains("dm4000")
        || m.contains("dm4100")
        || t.contains("switchport native vlan")
        || t.contains("set-member untagged")
        || t.contains("% 25: invalid input")
        || t.contains("copy running-config startup-config")
    {
        "dmswitch"
    } else {
        "dmos"
    }
}

fn datacom_commands(model: &str) -> Value {
    let platform = detect_datacom_platform(model, "");
    let commands: Vec<&'static str> = if platform == "dmswitch" {
        vec![
            "paginate false",
            "show system",
            "show version",
            "show interfaces status",
            "show interfaces counters errors",
            "show vlan",
            "show mac-address-table",
            "show eaps",
            "show spanning-tree",
            "show transceiver",
            "show running-config",
        ]
    } else {
        vec![
            "paginate false",
            "show system",
            "show chassis",
            "show interface status",
            "show interface counters",
            "show interface transceiver",
            "show dot1q",
            "show erps",
            "show eaps",
            "show mac-address-table",
            "show bgp ipv4 unicast summary",
            "show ospf neighbor",
            "show running-config",
        ]
    };

    json!({
        "action": "datacom_audit",
        "mode": "commands",
        "platform": platform,
        "read_only": true,
        "commands": commands,
        "notes": [
            "Em DmSwitch, NÃO envie `?` para autocomplete dentro do lote — ele interpreta ou polui o prompt.",
            "Em DmOS use `commit confirmed 120`; em DmSwitch alterações entram direto na running-config e exigem `copy running-config startup-config`."
        ]
    })
}

fn datacom_plan_change(args: &Value, model: &str) -> Value {
    let platform = detect_datacom_platform(model, "");
    let revert_secs = args
        .get("revert_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(30, 1800);

    if platform == "dmswitch" {
        json!({
            "action": "datacom_audit",
            "mode": "plan_change",
            "platform": "dmswitch",
            "warning": "DmSwitch NÃO tem `commit confirmed`: erros em porta de gerência ou VLAN/PVID derrubam imediatamente.",
            "steps": [
                "1. Backup prévio via `net_backup` (`show running-config`).",
                "2. Conferir interface de uplink/gerência e VLAN atual (`show vlan`, `show mac-address-table`).",
                "3. Em portas untagged/acesso, configurar os DOIS parâmetros juntos: `set-member untagged` na VLAN E `switchport native vlan <id>` (PVID) na interface!",
                "4. Provar funcionamento com `show mac-address-table` na VLAN esperada.",
                "5. Persistir: `copy running-config startup-config`."
            ],
            "rollback_commands": [
                "no switchport native vlan <id>",
                "reload (se não salvou startup-config e perdeu acesso sem OOB)"
            ]
        })
    } else {
        json!({
            "action": "datacom_audit",
            "mode": "plan_change",
            "platform": "dmos",
            "steps": [
                "1. Backup prévio via `net_backup`.",
                "2. Entrar em configuração (`config`) e validar sintaxe com `commit check`.",
                format!("3. Aplicar com rollback automático: `commit confirmed {revert_secs}`"),
                "4. Validar ERPS/EAPS (`show erps`), MACs (`show mac-address-table`) e BGP/OSPF.",
                "5. Confirmar definitivamente: `commit`."
            ],
            "rollback_commands": [
                "rollback <id>",
                "show configuration commit list"
            ]
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct RingStatus {
    ring_name: String,
    protocol: String,
    role: String,
    state: String,
}

fn parse_datacom_rings(text: &str) -> Vec<RingStatus> {
    let mut rings = Vec::new();
    let mut cur_name = String::new();
    let mut cur_proto = "ERPS".to_string();
    let mut cur_role = String::new();
    let mut cur_state = String::new();

    let flush = |rings: &mut Vec<RingStatus>,
                 name: &mut String,
                 proto: &str,
                 role: &mut String,
                 state: &mut String| {
        if !name.is_empty() || !state.is_empty() {
            rings.push(RingStatus {
                ring_name: if name.is_empty() {
                    "default".into()
                } else {
                    name.clone()
                },
                protocol: proto.to_string(),
                role: if role.is_empty() {
                    "unknown".into()
                } else {
                    role.clone()
                },
                state: if state.is_empty() {
                    "unknown".into()
                } else {
                    state.clone()
                },
            });
            name.clear();
            role.clear();
            state.clear();
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if low.contains("eaps") && (low.contains("domain") || low.contains("ring")) {
            flush(
                &mut rings,
                &mut cur_name,
                &cur_proto,
                &mut cur_role,
                &mut cur_state,
            );
            cur_proto = "EAPS".to_string();
            cur_name = line.split(':').nth(1).unwrap_or(line).trim().to_string();
        } else if (low.starts_with("ring ") || low.contains("erps ring")) && line.contains(':') {
            flush(
                &mut rings,
                &mut cur_name,
                &cur_proto,
                &mut cur_role,
                &mut cur_state,
            );
            cur_proto = "ERPS".to_string();
            cur_name = line.split(':').nth(1).unwrap_or(line).trim().to_string();
        } else if low.starts_with("role")
            || low.contains("rpl owner")
            || low.contains("master")
            || low.contains("transit")
        {
            if low.contains("master") {
                cur_role = "Master".into();
            } else if low.contains("transit") {
                cur_role = "Transit".into();
            } else if low.contains("owner") {
                cur_role = "RPL-Owner".into();
            } else if let Some(v) = line.split(':').nth(1) {
                cur_role = v.trim().to_string();
            }
        } else if low.starts_with("state") || low.contains("ring state") || low.contains("status:")
        {
            if let Some(v) = line.split(':').nth(1) {
                cur_state = v.trim().to_string();
            }
        }
    }
    flush(
        &mut rings,
        &mut cur_name,
        &cur_proto,
        &mut cur_role,
        &mut cur_state,
    );
    rings
}

#[derive(Debug, Clone, Serialize)]
struct PvidMismatch {
    interface: String,
    native_pvid: u16,
    untagged_vlans: Vec<u16>,
}

fn expand_port_spec(spec: &str) -> Vec<String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

type PvidAuditResult = (
    BTreeMap<String, u16>,
    BTreeMap<String, Vec<u16>>,
    Vec<PvidMismatch>,
);

fn parse_dmswitch_pvid_mismatches(text: &str) -> PvidAuditResult {
    let mut native_pvid: BTreeMap<String, u16> = BTreeMap::new();
    let mut untagged_by_port: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    let mut cur_iface: Option<String> = None;
    let mut cur_vlan: Option<u16> = None;

    for raw in text.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if let Some(rest) = low.strip_prefix("interface ") {
            cur_iface = Some(rest.trim().to_string());
            cur_vlan = None;
            continue;
        }
        if let Some(rest) = low.strip_prefix("vlan ") {
            if let Some(vid) = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u16>().ok())
            {
                cur_vlan = Some(vid);
                cur_iface = None;
                continue;
            }
        }
        if low == "exit" || low == "!" {
            cur_iface = None;
            cur_vlan = None;
            continue;
        }
        if let Some(ref iface) = cur_iface {
            if let Some(rest) = low.strip_prefix("switchport native vlan ") {
                if let Some(vid) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u16>().ok())
                {
                    native_pvid.insert(iface.clone(), vid);
                }
            }
        }
        if let Some(vid) = cur_vlan {
            if let Some(rest) = low.strip_prefix("set-member untagged ") {
                for port in expand_port_spec(rest) {
                    let entry = untagged_by_port.entry(port).or_default();
                    if !entry.contains(&vid) {
                        entry.push(vid);
                    }
                }
            }
        }
    }

    let mut mismatches = Vec::new();
    for (port, vlans) in &untagged_by_port {
        let pvid = native_pvid.get(port).copied().unwrap_or(1);
        if !vlans.is_empty() && !vlans.contains(&pvid) {
            mismatches.push(PvidMismatch {
                interface: port.clone(),
                native_pvid: pvid,
                untagged_vlans: vlans.clone(),
            });
        }
    }
    (native_pvid, untagged_by_port, mismatches)
}

fn parse_interface_errors(text: &str) -> Vec<(String, u64)> {
    let mut errs = Vec::new();
    for raw in text.lines() {
        let cols: Vec<&str> = raw.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        let low0 = cols[0].to_ascii_lowercase();
        if (low0.starts_with("eth")
            || low0.starts_with("gig")
            || low0.starts_with("ten")
            || low0.starts_with("hundred")
            || low0.contains('/'))
            && raw.to_ascii_lowercase().contains("err")
        {
            if let Some(val) = cols
                .iter()
                .skip(1)
                .filter_map(|c| parse_u64_token(c))
                .find(|&v| v > 0)
            {
                errs.push((cols[0].to_string(), val));
            }
        }
    }
    errs
}

fn datacom_parse(args: &Value, model: &str) -> Result<Value> {
    let (combined, _by_cmd) = collect_raw_inputs(args);
    if combined.trim().is_empty() {
        return Err(ToolError::BadArguments(
            "mode=\"parse\" requer `output` ou `outputs` não vazios".into(),
        ));
    }

    let platform = detect_datacom_platform(model, &combined);
    let rings = parse_datacom_rings(&combined);
    let (native_pvid, untagged_by_port, pvid_mismatches) =
        parse_dmswitch_pvid_mismatches(&combined);
    let dom_rows = parse_optical_dom(&combined);
    let if_errors = parse_interface_errors(&combined);
    let bgp_peers = parse_bgp_summary(&combined);
    let ospf_nbrs = parse_ospf_neighbors(&combined);

    let mut findings = Vec::new();

    for r in &rings {
        let st = r.state.to_ascii_lowercase();
        if st.contains("fail")
            || st.contains("protect")
            || st.contains("init")
            || st.contains("idle")
        {
            findings.push(Finding::new(
                "critical",
                "ring_eaps_erps",
                format!("Anel {} `{}` não está Complete (estado: {})", r.protocol, r.ring_name, r.state),
                format!("role={}, state={}", r.role, r.state),
                "Verificar link/transceiver na porta secundária do anel e checar se há bloqueio de loop ou flapping.",
            ));
        } else {
            findings.push(Finding::new(
                "ok",
                "ring_eaps_erps",
                format!(
                    "Anel {} `{}` saudável ({})",
                    r.protocol, r.ring_name, r.state
                ),
                format!("role={}", r.role),
                "Sem ação.",
            ));
        }
    }

    for mm in &pvid_mismatches {
        findings.push(Finding::new(
            "critical",
            "dmswitch_pvid_mismatch",
            format!(
                "Divergência PVID no DmSwitch na porta `{}`: native vlan = {}, untagged = {:?}",
                mm.interface, mm.native_pvid, mm.untagged_vlans
            ),
            format!("switchport native vlan {} != set-member untagged {:?}", mm.native_pvid, mm.untagged_vlans),
            format!("Configurar `switchport native vlan {}` dentro de `interface {}` e confirmar com `show mac-address-table`.", mm.untagged_vlans[0], mm.interface),
        ));
    }

    for (iface, cnt) in &if_errors {
        findings.push(Finding::new(
            "warning",
            "interface_errors",
            format!("Erros/CRC não-nulos na interface {iface}"),
            format!("count={cnt}"),
            "Validar sinal óptico DOM, duplex/velocidade e cordão físico.",
        ));
    }

    for d in &dom_rows {
        let rx_bad = d.rx_dbm.is_some_and(|v| v < -20.0 || v > 1.0);
        if rx_bad {
            findings.push(Finding::new(
                "critical",
                "optical_dom",
                format!("Sinal RX fora da faixa na interface {}", d.interface),
                format!("TX={:?} dBm, RX={:?} dBm", d.tx_dbm, d.rx_dbm),
                "Checar atenuação óptica e SFP.",
            ));
        }
    }

    if combined
        .to_ascii_lowercase()
        .contains("% 25: invalid input")
    {
        findings.push(Finding::new(
            "warning",
            "cli_syntax",
            "Erro clássico `% 25: Invalid input` de DmSwitch detectado na saída",
            "% 25: Invalid input",
            "Confirmar sintaxe específica de DmSwitch (não usar comandos DmOS como `commit` ou `dot1q vlan`).",
        ));
    }

    Ok(json!({
        "action": "datacom_audit",
        "mode": "parse",
        "platform": platform,
        "metrics": {
            "rings": rings,
            "native_pvid": native_pvid,
            "untagged_by_port": untagged_by_port,
            "pvid_mismatches": pvid_mismatches,
            "interface_errors": if_errors,
            "optical_dom": dom_rows,
            "bgp_peers": bgp_peers,
            "ospf_neighbors": ospf_nbrs,
        },
        "findings": findings,
    }))
}

// ============================================================================
// 3. Ubiquiti RF (airOS / airMAX / LTU / airFiber / UniFi)
// ============================================================================

fn ubiquiti_commands(model: &str) -> Value {
    let m = model.to_ascii_lowercase();
    let is_unifi = m.contains("unifi");
    let commands: Vec<&'static str> = if is_unifi {
        vec![
            "mca-cli-op info",
            "iwconfig",
            "cat /proc/uptime",
            "ip -br addr",
        ]
    } else {
        vec![
            "mca-status",
            "wstalist",
            "cat /tmp/system.cfg | grep -E 'radio|wireless|netconf'",
            "athstats",
        ]
    };

    json!({
        "action": "ubiquiti_rf_audit",
        "mode": "commands",
        "model": if model.is_empty() { "airos/airmax_ac" } else { model },
        "read_only": true,
        "ssh_commands": commands,
        "snmp_preset": "net_snmp(preset=\"ubiquiti_rf\")",
        "uisp_api_hint": "Use `net_api` em UISP (`/nms/api/v2.1/devices?withInterfaces=true`) ou `status.cgi` / `sta.cgi` no rádio.",
    })
}

fn ubiquiti_plan_change(model: &str) -> Value {
    json!({
        "action": "ubiquiti_rf_audit",
        "mode": "plan_change",
        "model": if model.is_empty() { "airos/airmax_ac" } else { model },
        "steps": [
            "1. Fazer backup de `/tmp/system.cfg` antes de alterar frequência, largura de canal (chanbw) ou SSID.",
            "2. Começar sempre pela ponta REMOTA (Station) antes da ponta LOCAL (AP), evitando perder o enlace.",
            "3. Na interface web airOS/UISP, usar **Test Mode** (revert automático em ~180s caso o link não volte).",
            "4. Evitar canais DFS sem verificação de radar local; após aplicar, validar `chain0`/`chain1`, CINR, CCQ e Airtime.",
            "5. Só clicar em Apply definitivo / `cfgmtd -w -p /etc/` após confirmar enlace estável."
        ],
    })
}

#[derive(Debug, Default, Serialize)]
struct RfMetrics {
    signal_dbm: Option<f64>,
    noise_floor_dbm: Option<f64>,
    snr_db: Option<f64>,
    cinr_db: Option<f64>,
    chain0_dbm: Option<f64>,
    chain1_dbm: Option<f64>,
    chain_imbalance_db: Option<f64>,
    ccq_pct: Option<f64>,
    airtime_pct: Option<f64>,
    dl_capacity_kbps: Option<f64>,
    ul_capacity_kbps: Option<f64>,
    tx_rate_mbps: Option<f64>,
    rx_rate_mbps: Option<f64>,
    distance_km: Option<f64>,
    freq_mhz: Option<f64>,
    chanbw_mhz: Option<f64>,
    tx_power_dbm: Option<f64>,
    uptime_secs: Option<u64>,
    fspl_db: Option<f64>,
    expected_rssi_dbm: Option<f64>,
    path_loss_delta_db: Option<f64>,
}

fn json_num(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(val) = v.get(*k) {
            if let Some(n) = val.as_f64() {
                return Some(n);
            }
            if let Some(s) = val.as_str() {
                if let Ok(n) = s.trim().parse::<f64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn parse_ubiquiti_json(val: &Value) -> RfMetrics {
    let mut m = RfMetrics::default();
    let w = val.get("wireless").unwrap_or(val);

    m.signal_dbm = json_num(w, &["signal", "rssi"]);
    m.noise_floor_dbm = json_num(w, &["noisef", "noise", "noiseFloor"]);
    m.cinr_db = json_num(w, &["cinr"]);
    m.ccq_pct = json_num(w, &["ccq"]).map(|c| if c > 100.0 { c / 10.0 } else { c });
    m.tx_rate_mbps = json_num(w, &["txrate", "txRate"]);
    m.rx_rate_mbps = json_num(w, &["rxrate", "rxRate"]);
    m.freq_mhz = json_num(w, &["frequency", "freq"]);
    m.chanbw_mhz = json_num(w, &["chanbw", "channelWidth"]);
    m.tx_power_dbm = json_num(w, &["txpower", "txPower"]);
    m.uptime_secs = w.get("uptime").and_then(Value::as_u64).or_else(|| {
        val.get("host")
            .and_then(|h| h.get("uptime"))
            .and_then(Value::as_u64)
    });

    if let Some(d_meters) = json_num(w, &["distance"]) {
        m.distance_km = Some(if d_meters > 100.0 {
            d_meters / 1000.0
        } else {
            d_meters
        });
    }
    if let Some(air) = w.get("airtime").and_then(Value::as_f64) {
        m.airtime_pct = Some(air);
    } else if let Some(polling) = w.get("polling") {
        m.airtime_pct = json_num(polling, &["airtime", "use"]);
        m.dl_capacity_kbps = json_num(polling, &["dl_capacity", "tx_capacity"]);
        m.ul_capacity_kbps = json_num(polling, &["ul_capacity", "rx_capacity"]);
    }
    m.dl_capacity_kbps = m.dl_capacity_kbps.or_else(|| json_num(w, &["dl_capacity"]));
    m.ul_capacity_kbps = m.ul_capacity_kbps.or_else(|| json_num(w, &["ul_capacity"]));

    if let Some(arr) = w.get("chainrssi").and_then(Value::as_array) {
        let mut vals = Vec::new();
        for item in arr {
            if let Some(v) = item.as_f64() {
                // airOS JSON sometimes reports positive RSSI above noise floor (e.g. 38 -> -54 dBm if noise=-92), or direct negative dBm
                let dbm = if v >= 0.0 {
                    if let Some(nf) = m.noise_floor_dbm {
                        nf + v
                    } else {
                        -v
                    }
                } else {
                    v
                };
                vals.push(dbm);
            }
        }
        if vals.len() >= 2 {
            m.chain0_dbm = Some(vals[0]);
            m.chain1_dbm = Some(vals[1]);
        }
    } else {
        m.chain0_dbm = json_num(w, &["chain0"]);
        m.chain1_dbm = json_num(w, &["chain1"]);
    }
    m
}

fn first_num(raw: &str) -> Option<f64> {
    raw.split([' ', '\t', '%'])
        .find(|t| !t.is_empty())
        .and_then(|t| {
            t.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                .parse::<f64>()
                .ok()
        })
}

fn assign_mca_kv(m: &mut RfMetrics, key: &str, val: &str) {
    let kl = key.trim().to_ascii_lowercase();
    let vt = val.trim();
    match kl.as_str() {
        "signal" | "signal_strength" | "rssi" => {
            if let Some(v) = first_num(vt) {
                m.signal_dbm = Some(v);
            }
        }
        "noise" | "noisef" | "noise_floor" => {
            if let Some(v) = first_num(vt) {
                m.noise_floor_dbm = Some(v);
            }
        }
        "cinr" => {
            if let Some(v) = first_num(vt) {
                m.cinr_db = Some(v);
            }
        }
        "ccq" => {
            if let Some(v) = first_num(vt) {
                m.ccq_pct = Some(if v > 100.0 { v / 10.0 } else { v });
            }
        }
        "wlantxrate" | "txrate" | "tx_rate" => {
            if let Some(v) = first_num(vt) {
                m.tx_rate_mbps = Some(v);
            }
        }
        "wlanrxrate" | "rxrate" | "rx_rate" => {
            if let Some(v) = first_num(vt) {
                m.rx_rate_mbps = Some(v);
            }
        }
        "freq" | "frequency" => {
            if let Some(v) = first_num(vt) {
                m.freq_mhz = Some(v);
            }
        }
        "chanbw" | "channel_width" => {
            if let Some(v) = first_num(vt) {
                m.chanbw_mhz = Some(v);
            }
        }
        "txpower" | "tx_power" | "tx" => {
            if let Some(v) = first_num(vt) {
                m.tx_power_dbm = Some(v);
            }
        }
        "uptime" => {
            if let Some(v) = first_num(vt) {
                m.uptime_secs = Some(v.max(0.0) as u64);
            }
        }
        "airtime" => {
            if let Some(v) = first_num(vt) {
                m.airtime_pct = Some(v);
            }
        }
        "distance" => {
            if let Some(v) = first_num(vt) {
                m.distance_km = Some(if v > 100.0 { v / 1000.0 } else { v });
            }
        }
        "chain0" | "chain_0" => {
            if let Some(v) = first_num(vt) {
                m.chain0_dbm = Some(v);
            }
        }
        "chain1" | "chain_1" => {
            if let Some(v) = first_num(vt) {
                m.chain1_dbm = Some(v);
            }
        }
        "chains" => {
            let parts: Vec<f64> = vt
                .split(['/', ' '])
                .filter_map(|p| p.trim().parse::<f64>().ok())
                .collect();
            if parts.len() >= 2 {
                m.chain0_dbm = Some(parts[0]);
                m.chain1_dbm = Some(parts[1]);
            }
        }
        _ => {}
    }
}

fn parse_mca_status(text: &str) -> RfMetrics {
    let mut m = RfMetrics::default();
    for raw in text.lines() {
        for chunk in raw.split([',', ';']) {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.matches('=').count() > 1 {
                // Space-separated `key=val` tokens (e.g. `mca-status: signal=-54 chain0=-54 chain1=-65`).
                for tok in trimmed.split_whitespace() {
                    if let Some((k, v)) = tok.split_once('=') {
                        assign_mca_kv(&mut m, k, v);
                    }
                }
            } else if let Some((k, v)) = trimmed.split_once('=') {
                // Single `key=val` chunk; strip optional prefix like `mca-status: signal`.
                let key = k.rsplit(':').next().unwrap_or(k);
                assign_mca_kv(&mut m, key, v);
            } else if let Some((k, v)) = trimmed.split_once(':') {
                assign_mca_kv(&mut m, k, v);
            }
        }
    }
    m
}

fn ubiquiti_parse(args: &Value, model: &str) -> Result<Value> {
    let (combined, _) = collect_raw_inputs(args);
    let mut m = if combined.trim().starts_with('{') || combined.trim().starts_with('[') {
        let parsed: Value = serde_json::from_str(combined.trim())
            .map_err(|e| ToolError::BadArguments(format!("JSON airOS inválido: {e}")))?;
        let target = parsed.as_array().and_then(|a| a.first()).unwrap_or(&parsed);
        parse_ubiquiti_json(target)
    } else if !combined.trim().is_empty() {
        parse_mca_status(&combined)
    } else {
        RfMetrics::default()
    };

    // Allow explicit argument overrides for RF engineering math (FSPL)
    if let Some(f) = args.get("freq_mhz").and_then(Value::as_f64) {
        m.freq_mhz = Some(f);
    }
    if let Some(d) = args.get("distance_km").and_then(Value::as_f64) {
        m.distance_km = Some(d);
    }
    if let Some(p) = args.get("tx_power_dbm").and_then(Value::as_f64) {
        m.tx_power_dbm = Some(p);
    }

    if let (Some(sig), Some(nf)) = (m.signal_dbm, m.noise_floor_dbm) {
        m.snr_db = Some((sig - nf).round());
    }
    if let (Some(c0), Some(c1)) = (m.chain0_dbm, m.chain1_dbm) {
        m.chain_imbalance_db = Some((c0 - c1).abs());
    }

    // FSPL = 32.44 + 20*log10(d_km) + 20*log10(f_mhz)
    if let (Some(d_km), Some(f_mhz)) = (
        m.distance_km.filter(|&d| d > 0.0),
        m.freq_mhz.filter(|&f| f > 0.0),
    ) {
        let fspl = 32.44 + 20.0 * d_km.log10() + 20.0 * f_mhz.log10();
        let fspl_rounded = (fspl * 100.0).round() / 100.0;
        m.fspl_db = Some(fspl_rounded);

        let tx_gain = args
            .get("tx_antenna_gain_dbi")
            .or_else(|| args.get("antenna_gain_dbi"))
            .and_then(Value::as_f64);
        let rx_gain = args
            .get("rx_antenna_gain_dbi")
            .or_else(|| args.get("antenna_gain_dbi"))
            .and_then(Value::as_f64);

        if let (Some(tx_p), Some(g_tx), Some(g_rx)) = (m.tx_power_dbm, tx_gain, rx_gain) {
            let expected = tx_p + g_tx + g_rx - fspl;
            m.expected_rssi_dbm = Some((expected * 100.0).round() / 100.0);
            if let Some(measured) = m.signal_dbm {
                m.path_loss_delta_db = Some(((expected - measured) * 100.0).round() / 100.0);
            }
        }
    }

    let mut findings = Vec::new();
    if let Some(imb) = m.chain_imbalance_db {
        if imb >= 8.0 {
            findings.push(Finding::new(
                "critical",
                "chain_imbalance",
                format!("Desbalanceamento crítico entre chains ({imb:.1} dB)"),
                format!("chain0={:?} dBm, chain1={:?} dBm", m.chain0_dbm, m.chain1_dbm),
                "Inspecionar alinhamento fino (azimute/elevação), obstrução parcial de Fresnel, pigtail ou conector SMA.",
            ));
        } else if imb >= 5.0 {
            findings.push(Finding::new(
                "warning",
                "chain_imbalance",
                format!("Desbalanceamento entre chains ({imb:.1} dB >= 5 dB)"),
                format!(
                    "chain0={:?} dBm, chain1={:?} dBm",
                    m.chain0_dbm, m.chain1_dbm
                ),
                "Planejar realinhamento ou revisão de polarização/pigtail.",
            ));
        } else {
            findings.push(Finding::new(
                "ok",
                "chain_imbalance",
                format!("Chains bem balanceadas (delta {imb:.1} dB)"),
                format!(
                    "chain0={:?} dBm, chain1={:?} dBm",
                    m.chain0_dbm, m.chain1_dbm
                ),
                "Sem ação.",
            ));
        }
    }

    if let Some(snr) = m.snr_db {
        if snr < 15.0 {
            findings.push(Finding::new(
                "critical",
                "snr",
                format!("SNR crítico ({snr:.1} dB)"),
                format!("signal={:?} dBm, noise={:?} dBm", m.signal_dbm, m.noise_floor_dbm),
                "Trocar para canal mais limpo ou reduzir largura de canal (ex.: 40 -> 20 MHz) para ganhar 3 dB de margem.",
            ));
        } else if snr < 20.0 {
            findings.push(Finding::new(
                "warning",
                "snr",
                format!("Margem de SNR moderada/baixa ({snr:.1} dB)"),
                format!(
                    "signal={:?} dBm, noise={:?} dBm",
                    m.signal_dbm, m.noise_floor_dbm
                ),
                "Avaliar espectro (AirView) e evitar modulações altas instáveis.",
            ));
        }
    }

    if let Some(cinr) = m.cinr_db {
        if cinr < 15.0 {
            findings.push(Finding::new(
                "critical",
                "cinr",
                format!("CINR crítico ({cinr:.1} dB) — interferência co-canal ou ruído"),
                format!("cinr={cinr} dB"),
                "Mudar frequência/canal e checar blindagem lateral.",
            ));
        } else if cinr < 22.0 {
            findings.push(Finding::new(
                "warning",
                "cinr",
                format!("CINR em atenção ({cinr:.1} dB)"),
                format!("cinr={cinr} dB"),
                "Monitorar estabilidade de modulação.",
            ));
        }
    }

    if let Some(ccq) = m.ccq_pct {
        if ccq < 70.0 {
            findings.push(Finding::new(
                "critical",
                "ccq",
                format!("CCQ crítico ({ccq:.1}%) — alta retransmissão RF"),
                format!("ccq={ccq}%"),
                "Verificar interferência, Fresnel obstruído ou cliente distante degradando o setor.",
            ));
        } else if ccq < 85.0 {
            findings.push(Finding::new(
                "warning",
                "ccq",
                format!("CCQ abaixo do ideal ({ccq:.1}%)"),
                format!("ccq={ccq}%"),
                "Checar airTime e estabilidade de modulação.",
            ));
        }
    }

    if let Some(delta) = m.path_loss_delta_db {
        if delta >= 10.0 {
            findings.push(Finding::new(
                "critical",
                "fresnel_fspl",
                format!("Perda adicional de {delta:.2} dB acima do FSPL esperado — provável obstrução de Fresnel ou desalinhamento"),
                format!("FSPL={:?} dB, expected_rssi={:?} dBm, measured={:?} dBm", m.fspl_db, m.expected_rssi_dbm, m.signal_dbm),
                "Auditar visada/zona de Fresnel (árvores, prédios, curvatura) e alinhamento das antenas.",
            ));
        } else if delta >= 5.0 {
            findings.push(Finding::new(
                "warning",
                "fresnel_fspl",
                format!("Perda de {delta:.2} dB acima do FSPL teórico"),
                format!(
                    "FSPL={:?} dB, expected_rssi={:?} dBm, measured={:?} dBm",
                    m.fspl_db, m.expected_rssi_dbm, m.signal_dbm
                ),
                "Conferir perdas em cabo/conector e alinhamento.",
            ));
        }
    }

    if findings.is_empty() && m.signal_dbm.is_none() && m.fspl_db.is_none() {
        return Err(ToolError::BadArguments(
            "não foi possível extrair métricas RF de `output` (envie JSON status.cgi, `mca-status` ou parâmetros FSPL)".into(),
        ));
    }

    Ok(json!({
        "action": "ubiquiti_rf_audit",
        "mode": "parse",
        "model": if model.is_empty() { "airos" } else { model },
        "metrics": m,
        "findings": findings,
    }))
}

// ============================================================================
// 4. Linux SRE Audit
// ============================================================================

fn linux_sre_commands() -> Value {
    let composite = concat!(
        "echo '===SYS==='; hostname; uname -a; uptime; ",
        "echo '===PSI==='; head -n 5 /proc/pressure/cpu /proc/pressure/memory /proc/pressure/io 2>/dev/null || true; ",
        "echo '===MEM_DISK==='; free -m; df -h -P; ",
        "echo '===SYSTEMD_FAILED==='; systemctl --failed --no-pager --no-legend 2>/dev/null || true; ",
        "echo '===JOURNAL_CRIT==='; journalctl -p 3 -n 30 --no-pager 2>/dev/null || true; ",
        "echo '===IP_ADDR==='; ip -j addr; ",
        "echo '===IP_ROUTE==='; ip -j route; ",
        "echo '===IP_RULE==='; ip -j rule; ",
        "echo '===SOCKETS==='; ss -tulpen; ",
        "echo '===CONNTRACK==='; conntrack -S 2>/dev/null || true; ",
        "echo '===WIREGUARD==='; wg show all dump 2>/dev/null || true; ",
        "echo '===FRR==='; command -v vtysh >/dev/null 2>&1 && vtysh -c 'show ip bgp summary' || true"
    );

    json!({
        "action": "linux_sre_audit",
        "mode": "commands",
        "read_only": true,
        "composite_command": composite,
        "notes": [
            "Comando único read-only: coleta PSI, systemd, journal, ip -j (addr/route/rule), ss, conntrack, WireGuard e FRR em uma única chamada."
        ]
    })
}

fn linux_sre_plan_change(args: &Value) -> Value {
    let revert_secs = args
        .get("revert_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(90)
        .clamp(30, 600);

    json!({
        "action": "linux_sre_audit",
        "mode": "plan_change",
        "validation_commands": [
            "sshd -t",
            "nginx -t",
            "nft -c -f /etc/nftables.conf",
            "ifreload -a -s",
            "netplan try --timeout 90"
        ],
        "dead_mans_switch_wrapper": {
            "description": "Agenda rollback automático antes de editar firewall/nftables ou interfaces remotamente.",
            "arm": format!(
                "cp /etc/nftables.conf /root/nftables.conf.pre && systemd-run --unit=zerith-rollback --on-active={revert_secs}s /bin/sh -c 'cp /root/nftables.conf.pre /etc/nftables.conf && nft -f /etc/nftables.conf'"
            ),
            "disarm_after_verify": "systemctl stop zerith-rollback.timer zerith-rollback.service"
        }
    })
}

#[derive(Debug, Default, Serialize)]
struct PsiMetric {
    resource: String,
    some_avg10: Option<f64>,
    full_avg10: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
struct ConntrackStats {
    entries: Option<u64>,
    insert_failed: Option<u64>,
    drop: Option<u64>,
    early_drop: Option<u64>,
}

fn parse_kv_f64(line: &str, key: &str) -> Option<f64> {
    for tok in line.split_whitespace() {
        if let Some(rest) = tok.strip_prefix(&format!("{key}=")) {
            if let Ok(v) = rest.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_kv_u64(line: &str, key: &str) -> Option<u64> {
    for tok in line.split_whitespace() {
        if let Some(rest) = tok.strip_prefix(&format!("{key}=")) {
            if let Ok(v) = rest.parse::<u64>() {
                return Some(v);
            }
        }
    }
    None
}

fn linux_sre_parse(args: &Value) -> Result<Value> {
    let (combined, _by_cmd) = collect_raw_inputs(args);
    if combined.trim().is_empty() {
        return Err(ToolError::BadArguments(
            "mode=\"parse\" requer `output` ou `outputs` não vazios".into(),
        ));
    }

    let mut psi_list = Vec::new();
    let mut cur_psi_res = "unknown".to_string();
    let mut cur_psi = PsiMetric::default();
    let mut failed_units = Vec::new();
    let mut conntrack = ConntrackStats::default();
    let mut disk_alerts: Vec<(String, u64)> = Vec::new();
    let mut mem_used_pct: Option<f64> = None;
    let mut default_routes = Vec::new();
    let mut in_failed_section = false;

    for raw in combined.lines() {
        let line = raw.trim();
        let low = line.to_ascii_lowercase();
        if line.starts_with("===") {
            in_failed_section = low.contains("systemd_failed");
            continue;
        }
        if low.contains("/proc/pressure/cpu") {
            cur_psi_res = "cpu".into();
        } else if low.contains("/proc/pressure/memory") {
            cur_psi_res = "memory".into();
        } else if low.contains("/proc/pressure/io") {
            cur_psi_res = "io".into();
        } else if low.starts_with("some avg10=") {
            cur_psi = PsiMetric {
                resource: cur_psi_res.clone(),
                some_avg10: parse_kv_f64(line, "avg10"),
                full_avg10: None,
            };
            if cur_psi_res == "cpu" {
                psi_list.push(std::mem::take(&mut cur_psi));
            }
        } else if low.starts_with("full avg10=") {
            if cur_psi.resource.is_empty() {
                cur_psi.resource = cur_psi_res.clone();
            }
            cur_psi.full_avg10 = parse_kv_f64(line, "avg10");
            psi_list.push(std::mem::take(&mut cur_psi));
        }

        if in_failed_section && !line.is_empty() && !low.starts_with("0 loaded") {
            if let Some(unit) = line.split_whitespace().next() {
                if unit.contains('.') || line.contains("failed") {
                    failed_units.push(line.to_string());
                }
            }
        } else if low.contains(".service") && low.contains("failed") {
            failed_units.push(line.to_string());
        }

        if low.contains("entries=") || low.contains("insert_failed=") || low.contains("early_drop=")
        {
            if let Some(v) = parse_kv_u64(line, "entries") {
                conntrack.entries = Some(conntrack.entries.unwrap_or(0) + v);
            }
            if let Some(v) = parse_kv_u64(line, "insert_failed") {
                conntrack.insert_failed = Some(conntrack.insert_failed.unwrap_or(0) + v);
            }
            if let Some(v) = parse_kv_u64(line, "drop") {
                conntrack.drop = Some(conntrack.drop.unwrap_or(0) + v);
            }
            if let Some(v) = parse_kv_u64(line, "early_drop") {
                conntrack.early_drop = Some(conntrack.early_drop.unwrap_or(0) + v);
            }
        }

        if low.starts_with("mem:") {
            let nums: Vec<u64> = line
                .split_whitespace()
                .filter_map(parse_u64_token)
                .collect();
            if nums.len() >= 2 && nums[0] > 0 {
                // free -m: total used free shared buff/cache available
                let used = if nums.len() >= 6 {
                    nums[0].saturating_sub(nums[5])
                } else {
                    nums[1]
                };
                mem_used_pct = Some((used as f64 * 100.0) / nums[0] as f64);
            }
        }

        // df -h -P line: /dev/sda1  50G  46G  4G  92%  /
        for tok in line.split_whitespace() {
            if let Some(pct_str) = tok.strip_suffix('%') {
                if let Ok(pct) = pct_str.parse::<u64>() {
                    if pct >= 85 && line.contains('/') {
                        let mount = line.split_whitespace().last().unwrap_or("/").to_string();
                        disk_alerts.push((mount, pct));
                    }
                }
            }
        }

        // ip -j route inline JSON array
        if line.starts_with('[') && line.contains("\"dst\"") {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(line) {
                for r in arr {
                    if r.get("dst").and_then(Value::as_str) == Some("default") {
                        let gw = r.get("gateway").and_then(Value::as_str).unwrap_or("direct");
                        let dev = r.get("dev").and_then(Value::as_str).unwrap_or("-");
                        default_routes.push(format!("via {gw} dev {dev}"));
                    }
                }
            }
        }
    }

    let mut findings = Vec::new();
    for p in &psi_list {
        if p.full_avg10.is_some_and(|v| v >= 5.0) || p.some_avg10.is_some_and(|v| v >= 25.0) {
            findings.push(Finding::new(
                "critical",
                "psi",
                format!("Contenção severa de PSI em `{}`", p.resource),
                format!("some_avg10={:?}, full_avg10={:?}", p.some_avg10, p.full_avg10),
                "Investigar top processos (`ps aux --sort=-%cpu`), I/O wait (`iostat -xz`) ou pressão de memória.",
            ));
        } else if p.some_avg10.is_some_and(|v| v >= 10.0) {
            findings.push(Finding::new(
                "warning",
                "psi",
                format!("Pressão PSI elevada em `{}`", p.resource),
                format!(
                    "some_avg10={:?}, full_avg10={:?}",
                    p.some_avg10, p.full_avg10
                ),
                "Monitorar tendência.",
            ));
        }
    }

    if !failed_units.is_empty() {
        findings.push(Finding::new(
            "critical",
            "systemd",
            format!("{} unidade(s) systemd em estado failed", failed_units.len()),
            failed_units.join("; "),
            "Executar `systemctl status <unit>` e `journalctl -u <unit> -n 50`.",
        ));
    }

    if conntrack.drop.is_some_and(|d| d > 0)
        || conntrack.insert_failed.is_some_and(|d| d > 0)
        || conntrack.early_drop.is_some_and(|d| d > 0)
    {
        findings.push(Finding::new(
            "critical",
            "conntrack",
            "Drops / falhas de inserção na tabela conntrack",
            format!("entries={:?}, drop={:?}, insert_failed={:?}, early_drop={:?}", conntrack.entries, conntrack.drop, conntrack.insert_failed, conntrack.early_drop),
            "Aumentar `net.netfilter.nf_conntrack_max` e `nf_conntrack_buckets`, ou usar `notrack` em tráfego transit/BGP.",
        ));
    }

    for (mount, pct) in &disk_alerts {
        let sev = if *pct >= 95 { "critical" } else { "warning" };
        findings.push(Finding::new(
            sev,
            "disk",
            format!("Disco `{mount}` em {pct}% de uso"),
            format!("{mount}={pct}%"),
            "Limpar logs/dumps antigos ou expandir volume antes de atingir 100%.",
        ));
    }

    if let Some(mp) = mem_used_pct {
        if mp >= 90.0 {
            findings.push(Finding::new(
                "critical",
                "memory",
                format!("Uso de RAM crítico ({mp:.1}%)"),
                format!("used_pct={mp:.1}%"),
                "Checar processos consumidores e risco de OOM killer.",
            ));
        } else if mp >= 80.0 {
            findings.push(Finding::new(
                "warning",
                "memory",
                format!("Uso de RAM elevado ({mp:.1}%)"),
                format!("used_pct={mp:.1}%"),
                "Monitorar memória disponível.",
            ));
        }
    }

    Ok(json!({
        "action": "linux_sre_audit",
        "mode": "parse",
        "metrics": {
            "psi": psi_list,
            "failed_units": failed_units,
            "conntrack": conntrack,
            "mem_used_pct": mem_used_pct,
            "disk_alerts": disk_alerts,
            "default_routes": default_routes,
        },
        "findings": findings,
    }))
}

// ============================================================================
// 5. Topology Build + Mermaid + Blast Radius
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct TopoEdge {
    from: String,
    to: String,
    kind: String,
    local_if: Option<String>,
    remote_if: Option<String>,
    metric: Option<u64>,
    mtu: Option<u64>,
    state: String,
}

#[derive(Debug, Clone, Serialize)]
struct TopoAnomaly {
    severity: &'static str,
    kind: &'static str,
    detail: String,
}

fn sanitize_mermaid_id(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "node".into()
    } else {
        s
    }
}

fn add_edge(nodes: &mut BTreeSet<String>, edges: &mut Vec<TopoEdge>, edge: TopoEdge) {
    if edge.from.trim().is_empty() || edge.to.trim().is_empty() {
        return;
    }
    nodes.insert(edge.from.clone());
    nodes.insert(edge.to.clone());
    edges.push(edge);
}

fn parse_structured_links(
    val: Option<&Value>,
    kind: &str,
    nodes: &mut BTreeSet<String>,
    edges: &mut Vec<TopoEdge>,
) {
    let Some(v) = val else { return };
    if let Some(arr) = v.as_array() {
        for item in arr {
            let from = item
                .get("from")
                .or_else(|| item.get("local_node"))
                .or_else(|| item.get("local"))
                .or_else(|| item.get("source"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let to = item
                .get("to")
                .or_else(|| item.get("remote_node"))
                .or_else(|| item.get("neighbor"))
                .or_else(|| item.get("remote"))
                .or_else(|| item.get("target"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if from.is_empty() || to.is_empty() {
                continue;
            }
            let local_if = item
                .get("local_if")
                .or_else(|| item.get("local_interface"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let remote_if = item
                .get("remote_if")
                .or_else(|| item.get("remote_interface"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let metric = item.get("metric").and_then(Value::as_u64);
            let mtu = item.get("mtu").and_then(Value::as_u64);
            let state = item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("up")
                .to_string();
            add_edge(
                nodes,
                edges,
                TopoEdge {
                    from: from.to_string(),
                    to: to.to_string(),
                    kind: kind.to_string(),
                    local_if,
                    remote_if,
                    metric,
                    mtu,
                    state,
                },
            );
        }
    } else if let Some(text) = v.as_str() {
        parse_cli_topology_text(text, kind, nodes, edges);
    }
}

fn parse_cli_topology_text(
    text: &str,
    default_kind: &str,
    nodes: &mut BTreeSet<String>,
    edges: &mut Vec<TopoEdge>,
) {
    let mut local_node = "local-device".to_string();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("hostname ")
            .or_else(|| line.strip_prefix("local_node:"))
        {
            local_node = rest.trim().to_string();
            nodes.insert(local_node.clone());
            continue;
        }
        // Simple LLDP/CDP table line: RemoteDevice  LocalIf  HoldTime  Cap  RemoteIf
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 3
            && !matches!(
                cols[0].to_ascii_lowercase().as_str(),
                "device" | "system" | "neighbor" | "capability" | "total"
            )
            && (default_kind == "lldp" || default_kind == "cdp")
            && (cols[1].contains('/')
                || cols[1].starts_with("eth")
                || cols[1].starts_with("Gi")
                || cols[1].starts_with("Te"))
        {
            add_edge(
                nodes,
                edges,
                TopoEdge {
                    from: local_node.clone(),
                    to: cols[0].to_string(),
                    kind: default_kind.to_string(),
                    local_if: Some(cols[1].to_string()),
                    remote_if: cols.last().copied().map(str::to_string),
                    metric: None,
                    mtu: None,
                    state: "up".into(),
                },
            );
        }
    }
}

fn is_connected_without(
    nodes: &BTreeSet<String>,
    edges: &[TopoEdge],
    removed_node: Option<&str>,
    removed_iface: Option<&str>,
) -> Vec<String> {
    let remaining: Vec<String> = nodes
        .iter()
        .filter(|n| removed_node != Some(n.as_str()))
        .cloned()
        .collect();
    if remaining.is_empty() {
        return Vec::new();
    }
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in edges {
        if removed_node == Some(e.from.as_str()) || removed_node == Some(e.to.as_str()) {
            continue;
        }
        if let Some(rif) = removed_iface {
            let hits_iface =
                e.local_if.as_deref() == Some(rif) || e.remote_if.as_deref() == Some(rif);
            if hits_iface {
                continue;
            }
        }
        if e.state.eq_ignore_ascii_case("down") || e.state.eq_ignore_ascii_case("idle") {
            continue;
        }
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
        adj.entry(e.to.as_str()).or_default().push(e.from.as_str());
    }

    // BFS from highest-degree node (deterministic alphabetical tie-breaker)
    let root = remaining
        .iter()
        .max_by_key(|n| (adj.get(n.as_str()).map_or(0, Vec::len), Reverse(n.as_str())))
        .map(String::as_str)
        .unwrap_or("");
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    visited.insert(root);
    queue.push_back(root);
    while let Some(cur) = queue.pop_front() {
        if let Some(nbrs) = adj.get(cur) {
            for &nxt in nbrs {
                if visited.insert(nxt) {
                    queue.push_back(nxt);
                }
            }
        }
    }
    let disconnected: Vec<String> = remaining
        .iter()
        .filter(|n| !visited.contains(n.as_str()))
        .cloned()
        .collect();
    disconnected
}

fn topology_build(args: &Value) -> Result<Value> {
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    let mut edges: Vec<TopoEdge> = Vec::new();

    if let Some(devs) = args.get("devices").and_then(Value::as_array) {
        for d in devs {
            if let Some(name) = d
                .as_str()
                .or_else(|| d.get("name").and_then(Value::as_str))
                .or_else(|| d.get("id").and_then(Value::as_str))
            {
                if !name.trim().is_empty() {
                    nodes.insert(name.trim().to_string());
                }
            }
        }
    }

    parse_structured_links(args.get("lldp"), "lldp", &mut nodes, &mut edges);
    parse_structured_links(args.get("cdp"), "cdp", &mut nodes, &mut edges);
    parse_structured_links(args.get("bgp"), "bgp", &mut nodes, &mut edges);
    parse_structured_links(args.get("ospf"), "ospf", &mut nodes, &mut edges);
    parse_structured_links(args.get("rf_links"), "rf", &mut nodes, &mut edges);

    if let Some(raw) = opt_str(args, "output") {
        parse_cli_topology_text(raw, "lldp", &mut nodes, &mut edges);
    }

    if nodes.is_empty() && edges.is_empty() {
        return Err(ToolError::BadArguments(
            "topology_build requer ao menos um dos campos: devices, lldp, cdp, bgp, ospf, rf_links ou output".into(),
        ));
    }

    // Anomaly detection
    let mut anomalies = Vec::new();
    for i in 0..edges.len() {
        let a = &edges[i];
        if a.state.eq_ignore_ascii_case("down") || a.state.eq_ignore_ascii_case("idle") {
            anomalies.push(TopoAnomaly {
                severity: "critical",
                kind: "link_down",
                detail: format!(
                    "Enlace {} -> {} ({}) em estado `{}`",
                    a.from, a.to, a.kind, a.state
                ),
            });
        }
        for b in edges.iter().skip(i + 1) {
            let reverse_pair = a.from == b.to && a.to == b.from && a.kind == b.kind;
            if reverse_pair {
                if let (Some(ma), Some(mb)) = (a.metric, b.metric) {
                    if ma != mb {
                        anomalies.push(TopoAnomaly {
                            severity: "warning",
                            kind: "asymmetric_metric",
                            detail: format!(
                                "Métrica {} assimétrica entre {} <-> {}: {} vs {}",
                                a.kind, a.from, a.to, ma, mb
                            ),
                        });
                    }
                }
                if let (Some(ua), Some(ub)) = (a.mtu, b.mtu) {
                    if ua != ub {
                        anomalies.push(TopoAnomaly {
                            severity: "critical",
                            kind: "mtu_mismatch",
                            detail: format!(
                                "MTU divergente entre {} <-> {}: {} vs {}",
                                a.from, a.to, ua, ub
                            ),
                        });
                    }
                }
            }
        }
    }

    // Articulation-point / SPOF check
    if nodes.len() >= 3 {
        let baseline_disconnected = is_connected_without(&nodes, &edges, None, None);
        if baseline_disconnected.is_empty() {
            for candidate in &nodes {
                let orphaned = is_connected_without(&nodes, &edges, Some(candidate.as_str()), None);
                if !orphaned.is_empty() {
                    anomalies.push(TopoAnomaly {
                        severity: "warning",
                        kind: "single_point_of_failure",
                        detail: format!(
                            "Nó `{candidate}` é ponto único de falha (SPOF): sua queda isola {:?}",
                            orphaned
                        ),
                    });
                }
            }
        }
    }

    // Mermaid diagram
    let mut mermaid = vec!["graph LR".to_string()];
    for n in &nodes {
        let id = sanitize_mermaid_id(n);
        mermaid.push(format!("  {id}[\"{n}\"]"));
    }
    for e in &edges {
        let fid = sanitize_mermaid_id(&e.from);
        let tid = sanitize_mermaid_id(&e.to);
        let mut lbl_parts = vec![e.kind.to_uppercase()];
        if let (Some(li), Some(ri)) = (&e.local_if, &e.remote_if) {
            lbl_parts.push(format!("{li}<->{ri}"));
        } else if let Some(li) = &e.local_if {
            lbl_parts.push(li.clone());
        }
        if let Some(m) = e.metric {
            lbl_parts.push(format!("cost:{m}"));
        }
        let lbl = lbl_parts.join(" ");
        mermaid.push(format!("  {fid} -->|\"{lbl}\"| {tid}"));
    }

    // Blast radius
    let blast_radius = opt_str(args, "target_node").map(|target| {
        let target_if = opt_str(args, "target_interface");
        let impacted_edges: Vec<&TopoEdge> = edges
            .iter()
            .filter(|e| {
                let node_match = e.from == target || e.to == target;
                if let Some(iface) = target_if {
                    (e.from == target && e.local_if.as_deref() == Some(iface))
                        || (e.to == target && e.remote_if.as_deref() == Some(iface))
                } else {
                    node_match
                }
            })
            .collect();
        let isolated_nodes = if target_if.is_some() {
            is_connected_without(&nodes, &edges, None, target_if)
        } else {
            is_connected_without(&nodes, &edges, Some(target), None)
        };
        json!({
            "target_node": target,
            "target_interface": target_if,
            "impacted_links_count": impacted_edges.len(),
            "impacted_links": impacted_edges,
            "isolated_downstream_nodes": isolated_nodes,
        })
    });

    Ok(json!({
        "action": "topology_build",
        "nodes": nodes.into_iter().collect::<Vec<_>>(),
        "edges": edges,
        "anomalies": anomalies,
        "mermaid": mermaid.join("\n"),
        "blast_radius": blast_radius,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use zerith_providers::Cancel;
    use zerith_security::Policy;

    fn call_tool(args: Value) -> Result<Value> {
        let tools = all();
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.spec().name, "net_audit");
        assert_eq!(tool.spec().permission, Permission::Read);
        assert!(tool.concurrency_safe(&args));

        let pol = Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            Path::new("."),
        );
        let ctx = CallCtx {
            workspace: Path::new("."),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let cancel = Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(10),
            timeout: Duration::from_secs(10),
            host: &host,
        };
        let raw = tool.run(&args, &ctx, &exec)?;
        Ok(serde_json::from_str(&raw).unwrap())
    }

    #[test]
    fn cisco_asr_commands_plan_and_parse_qfp_drops_license_copp_bng() {
        let cmds = call_tool(json!({
            "action": "cisco_asr_audit",
            "model": "asr1001x"
        }))
        .unwrap();
        assert_eq!(cmds["mode"], "commands");
        assert_eq!(cmds["family"], "iosxe");

        let plan = call_tool(json!({
            "action": "cisco_asr_audit",
            "mode": "plan_change",
            "model": "asr1001x",
            "revert_minutes": 7
        }))
        .unwrap();
        assert!(plan["steps"].to_string().contains("revert timer 7"));

        let sample = "\
### show platform hardware qfp active datapath utilization
CPP 0: Subdev 0            5 secs        1 min        5 mins        1 hour
Input: Total (pps)             125000       124000       120000        110000
Processing: Load (pct)             82           80           78            72

### show platform hardware qfp active statistics drop
ArpNotForUs                    15420        986880
NatNoPort                      42           2688
TailDrop                       1200         1500000

### show platform hardware throughput level
Current configured throughput level is 5G

### show policy-map control-plane
Class-map: class-bgp (match-all)
  conformed 5000 packets, exceeded 12 packets, violated 4 packets

### show pppoe summary
Total       5200        4800        400

### show ip nat statistics
Total active translations: 185000
Hits: 900000  Misses: 200000

### show ip bgp summary
198.51.100.2    4  65002     120     130       10    0    0 00:05:10 Idle
198.51.100.3    4  65003    5000    5100       10    0  150 2d03h    125000

### show ip ospf neighbor
10.0.0.2          1   EXSTART/ -      00:00:35    10.1.1.2        Gi0/0/1

### show interfaces transceiver detail
Te0/0/0  42.1  3.30  22.0  -1.8  -24.5
";
        let parsed = call_tool(json!({
            "action": "cisco_asr_audit",
            "model": "asr1001x",
            "output": sample
        }))
        .unwrap();

        let metrics = &parsed["metrics"];
        assert_eq!(metrics["qfp_utilization"]["processing_5m_pct"], 78.0);
        assert_eq!(metrics["qfp_drops"]["critical_drops"]["NatNoPort"], 42);
        assert_eq!(metrics["qfp_drops"]["critical_drops"]["TailDrop"], 1200);
        assert_eq!(metrics["qfp_drops"]["benign_drops"]["ArpNotForUs"], 15420);
        assert!(metrics["qfp_drops"]["critical_drops"]
            .get("ArpNotForUs")
            .is_none());
        assert_eq!(metrics["throughput_license"]["capped"], true);
        assert_eq!(metrics["bng_nat"]["pppoe_total"], 5200);
        assert_eq!(metrics["bng_nat"]["pppoe_pta"], 4800);

        let findings_str = parsed["findings"].to_string();
        assert!(findings_str.contains("QFP Processing Load elevado"));
        assert!(findings_str.contains("Drops críticos não-nulos"));
        assert!(findings_str.contains("Licença de throughput limitada"));
        assert!(findings_str.contains("CoPP descartando pacotes"));
        assert!(findings_str.contains("CGNAT/NAT com taxa de miss elevada"));
        assert!(findings_str.contains("não estabelecido: Idle"));
        assert!(findings_str.contains("estado anômalo (EXSTART)"));
        assert!(findings_str.contains("Sinal óptico fora da faixa"));
    }

    #[test]
    fn datacom_audit_detects_dmswitch_pvid_mismatch_and_erps_failure() {
        let cfg = "\
! Datacom DmSwitch
interface ethernet 1/1/1
  switchport native vlan 1
!
vlan 100
  set-member untagged ethernet 1/1/1
!
ERPS Ring 10: ring-sul
  Role: RPL Owner
  State: Protection
";
        let parsed = call_tool(json!({
            "action": "datacom_audit",
            "output": cfg
        }))
        .unwrap();
        assert_eq!(parsed["platform"], "dmswitch");
        assert_eq!(
            parsed["metrics"]["pvid_mismatches"][0]["interface"],
            "ethernet 1/1/1"
        );
        assert_eq!(parsed["metrics"]["pvid_mismatches"][0]["native_pvid"], 1);
        let findings = parsed["findings"].to_string();
        assert!(findings.contains("Divergência PVID no DmSwitch"));
        assert!(findings.contains("não está Complete"));

        let dmos_plan = call_tool(json!({
            "action": "datacom_audit",
            "mode": "plan_change",
            "model": "dm4370",
            "revert_seconds": 180
        }))
        .unwrap();
        assert!(dmos_plan["steps"]
            .to_string()
            .contains("commit confirmed 180"));
    }

    #[test]
    fn ubiquiti_rf_audit_calculates_chain_imbalance_snr_and_fspl() {
        let parsed = call_tool(json!({
            "action": "ubiquiti_rf_audit",
            "output": "signal=-62, noise=-90, ccq=82, chains=-58/-67, wlanTxRate=144, wlanRxRate=130, distance=10000, freq=5800, txpower=20",
            "antenna_gain_dbi": 25.0
        }))
        .unwrap();

        let m = &parsed["metrics"];
        assert_eq!(m["snr_db"], 28.0);
        assert_eq!(m["chain_imbalance_db"], 9.0);
        // FSPL(10km, 5800MHz) = 32.44 + 20*1 + 20*log10(5800) = 127.71 dB
        let fspl = m["fspl_db"].as_f64().unwrap();
        assert!((fspl - 127.71).abs() < 0.1, "fspl={fspl}");
        // expected = 20 + 25 + 25 - 127.71 = -57.71 -> delta = -57.71 - (-62) = +4.29 dB
        let delta = m["path_loss_delta_db"].as_f64().unwrap();
        assert!((delta - 4.29).abs() < 0.1, "delta={delta}");

        let findings = parsed["findings"].to_string();
        assert!(findings.contains("Desbalanceamento crítico entre chains"));
        assert!(findings.contains("CCQ abaixo do ideal"));

        // Also test airOS JSON input
        let json_out = r#"{"wireless":{"signal":-52,"noisef":-92,"chainrssi":[39,40],"ccq":990,"txrate":650,"rxrate":650}}"#;
        let json_parsed = call_tool(json!({
            "action": "ubiquiti_rf_audit",
            "output": json_out
        }))
        .unwrap();
        assert_eq!(json_parsed["metrics"]["ccq_pct"], 99.0);
        assert_eq!(json_parsed["metrics"]["chain_imbalance_db"], 1.0);

        // Space-separated one-liner and colon key: value formats
        let sp = call_tool(json!({
            "action": "ubiquiti_rf_audit",
            "output": "mca-status: signal=-54 chain0=-54 chain1=-65 noise=-92 ccq=78 distance=12000 freq=5800 txpower=24",
            "antenna_gain_dbi": 27.0
        }))
        .unwrap();
        assert_eq!(sp["metrics"]["signal_dbm"], -54.0);
        assert_eq!(sp["metrics"]["chain_imbalance_db"], 11.0);
        assert_eq!(sp["metrics"]["snr_db"], 38.0);
        assert_eq!(sp["metrics"]["distance_km"], 12.0);

        let colon = call_tool(json!({
            "action": "ubiquiti_rf_audit",
            "output": "signal_strength: -54 dBm\nchain0: -54\nchain1: -65\nnoise_floor: -92 dBm"
        }))
        .unwrap();
        assert_eq!(colon["metrics"]["signal_dbm"], -54.0);
        assert_eq!(colon["metrics"]["chain_imbalance_db"], 11.0);
        assert_eq!(colon["metrics"]["snr_db"], 38.0);
    }

    #[test]
    fn linux_sre_audit_commands_plan_and_parse() {
        let cmds = call_tool(json!({ "action": "linux_sre_audit" })).unwrap();
        let composite = cmds["composite_command"].as_str().unwrap();
        assert!(composite.contains("/proc/pressure/cpu"));
        assert!(composite.contains("conntrack -S"));
        assert!(composite.contains("ip -j route"));

        let plan = call_tool(json!({
            "action": "linux_sre_audit",
            "mode": "plan_change",
            "revert_seconds": 120
        }))
        .unwrap();
        assert!(plan["dead_mans_switch_wrapper"]["arm"]
            .as_str()
            .unwrap()
            .contains("--on-active=120s"));

        let snapshot = "\
===PSI===
==> /proc/pressure/cpu <==
some avg10=28.50 avg60=15.00 avg300=8.00 total=1000
==> /proc/pressure/io <==
some avg10=12.00 avg60=5.00 avg300=2.00 total=500
full avg10=7.50 avg60=3.00 avg300=1.00 total=300
===MEM_DISK===
Mem: 16000 14000 500 100 1500 1200
/dev/nvme0n1p1 100G 96G 4G 96% /
===SYSTEMD_FAILED===
frr.service loaded failed failed FRRouting
===IP_ROUTE===
[{\"dst\":\"default\",\"gateway\":\"192.0.2.1\",\"dev\":\"eth0\"}]
===CONNTRACK===
cpu=0 found=10 invalid=0 insert=5 insert_failed=3 drop=12 early_drop=0
";
        let parsed = call_tool(json!({
            "action": "linux_sre_audit",
            "output": snapshot
        }))
        .unwrap();
        let f = parsed["findings"].to_string();
        assert!(f.contains("Contenção severa de PSI"));
        assert!(f.contains("systemd em estado failed"));
        assert!(f.contains("conntrack"));
        assert!(f.contains("Disco `/` em 96%"));
        assert!(f.contains("Uso de RAM crítico"));
    }

    #[test]
    fn topology_build_detects_anomalies_mermaid_and_blast_radius() {
        let res = call_tool(json!({
            "action": "topology_build",
            "ospf": [
                {"from": "core-01", "to": "dist-01", "local_if": "Te0/0/0", "remote_if": "Te0/0/1", "metric": 10, "mtu": 9000},
                {"from": "dist-01", "to": "core-01", "local_if": "Te0/0/1", "remote_if": "Te0/0/0", "metric": 100, "mtu": 1500},
                {"from": "dist-01", "to": "olt-01", "local_if": "Gi0/0/1", "remote_if": "Gi1/1/1", "metric": 20, "mtu": 1500}
            ],
            "target_node": "dist-01"
        }))
        .unwrap();

        let mermaid = res["mermaid"].as_str().unwrap();
        assert!(mermaid.starts_with("graph LR"));
        assert!(mermaid.contains("core_01"));
        assert!(mermaid.contains("olt_01"));

        let anomalies = res["anomalies"].to_string();
        assert!(anomalies.contains("asymmetric_metric"));
        assert!(anomalies.contains("mtu_mismatch"));
        assert!(anomalies.contains("single_point_of_failure"));

        let blast = &res["blast_radius"];
        assert_eq!(blast["impacted_links_count"], 3);
        let isolated = blast["isolated_downstream_nodes"].as_array().unwrap();
        assert!(isolated.contains(&json!("olt-01")));
    }
}
