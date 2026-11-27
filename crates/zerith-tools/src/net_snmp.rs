//! Ferramenta `net_snmp`: consultas SNMPv2c puras em Rust (UDP + BER, zero crates
//! externos), presets ISP (`if_table`, `dom_optical`, `bgp_peers`, `cpu_mem`,
//! `gpon_onu`, `ubiquiti_rf`, `cisco_asr_health`) e suporte a SNMPv3 USM via
//! `net-snmp` (`snmpget`/`snmpwalk`/`snmpbulkwalk`).

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::opt_str;
use crate::network::load_inventory;
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

const DEFAULT_SNMP_PORT: u16 = 161;
const MAX_UDP_PACKET: usize = 65_535;
const DEFAULT_MAX_VARBINDS: usize = 500;
const HARD_MAX_VARBINDS: usize = 5000;

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(NetSnmpTool::new())]
}

struct NetSnmpTool {
    spec: ToolSpec,
}

impl NetSnmpTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: "net_snmp".into(),
                description: "Consulta equipamentos de rede por SNMP (SNMPv2c nativo em Rust via UDP/BER \
                    com `get`, `walk`, `bulkwalk` e presets ISP; SNMPv3 USM via CLI `net-snmp`). \
                    Suporta lookup no inventário (`device`/`client`) ou `host`+`port` (161), \
                    `community_secret_ref` no cofre e presets: `if_table`, `dom_optical`, `bgp_peers`, \
                    `cpu_mem`, `gpon_onu`, `ubiquiti_rf`, `cisco_asr_health`."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["get", "walk", "bulkwalk"],
                            "description": "operação SNMP (padrão: get com oids, ou bulkwalk com preset)"
                        },
                        "device": {
                            "type": "string",
                            "description": "nome do dispositivo no inventory.toml"
                        },
                        "client": {
                            "type": "string",
                            "description": "cliente opcional para desambiguar `device`"
                        },
                        "host": {
                            "type": "string",
                            "description": "IP ou hostname de destino (alternativa a `device`)"
                        },
                        "port": {
                            "type": "integer",
                            "description": "porta UDP SNMP (padrão 161)"
                        },
                        "version": {
                            "type": "string",
                            "enum": ["2c", "3"],
                            "description": "versão SNMP: 2c (padrão, nativo) ou 3 (USM via net-snmp CLI)"
                        },
                        "community_secret_ref": {
                            "type": "string",
                            "description": "nome do segredo no cofre com a community SNMPv2c"
                        },
                        "community": {
                            "type": "string",
                            "description": "community SNMPv2c direta (redigida de saídas/erros; padrão 'public' se omitida)"
                        },
                        "preset": {
                            "type": "string",
                            "enum": [
                                "if_table",
                                "dom_optical",
                                "bgp_peers",
                                "cpu_mem",
                                "gpon_onu",
                                "ubiquiti_rf",
                                "cisco_asr_health"
                            ],
                            "description": "preset ISP pré-configurado com resumo formatado + OIDs"
                        },
                        "oids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "lista de OIDs numéricos (ex.: [\"1.3.6.1.2.1.1.1.0\"])"
                        },
                        "oid": {
                            "type": "string",
                            "description": "OID único alternativo a `oids`"
                        },
                        "max_repetitions": {
                            "type": "integer",
                            "description": "bulkwalk: max-repetitions por pacote (padrão 25, máx 50)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "máximo de varbinds retornados por walk/bulkwalk (padrão 500, máx 5000)"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "timeout por requisição em segundos (padrão 5, máx 60)"
                        },
                        "v3_user": {
                            "type": "string",
                            "description": "SNMPv3: usuário USM (securityName)"
                        },
                        "v3_level": {
                            "type": "string",
                            "enum": ["noAuthNoPriv", "authNoPriv", "authPriv"],
                            "description": "SNMPv3: nível de segurança"
                        },
                        "v3_auth_proto": {
                            "type": "string",
                            "enum": ["SHA", "MD5", "SHA-256", "SHA-512"],
                            "description": "SNMPv3: protocolo de autenticação (padrão SHA)"
                        },
                        "v3_priv_proto": {
                            "type": "string",
                            "enum": ["AES", "DES", "AES-192", "AES-256"],
                            "description": "SNMPv3: protocolo de privacidade (padrão AES)"
                        },
                        "auth_secret_ref": {
                            "type": "string",
                            "description": "SNMPv3: nome do segredo no cofre para senha de autenticação"
                        },
                        "priv_secret_ref": {
                            "type": "string",
                            "description": "SNMPv3: nome do segredo no cofre para senha de privacidade"
                        }
                    }
                }),
                returns: "valores SNMP formatados por OID ou resumo tabular do preset ISP",
                permission: Permission::Network,
                timeout: Duration::from_secs(60),
                cancellable: true,
                local: false,
                source: "built-in".into(),
            },
        }
    }
}

impl Tool for NetSnmpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn concurrency_safe(&self, _args: &Value) -> bool {
        true
    }

    fn action(&self, args: &Value, _ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        let act = opt_str(args, "preset")
            .or_else(|| opt_str(args, "action"))
            .unwrap_or("get");
        let target = opt_str(args, "device")
            .or_else(|| opt_str(args, "host"))
            .unwrap_or("snmp");
        Ok(Some(Action::Network(format!("snmp {act} {target}"))))
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        let (host, port) = resolve_target(args, ctx)?;
        let version = opt_str(args, "version").unwrap_or("2c");
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 60);
        let timeout = Duration::from_secs(timeout_secs);
        let max_reps = args
            .get("max_repetitions")
            .and_then(Value::as_u64)
            .unwrap_or(25)
            .clamp(1, 50) as i32;
        let max_varbinds = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_VARBINDS)
            .clamp(1, HARD_MAX_VARBINDS);

        if version == "3" {
            return run_snmp_v3(args, &host, port, timeout, max_reps, exec);
        }
        if version != "2c" && version != "2" {
            return Err(ToolError::BadArguments(format!(
                "versão SNMP não suportada: '{version}' (use '2c' ou '3')"
            )));
        }

        let (community, redact_community) = resolve_community(args, exec)?;
        let res = run_snmp_v2c(
            args,
            &host,
            port,
            &community,
            timeout,
            max_reps,
            max_varbinds,
            exec,
        );
        match res {
            Ok(out) => Ok(if redact_community {
                scrub(&out, &community)
            } else {
                out
            }),
            Err(e) => {
                if redact_community {
                    Err(ToolError::Failed(scrub(&e.to_string(), &community)))
                } else {
                    Err(e)
                }
            }
        }
    }
}

fn scrub(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "[redacted]")
    }
}

fn resolve_target(args: &Value, ctx: &CallCtx<'_>) -> Result<(String, u16)> {
    let port = args
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p > 0)
        .unwrap_or(DEFAULT_SNMP_PORT);

    if let Some(dev_arg) = opt_str(args, "device") {
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
        return Ok((found.host, port));
    }

    let host = opt_str(args, "host").ok_or_else(|| {
        ToolError::BadArguments("informe `device` (do inventário) ou `host`".into())
    })?;
    Ok((host.trim().to_string(), port))
}

fn resolve_community(args: &Value, exec: &Exec<'_>) -> Result<(String, bool)> {
    if let Some(sec_ref) = opt_str(args, "community_secret_ref") {
        let val = zerith_core::secrets::get_secret(sec_ref).map_err(|e| {
            ToolError::Failed(format!(
                "segredo '{sec_ref}' não encontrado no cofre: {e} (grave com `zerith secret set {sec_ref}`)"
            ))
        })?;
        let trimmed = val.trim().to_string();
        exec.host.note_secret(&trimmed);
        return Ok((trimmed, true));
    }
    if let Some(comm) = opt_str(args, "community") {
        let trimmed = comm.trim().to_string();
        exec.host.note_secret(&trimmed);
        return Ok((trimmed, true));
    }
    Ok(("public".to_string(), false))
}

fn collect_oids(args: &Value) -> Result<Vec<Vec<u32>>> {
    let mut raw_list = Vec::new();
    if let Some(arr) = args.get("oids").and_then(Value::as_array) {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                raw_list.push(s.to_string());
            }
        }
    }
    if let Some(single) = opt_str(args, "oid") {
        raw_list.push(single.trim().to_string());
    }
    if raw_list.is_empty() {
        return Err(ToolError::BadArguments(
            "informe `preset` ou `oid`/`oids` (OID numérico, ex.: 1.3.6.1.2.1.1.1.0)".into(),
        ));
    }
    raw_list.iter().map(|s| parse_oid(s)).collect()
}

pub(crate) fn parse_oid(s: &str) -> Result<Vec<u32>> {
    let trimmed = s.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        return Err(ToolError::BadArguments("OID vazio".into()));
    }
    let parts: std::result::Result<Vec<u32>, _> =
        trimmed.split('.').map(|p| p.parse::<u32>()).collect();
    let oid = parts.map_err(|_| {
        ToolError::BadArguments(format!(
            "OID inválido '{s}': use formato numérico pontuado (ex.: 1.3.6.1.2.1.1.1.0)"
        ))
    })?;
    if oid.len() < 2 {
        return Err(ToolError::BadArguments(format!(
            "OID '{s}' muito curto (precisa de ao menos 2 arcos, ex.: 1.3)"
        )));
    }
    if oid[0] > 2 || (oid[0] < 2 && oid[1] >= 40) {
        return Err(ToolError::BadArguments(format!(
            "OID '{s}' inválido na raiz ITU/ISO"
        )));
    }
    Ok(oid)
}

pub(crate) fn format_oid(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

pub(crate) fn oid_in_subtree(oid: &[u32], base: &[u32]) -> bool {
    oid.len() >= base.len() && &oid[..base.len()] == base
}

// --- Pure-Rust ASN.1 BER + SNMPv2c wire codec -------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum PduKind {
    GetRequest = 0xA0,
    GetNextRequest = 0xA1,
    GetResponse = 0xA2,
    GetBulkRequest = 0xA5,
}

impl PduKind {
    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            0xA0 => Ok(Self::GetRequest),
            0xA1 => Ok(Self::GetNextRequest),
            0xA2 => Ok(Self::GetResponse),
            0xA5 => Ok(Self::GetBulkRequest),
            _ => Err(ToolError::Failed(format!(
                "PDU SNMP inesperado: tag 0x{tag:02X}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SnmpValue {
    Integer(i64),
    OctetString(Vec<u8>),
    Null,
    Oid(Vec<u32>),
    IpAddress([u8; 4]),
    Counter32(u32),
    Gauge32(u32),
    TimeTicks(u32),
    Opaque(Vec<u8>),
    Counter64(u64),
    NoSuchObject,
    NoSuchInstance,
    EndOfMibView,
    Unknown { tag: u8, bytes: Vec<u8> },
}

impl SnmpValue {
    fn is_end_or_missing(&self) -> bool {
        matches!(
            self,
            Self::EndOfMibView | Self::NoSuchObject | Self::NoSuchInstance
        )
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(n) => Some(*n),
            Self::Counter32(n) | Self::Gauge32(n) | Self::TimeTicks(n) => Some(i64::from(*n)),
            Self::Counter64(n) => i64::try_from(*n).ok(),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Integer(n) if *n >= 0 => Some(*n as u64),
            Self::Counter32(n) | Self::Gauge32(n) | Self::TimeTicks(n) => Some(u64::from(*n)),
            Self::Counter64(n) => Some(*n),
            _ => None,
        }
    }

    fn as_display(&self) -> String {
        match self {
            Self::Integer(n) => n.to_string(),
            Self::OctetString(b) => format_octet_string(b),
            Self::Null => "NULL".into(),
            Self::Oid(o) => format_oid(o),
            Self::IpAddress(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            Self::Counter32(n) => n.to_string(),
            Self::Gauge32(n) => n.to_string(),
            Self::TimeTicks(ticks) => format_timeticks(*ticks),
            Self::Opaque(b) => format!("Opaque({})", format_hex(b)),
            Self::Counter64(n) => n.to_string(),
            Self::NoSuchObject => "noSuchObject".into(),
            Self::NoSuchInstance => "noSuchInstance".into(),
            Self::EndOfMibView => "endOfMibView".into(),
            Self::Unknown { tag, bytes } => format!("tag(0x{tag:02X}): {}", format_hex(bytes)),
        }
    }
}

fn format_octet_string(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(b) {
        if s.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        {
            return s.trim_end_matches('\0').to_string();
        }
    }
    format_hex(b)
}

fn format_hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn format_timeticks(ticks: u32) -> String {
    let total_secs = ticks / 100;
    let rem_cs = ticks % 100;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{mins:02}:{secs:02}.{rem_cs:02} ({ticks} ticks)")
    } else {
        format!("{hours:02}:{mins:02}:{secs:02}.{rem_cs:02} ({ticks} ticks)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnmpMessage {
    pub version: i64,
    pub community: String,
    pub pdu_type: PduKind,
    pub request_id: i32,
    pub error_or_non_repeaters: i32,
    pub index_or_max_reps: i32,
    pub varbinds: Vec<(Vec<u32>, SnmpValue)>,
}

fn encode_length(len: usize, out: &mut Vec<u8>) {
    if len < 128 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let first_nz = bytes
            .iter()
            .position(|&b| b != 0)
            .unwrap_or(bytes.len() - 1);
        let slice = &bytes[first_nz..];
        out.push(0x80 | (slice.len() as u8));
        out.extend_from_slice(slice);
    }
}

fn encode_tlv(tag: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    encode_length(payload.len(), out);
    out.extend_from_slice(payload);
}

fn encode_signed_int(val: i64) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    let mut idx = 0;
    while idx + 1 < bytes.len() {
        let same_sign_pad = (bytes[idx] == 0x00 && (bytes[idx + 1] & 0x80) == 0)
            || (bytes[idx] == 0xFF && (bytes[idx + 1] & 0x80) != 0);
        if same_sign_pad {
            idx += 1;
        } else {
            break;
        }
    }
    bytes[idx..].to_vec()
}

fn encode_unsigned_int(val: u64) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    let first_nz = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    let slice = &bytes[first_nz..];
    let mut out = Vec::with_capacity(slice.len() + 1);
    if (slice[0] & 0x80) != 0 {
        out.push(0x00);
    }
    out.extend_from_slice(slice);
    out
}

fn encode_oid_payload(oid: &[u32]) -> Result<Vec<u8>> {
    if oid.len() < 2 {
        return Err(ToolError::BadArguments(
            "OID precisa de ao menos 2 arcos".into(),
        ));
    }
    let mut buf = Vec::new();
    let first = oid[0]
        .checked_mul(40)
        .and_then(|v| v.checked_add(oid[1]))
        .ok_or_else(|| ToolError::BadArguments("overflow no prefixo do OID".into()))?;
    encode_vlq(first, &mut buf);
    for &arc in &oid[2..] {
        encode_vlq(arc, &mut buf);
    }
    Ok(buf)
}

fn encode_vlq(mut val: u32, out: &mut Vec<u8>) {
    let mut tmp = [0u8; 5];
    let mut n = 0;
    loop {
        tmp[n] = (val & 0x7F) as u8;
        n += 1;
        val >>= 7;
        if val == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        let byte = if i == 0 { tmp[i] } else { tmp[i] | 0x80 };
        out.push(byte);
    }
}

fn encode_value(val: &SnmpValue, out: &mut Vec<u8>) -> Result<()> {
    match val {
        SnmpValue::Integer(n) => encode_tlv(0x02, &encode_signed_int(*n), out),
        SnmpValue::OctetString(b) => encode_tlv(0x04, b, out),
        SnmpValue::Null => encode_tlv(0x05, &[], out),
        SnmpValue::Oid(o) => encode_tlv(0x06, &encode_oid_payload(o)?, out),
        SnmpValue::IpAddress(ip) => encode_tlv(0x40, ip, out),
        SnmpValue::Counter32(n) => encode_tlv(0x41, &encode_unsigned_int(u64::from(*n)), out),
        SnmpValue::Gauge32(n) => encode_tlv(0x42, &encode_unsigned_int(u64::from(*n)), out),
        SnmpValue::TimeTicks(n) => encode_tlv(0x43, &encode_unsigned_int(u64::from(*n)), out),
        SnmpValue::Opaque(b) => encode_tlv(0x44, b, out),
        SnmpValue::Counter64(n) => encode_tlv(0x46, &encode_unsigned_int(*n), out),
        SnmpValue::NoSuchObject => encode_tlv(0x80, &[], out),
        SnmpValue::NoSuchInstance => encode_tlv(0x81, &[], out),
        SnmpValue::EndOfMibView => encode_tlv(0x82, &[], out),
        SnmpValue::Unknown { tag, bytes } => encode_tlv(*tag, bytes, out),
    }
    Ok(())
}

pub(crate) fn encode_message(msg: &SnmpMessage) -> Result<Vec<u8>> {
    let mut vbs_payload = Vec::new();
    for (oid, val) in &msg.varbinds {
        let mut vb = Vec::new();
        encode_tlv(0x06, &encode_oid_payload(oid)?, &mut vb);
        encode_value(val, &mut vb)?;
        encode_tlv(0x30, &vb, &mut vbs_payload);
    }

    let mut pdu_payload = Vec::new();
    encode_tlv(
        0x02,
        &encode_signed_int(i64::from(msg.request_id)),
        &mut pdu_payload,
    );
    encode_tlv(
        0x02,
        &encode_signed_int(i64::from(msg.error_or_non_repeaters)),
        &mut pdu_payload,
    );
    encode_tlv(
        0x02,
        &encode_signed_int(i64::from(msg.index_or_max_reps)),
        &mut pdu_payload,
    );
    encode_tlv(0x30, &vbs_payload, &mut pdu_payload);

    let mut msg_payload = Vec::new();
    encode_tlv(0x02, &encode_signed_int(msg.version), &mut msg_payload);
    encode_tlv(0x04, msg.community.as_bytes(), &mut msg_payload);
    encode_tlv(msg.pdu_type as u8, &pdu_payload, &mut msg_payload);

    let mut packet = Vec::with_capacity(msg_payload.len() + 8);
    encode_tlv(0x30, &msg_payload, &mut packet);
    Ok(packet)
}

struct BerReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BerReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_byte(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| ToolError::Failed("pacote BER/SNMP truncado".into()))?;
        self.pos += 1;
        Ok(b)
    }

    fn read_length(&mut self) -> Result<usize> {
        let first = self.read_byte()?;
        if first < 0x80 {
            return Ok(usize::from(first));
        }
        let count = usize::from(first & 0x7F);
        if count == 0 || count > 4 {
            return Err(ToolError::Failed(format!(
                "comprimento BER SNMP inválido (octets={count})"
            )));
        }
        let mut len = 0usize;
        for _ in 0..count {
            let b = self.read_byte()?;
            len = (len << 8) | usize::from(b);
        }
        Ok(len)
    }

    fn read_tlv(&mut self) -> Result<(u8, &'a [u8])> {
        let tag = self.read_byte()?;
        let len = self.read_length()?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| ToolError::Failed("overflow de comprimento BER".into()))?;
        if end > self.buf.len() {
            return Err(ToolError::Failed("campo BER ultrapassa o pacote".into()));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok((tag, slice))
    }

    fn expect_tlv(&mut self, expected_tag: u8) -> Result<&'a [u8]> {
        let (tag, slice) = self.read_tlv()?;
        if tag != expected_tag {
            return Err(ToolError::Failed(format!(
                "tag BER esperado 0x{expected_tag:02X}, recebido 0x{tag:02X}"
            )));
        }
        Ok(slice)
    }
}

fn decode_signed_int(bytes: &[u8]) -> Result<i64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(ToolError::Failed(format!(
            "INTEGER BER com tamanho inválido ({} bytes)",
            bytes.len()
        )));
    }
    let sign_extend = (bytes[0] & 0x80) != 0;
    let mut val: u64 = if sign_extend { u64::MAX } else { 0 };
    for &b in bytes {
        val = (val << 8) | u64::from(b);
    }
    Ok(val as i64)
}

fn decode_unsigned_int(bytes: &[u8]) -> Result<u64> {
    if bytes.is_empty() || bytes.len() > 9 {
        return Err(ToolError::Failed(format!(
            "inteiro sem sinal BER inválido ({} bytes)",
            bytes.len()
        )));
    }
    let slice = if bytes.len() > 1 && bytes[0] == 0x00 {
        &bytes[1..]
    } else {
        bytes
    };
    if slice.len() > 8 {
        return Err(ToolError::Failed("inteiro sem sinal BER > 64 bits".into()));
    }
    let mut val = 0u64;
    for &b in slice {
        val = (val << 8) | u64::from(b);
    }
    Ok(val)
}

fn decode_oid(bytes: &[u8]) -> Result<Vec<u32>> {
    if bytes.is_empty() {
        return Err(ToolError::Failed("OID BER vazio".into()));
    }
    let mut arcs = Vec::new();
    let mut idx = 0usize;
    let first = decode_vlq(bytes, &mut idx)?;
    if first < 40 {
        arcs.push(0);
        arcs.push(first);
    } else if first < 80 {
        arcs.push(1);
        arcs.push(first - 40);
    } else {
        arcs.push(2);
        arcs.push(first - 80);
    }
    while idx < bytes.len() {
        arcs.push(decode_vlq(bytes, &mut idx)?);
    }
    Ok(arcs)
}

fn decode_vlq(bytes: &[u8], idx: &mut usize) -> Result<u32> {
    let mut val = 0u32;
    for _ in 0..5 {
        let b = *bytes
            .get(*idx)
            .ok_or_else(|| ToolError::Failed("sub-identificador OID truncado".into()))?;
        *idx += 1;
        val = val
            .checked_shl(7)
            .ok_or_else(|| ToolError::Failed("overflow em sub-identificador OID".into()))?
            | u32::from(b & 0x7F);
        if (b & 0x80) == 0 {
            return Ok(val);
        }
    }
    Err(ToolError::Failed("sub-identificador OID > 32 bits".into()))
}

fn decode_value(tag: u8, bytes: &[u8]) -> Result<SnmpValue> {
    Ok(match tag {
        0x02 => SnmpValue::Integer(decode_signed_int(bytes)?),
        0x04 => SnmpValue::OctetString(bytes.to_vec()),
        0x05 => SnmpValue::Null,
        0x06 => SnmpValue::Oid(decode_oid(bytes)?),
        0x40 => {
            if bytes.len() != 4 {
                return Err(ToolError::Failed(format!(
                    "IpAddress BER inválido ({} bytes)",
                    bytes.len()
                )));
            }
            SnmpValue::IpAddress([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
        0x41 => SnmpValue::Counter32(
            u32::try_from(decode_unsigned_int(bytes)?)
                .map_err(|_| ToolError::Failed("Counter32 fora da faixa".into()))?,
        ),
        0x42 => SnmpValue::Gauge32(
            u32::try_from(decode_unsigned_int(bytes)?)
                .map_err(|_| ToolError::Failed("Gauge32 fora da faixa".into()))?,
        ),
        0x43 => SnmpValue::TimeTicks(
            u32::try_from(decode_unsigned_int(bytes)?)
                .map_err(|_| ToolError::Failed("TimeTicks fora da faixa".into()))?,
        ),
        0x44 => SnmpValue::Opaque(bytes.to_vec()),
        0x46 => SnmpValue::Counter64(decode_unsigned_int(bytes)?),
        0x80 => SnmpValue::NoSuchObject,
        0x81 => SnmpValue::NoSuchInstance,
        0x82 => SnmpValue::EndOfMibView,
        other => SnmpValue::Unknown {
            tag: other,
            bytes: bytes.to_vec(),
        },
    })
}

pub(crate) fn decode_message(packet: &[u8]) -> Result<SnmpMessage> {
    let mut outer = BerReader::new(packet);
    let msg_bytes = outer.expect_tlv(0x30)?;
    let mut rd = BerReader::new(msg_bytes);
    let version = decode_signed_int(rd.expect_tlv(0x02)?)?;
    let community_bytes = rd.expect_tlv(0x04)?;
    let community = String::from_utf8_lossy(community_bytes).into_owned();
    let (pdu_tag, pdu_bytes) = rd.read_tlv()?;
    let pdu_type = PduKind::from_tag(pdu_tag)?;

    let mut pdu = BerReader::new(pdu_bytes);
    let req_id = decode_signed_int(pdu.expect_tlv(0x02)?)? as i32;
    let err_status = decode_signed_int(pdu.expect_tlv(0x02)?)? as i32;
    let err_index = decode_signed_int(pdu.expect_tlv(0x02)?)? as i32;
    let vbs_bytes = pdu.expect_tlv(0x30)?;

    let mut vbs_rd = BerReader::new(vbs_bytes);
    let mut varbinds = Vec::new();
    while !vbs_rd.is_empty() {
        let vb_bytes = vbs_rd.expect_tlv(0x30)?;
        let mut vb_rd = BerReader::new(vb_bytes);
        let oid = decode_oid(vb_rd.expect_tlv(0x06)?)?;
        let (vtag, vbytes) = vb_rd.read_tlv()?;
        let val = decode_value(vtag, vbytes)?;
        varbinds.push((oid, val));
    }

    Ok(SnmpMessage {
        version,
        community,
        pdu_type,
        request_id: req_id,
        error_or_non_repeaters: err_status,
        index_or_max_reps: err_index,
        varbinds,
    })
}

fn snmp_error_label(code: i32) -> &'static str {
    match code {
        0 => "noError",
        1 => "tooBig",
        2 => "noSuchName",
        3 => "badValue",
        4 => "readOnly",
        5 => "genErr",
        6 => "noAccess",
        7 => "wrongType",
        8 => "wrongLength",
        9 => "wrongEncoding",
        10 => "wrongValue",
        11 => "noCreation",
        12 => "inconsistentValue",
        13 => "resourceUnavailable",
        14 => "commitFailed",
        15 => "undoFailed",
        16 => "authorizationError",
        17 => "notWritable",
        18 => "inconsistentName",
        _ => "snmpError",
    }
}

struct SnmpV2Client {
    socket: UdpSocket,
    target: SocketAddr,
    community: String,
    timeout: Duration,
    next_id: i32,
}

impl SnmpV2Client {
    fn connect(host: &str, port: u16, community: String, timeout: Duration) -> Result<Self> {
        let target = (host, port)
            .to_socket_addrs()
            .map_err(|e| ToolError::Failed(format!("DNS/endereço SNMP '{host}:{port}': {e}")))?
            .next()
            .ok_or_else(|| {
                ToolError::Failed(format!("nenhum endereço resolvido para '{host}:{port}'"))
            })?;
        let bind_addr = if target.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .map_err(|e| ToolError::Failed(format!("falha ao abrir socket UDP SNMP: {e}")))?;
        socket.connect(target).map_err(|e| {
            ToolError::Failed(format!("falha ao conectar UDP SNMP a {target}: {e}"))
        })?;
        let seed = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            & 0x3FFF_FFFF) as i32;
        Ok(Self {
            socket,
            target,
            community,
            timeout,
            next_id: seed.max(1),
        })
    }

    fn exchange(
        &mut self,
        pdu_type: PduKind,
        non_rep_or_err: i32,
        max_reps_or_idx: i32,
        varbinds: Vec<(Vec<u32>, SnmpValue)>,
        exec: &Exec<'_>,
    ) -> Result<SnmpMessage> {
        let mut last_err = None;
        for attempt in 0..2 {
            exec.check()?;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let req_id = self.next_id;
            let msg = SnmpMessage {
                version: 1, // SNMPv2c
                community: self.community.clone(),
                pdu_type,
                request_id: req_id,
                error_or_non_repeaters: non_rep_or_err,
                index_or_max_reps: max_reps_or_idx,
                varbinds: varbinds.clone(),
            };
            let pkt = encode_message(&msg)?;
            self.socket.send(&pkt).map_err(|e| {
                ToolError::Failed(format!(
                    "falha ao enviar pacote SNMP para {}: {e}",
                    self.target
                ))
            })?;

            let req_deadline = Instant::now() + self.timeout;
            let mut buf = vec![0u8; MAX_UDP_PACKET];
            loop {
                exec.check()?;
                let now = Instant::now();
                if now >= req_deadline {
                    last_err = Some(ToolError::Failed(format!(
                        "timeout SNMP ({}s) aguardando resposta de {}",
                        self.timeout.as_secs(),
                        self.target
                    )));
                    break;
                }
                let slice = (req_deadline - now).min(Duration::from_millis(200));
                let _ = self.socket.set_read_timeout(Some(slice));
                match self.socket.recv(&mut buf) {
                    Ok(n) => {
                        let resp = decode_message(&buf[..n])?;
                        if resp.request_id != req_id {
                            continue;
                        }
                        if resp.error_or_non_repeaters != 0 {
                            return Err(ToolError::Failed(format!(
                                "erro SNMP {} (código {}, índice {})",
                                snmp_error_label(resp.error_or_non_repeaters),
                                resp.error_or_non_repeaters,
                                resp.index_or_max_reps
                            )));
                        }
                        return Ok(resp);
                    }
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                        continue;
                    }
                    Err(e) => {
                        return Err(ToolError::Failed(format!(
                            "erro recebendo UDP SNMP de {}: {e}",
                            self.target
                        )));
                    }
                }
            }
            if attempt == 0 {
                continue;
            }
        }
        Err(last_err.unwrap_or_else(|| ToolError::Failed("timeout SNMP".into())))
    }

    fn get(&mut self, oids: &[Vec<u32>], exec: &Exec<'_>) -> Result<Vec<(Vec<u32>, SnmpValue)>> {
        let vbs = oids.iter().map(|o| (o.clone(), SnmpValue::Null)).collect();
        let resp = self.exchange(PduKind::GetRequest, 0, 0, vbs, exec)?;
        Ok(resp.varbinds)
    }

    fn walk(
        &mut self,
        base: &[u32],
        max_varbinds: usize,
        exec: &Exec<'_>,
    ) -> Result<Vec<(Vec<u32>, SnmpValue)>> {
        let mut out = Vec::new();
        let mut cursor = base.to_vec();
        while out.len() < max_varbinds {
            let resp = self.exchange(
                PduKind::GetNextRequest,
                0,
                0,
                vec![(cursor.clone(), SnmpValue::Null)],
                exec,
            )?;
            let Some((oid, val)) = resp.varbinds.into_iter().next() else {
                break;
            };
            if val.is_end_or_missing() || !oid_in_subtree(&oid, base) || oid <= cursor {
                break;
            }
            cursor = oid.clone();
            out.push((oid, val));
        }
        Ok(out)
    }

    fn bulkwalk(
        &mut self,
        base: &[u32],
        max_reps: i32,
        max_varbinds: usize,
        exec: &Exec<'_>,
    ) -> Result<Vec<(Vec<u32>, SnmpValue)>> {
        let mut out = Vec::new();
        let mut cursor = base.to_vec();
        while out.len() < max_varbinds {
            let resp = self.exchange(
                PduKind::GetBulkRequest,
                0,
                max_reps,
                vec![(cursor.clone(), SnmpValue::Null)],
                exec,
            )?;
            if resp.varbinds.is_empty() {
                break;
            }
            let mut progressed = false;
            for (oid, val) in resp.varbinds {
                if val.is_end_or_missing() || !oid_in_subtree(&oid, base) || oid <= cursor {
                    return Ok(out);
                }
                cursor = oid.clone();
                out.push((oid, val));
                progressed = true;
                if out.len() >= max_varbinds {
                    return Ok(out);
                }
            }
            if !progressed {
                break;
            }
        }
        Ok(out)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_snmp_v2c(
    args: &Value,
    host: &str,
    port: u16,
    community: &str,
    timeout: Duration,
    max_reps: i32,
    max_varbinds: usize,
    exec: &Exec<'_>,
) -> Result<String> {
    let mut client = SnmpV2Client::connect(host, port, community.to_string(), timeout)?;

    if let Some(preset_name) = opt_str(args, "preset") {
        let preset = resolve_preset(preset_name)?;
        let use_walk = opt_str(args, "action") == Some("walk");
        let mut all_vbs = Vec::new();
        for base_str in preset.oids {
            let base = parse_oid(base_str)?;
            let mut vbs = if use_walk {
                client.walk(&base, max_varbinds, exec)?
            } else {
                client.bulkwalk(&base, max_reps, max_varbinds, exec)?
            };
            all_vbs.append(&mut vbs);
        }
        return Ok(format_preset_output(&preset, host, port, &all_vbs));
    }

    let action = opt_str(args, "action").unwrap_or("get");
    let oids = collect_oids(args)?;

    match action {
        "get" => {
            let vbs = client.get(&oids, exec)?;
            Ok(format_raw_varbinds("SNMP GET", host, port, &vbs))
        }
        "walk" => {
            let mut all = Vec::new();
            for base in &oids {
                all.extend(client.walk(base, max_varbinds, exec)?);
            }
            Ok(format_raw_varbinds("SNMP WALK", host, port, &all))
        }
        "bulkwalk" => {
            let mut all = Vec::new();
            for base in &oids {
                all.extend(client.bulkwalk(base, max_reps, max_varbinds, exec)?);
            }
            Ok(format_raw_varbinds("SNMP BULKWALK", host, port, &all))
        }
        other => Err(ToolError::BadArguments(format!(
            "ação SNMP desconhecida: '{other}' (use get, walk, bulkwalk)"
        ))),
    }
}

fn format_raw_varbinds(
    title: &str,
    host: &str,
    port: u16,
    vbs: &[(Vec<u32>, SnmpValue)],
) -> String {
    if vbs.is_empty() {
        return format!("{title} {host}:{port} — nenhum varbind retornado.");
    }
    let mut lines = vec![format!("### {title} ({host}:{port}) — {} itens", vbs.len())];
    for (oid, val) in vbs {
        lines.push(format!("- `{}` = `{}`", format_oid(oid), val.as_display()));
    }
    lines.join("\n")
}

// --- ISP Presets ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IspPresetKind {
    IfTable,
    DomOptical,
    BgpPeers,
    CpuMem,
    GponOnu,
    UbiquitiRf,
    CiscoAsrHealth,
}

pub(crate) struct IspPreset {
    pub kind: IspPresetKind,
    pub title: &'static str,
    pub oids: &'static [&'static str],
}

pub(crate) fn resolve_preset(name: &str) -> Result<IspPreset> {
    match name.trim().to_ascii_lowercase().as_str() {
        "if_table" => Ok(IspPreset {
            kind: IspPresetKind::IfTable,
            title: "Tabela de Interfaces (IF-MIB)",
            oids: &[
                "1.3.6.1.2.1.2.2.1.2",     // ifDescr
                "1.3.6.1.2.1.2.2.1.7",     // ifAdminStatus
                "1.3.6.1.2.1.2.2.1.8",     // ifOperStatus
                "1.3.6.1.2.1.2.2.1.14",    // ifInErrors
                "1.3.6.1.2.1.2.2.1.20",    // ifOutErrors
                "1.3.6.1.2.1.31.1.1.1.15", // ifHighSpeed (Mbps)
                "1.3.6.1.2.1.31.1.1.1.6",  // ifHCInOctets
                "1.3.6.1.2.1.31.1.1.1.10", // ifHCOutOctets
                "1.3.6.1.2.1.31.1.1.1.18", // ifAlias
            ],
        }),
        "dom_optical" => Ok(IspPreset {
            kind: IspPresetKind::DomOptical,
            title: "Diagnóstico Óptico / DOM (Entity Sensor + Huawei + MikroTik)",
            oids: &[
                "1.3.6.1.2.1.47.1.1.1.1.2",       // entPhysicalDescr
                "1.3.6.1.2.1.99.1.1.1.4",         // entSensorValue
                "1.3.6.1.4.1.2011.5.25.31.1.1.3", // hwOpticalModuleInfoTable
                "1.3.6.1.4.1.14988.1.1.19",       // mtxrOpticalTable
            ],
        }),
        "bgp_peers" => Ok(IspPreset {
            kind: IspPresetKind::BgpPeers,
            title: "Sessões BGP (BGP4-MIB)",
            oids: &[
                "1.3.6.1.2.1.15.2",    // bgpLocalAs
                "1.3.6.1.2.1.15.3.1",  // bgpPeerTable
            ],
        }),
        "cpu_mem" => Ok(IspPreset {
            kind: IspPresetKind::CpuMem,
            title: "Sistema, CPU e Memória",
            oids: &[
                "1.3.6.1.2.1.1.1",                // sysDescr
                "1.3.6.1.2.1.1.3",                // sysUpTime
                "1.3.6.1.2.1.1.5",                // sysName
                "1.3.6.1.2.1.25.3.3.1.2",         // hrProcessorLoad
                "1.3.6.1.2.1.25.2.3.1",           // hrStorageTable
                "1.3.6.1.4.1.2021.10.1.3",        // laLoad
                "1.3.6.1.4.1.2021.4",             // UCD memory
                "1.3.6.1.4.1.9.9.109.1.1.1.1.8",  // Cisco cpmCPUTotal5minRev
                "1.3.6.1.4.1.2011.5.25.31.1.1.1", // Huawei hwEntityStateTable
            ],
        }),
        "gpon_onu" => Ok(IspPreset {
            kind: IspPresetKind::GponOnu,
            title: "GPON ONUs / OLT (Huawei / ZTE / FiberHome)",
            oids: &[
                "1.3.6.1.4.1.2011.6.128.1.1.2.43.1.9",  // hwGponDeviceOntDescript
                "1.3.6.1.4.1.2011.6.128.1.1.2.46.1.15", // hwGponDeviceOntControlRunStatus
                "1.3.6.1.4.1.2011.6.128.1.1.2.51.1.4",  // hwGponDeviceOntOpticalDdmRxPower
                "1.3.6.1.4.1.3902.1012.3.28.1.1.2",     // ZTE zxAnGponOnuMgmtTypeName
                "1.3.6.1.4.1.5875.800.3.10.1.1.11",     // FiberHome onuStatus
            ],
        }),
        "ubiquiti_rf" => Ok(IspPreset {
            kind: IspPresetKind::UbiquitiRf,
            title: "Ubiquiti RF / airMAX / airFiber (UBNT-AirMAX-MIB)",
            oids: &[
                "1.3.6.1.4.1.41112.1.4.1.1.4",  // ubntRadioFreq (MHz)
                "1.3.6.1.4.1.41112.1.4.1.1.6",  // ubntRadioTxPower (dBm)
                "1.3.6.1.4.1.41112.1.4.5.1.5",  // ubntWlStatSignal (dBm)
                "1.3.6.1.4.1.41112.1.4.5.1.7",  // ubntWlStatCcq (%)
                "1.3.6.1.4.1.41112.1.4.5.1.8",  // ubntWlStatNoiseFloor (dBm)
                "1.3.6.1.4.1.41112.1.4.5.1.9",  // ubntWlStatTxRate
                "1.3.6.1.4.1.41112.1.4.5.1.10", // ubntWlStatRxRate
                "1.3.6.1.4.1.41112.1.4.6.1.3",  // ubntAirMaxQuality (%)
                "1.3.6.1.4.1.41112.1.4.6.1.4",  // ubntAirMaxCapacity (%)
                "1.3.6.1.4.1.41112.1.4.7.1.3",  // ubntStaSignal (dBm)
                "1.3.6.1.4.1.41112.1.4.7.1.4",  // ubntStaNoiseFloor (dBm)
                "1.3.6.1.4.1.41112.1.4.7.1.5",  // ubntStaDistance (m)
                "1.3.6.1.4.1.41112.1.4.7.1.6",  // ubntStaCcq (%)
                "1.3.6.1.4.1.41112.1.4.7.1.11", // ubntStaTxRate
                "1.3.6.1.4.1.41112.1.4.7.1.12", // ubntStaRxRate
                "1.3.6.1.4.1.41112.1.4.7.1.17", // ubntStaTxAirtime
                "1.3.6.1.4.1.41112.1.4.7.1.18", // ubntStaRxAirtime
                "1.3.6.1.4.1.41112.1.3.2.1",    // ubntAirFiber status/capacity
            ],
        }),
        "cisco_asr_health" => Ok(IspPreset {
            kind: IspPresetKind::CiscoAsrHealth,
            title: "Cisco ASR / IOS-XE Health (CPU, MemPool, EnvMon, QFP, NAT)",
            oids: &[
                "1.3.6.1.4.1.9.9.109.1.1.1.1.6",  // cpmCPUTotal5secRev
                "1.3.6.1.4.1.9.9.109.1.1.1.1.7",  // cpmCPUTotal1minRev
                "1.3.6.1.4.1.9.9.109.1.1.1.1.8",  // cpmCPUTotal5minRev
                "1.3.6.1.4.1.9.9.221.1.1.1.1.3",  // cempMemPoolName
                "1.3.6.1.4.1.9.9.221.1.1.1.1.18", // cempMemPoolHCUsed
                "1.3.6.1.4.1.9.9.221.1.1.1.1.20", // cempMemPoolHCFree
                "1.3.6.1.4.1.9.9.13.1.3.1.2",     // ciscoEnvMonTemperatureStatusDescr
                "1.3.6.1.4.1.9.9.13.1.3.1.3",     // ciscoEnvMonTemperatureStatusValue
                "1.3.6.1.4.1.9.9.13.1.3.1.6",     // ciscoEnvMonTemperatureState
                "1.3.6.1.4.1.9.9.13.1.4.1.3",     // ciscoEnvMonFanState
                "1.3.6.1.4.1.9.9.13.1.5.1.3",     // ciscoEnvMonSupplyState
                "1.3.6.1.4.1.9.9.715.1.1.1",      // CISCO-QFP-MIB utilization
                "1.3.6.1.4.1.9.9.532.1.1",        // CISCO-NAT-EXT-MIB stats
                "1.3.6.1.4.1.9.10.14.1",          // CISCO-AAA-SESSION-MIB BNG
            ],
        }),
        other => Err(ToolError::BadArguments(format!(
            "preset SNMP desconhecido: '{other}' (use if_table, dom_optical, bgp_peers, cpu_mem, gpon_onu, ubiquiti_rf, cisco_asr_health)"
        ))),
    }
}

fn format_preset_output(
    preset: &IspPreset,
    host: &str,
    port: u16,
    vbs: &[(Vec<u32>, SnmpValue)],
) -> String {
    if vbs.is_empty() {
        return format!(
            "### {} ({host}:{port})\nNenhum OID respondeu para este preset (verifique community/MIBs do equipamento).",
            preset.title
        );
    }

    let summary = match preset.kind {
        IspPresetKind::IfTable => summarize_if_table(vbs),
        IspPresetKind::BgpPeers => summarize_bgp_peers(vbs),
        IspPresetKind::CpuMem => summarize_cpu_mem(vbs),
        IspPresetKind::UbiquitiRf => summarize_ubiquiti_rf(vbs),
        IspPresetKind::CiscoAsrHealth => summarize_cisco_asr_health(vbs),
        IspPresetKind::DomOptical | IspPresetKind::GponOnu => summarize_grouped_table(vbs),
    };

    let mut out = format!("### {} ({host}:{port})\n{summary}", preset.title);
    out.push_str(&format!("\n\n<details_oids total=\"{}\">\n", vbs.len()));
    for (oid, val) in vbs.iter().take(120) {
        out.push_str(&format!(
            "- `{}` = `{}`\n",
            format_oid(oid),
            val.as_display()
        ));
    }
    if vbs.len() > 120 {
        out.push_str(&format!("... (+{} OIDs omitidos)\n", vbs.len() - 120));
    }
    out.push_str("</details_oids>");
    out
}

fn summarize_if_table(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    #[derive(Default)]
    struct IfRow {
        descr: String,
        alias: String,
        admin: String,
        oper: String,
        speed_mbps: Option<u64>,
        in_oct: Option<u64>,
        out_oct: Option<u64>,
        in_err: Option<u64>,
        out_err: Option<u64>,
    }
    let status_str = |v: &SnmpValue| match v.as_i64() {
        Some(1) => "up".to_string(),
        Some(2) => "down".to_string(),
        Some(3) => "testing".to_string(),
        Some(n) => n.to_string(),
        None => v.as_display(),
    };
    let mut rows: BTreeMap<u32, IfRow> = BTreeMap::new();
    for (oid, val) in vbs {
        let Some(&idx) = oid.last() else { continue };
        let row = rows.entry(idx).or_default();
        if oid.starts_with(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 2]) {
            row.descr = val.as_display();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 7]) {
            row.admin = status_str(val);
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 8]) {
            row.oper = status_str(val);
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 14]) {
            row.in_err = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 20]) {
            row.out_err = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 15]) {
            row.speed_mbps = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6]) {
            row.in_oct = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 10]) {
            row.out_oct = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 18]) {
            row.alias = val.as_display();
        }
    }
    if rows.is_empty() {
        return "Nenhuma interface encontrada.".into();
    }
    let mut lines = vec![
        "| idx | Interface | Admin/Oper | Mbps | RX Bytes | TX Bytes | Err (In/Out) | Alias |"
            .to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];
    for (idx, r) in rows {
        let name = if r.descr.is_empty() {
            format!("if#{idx}")
        } else {
            r.descr
        };
        let speed = r.speed_mbps.map_or("-".into(), |s| s.to_string());
        let rx = r.in_oct.map_or("-".into(), |s| s.to_string());
        let tx = r.out_oct.map_or("-".into(), |s| s.to_string());
        let errs = format!(
            "{}/{}",
            r.in_err.map_or("-".into(), |s| s.to_string()),
            r.out_err.map_or("-".into(), |s| s.to_string())
        );
        lines.push(format!(
            "| {idx} | {name} | {}/{} | {speed} | {rx} | {tx} | {errs} | {} |",
            if r.admin.is_empty() { "-" } else { &r.admin },
            if r.oper.is_empty() { "-" } else { &r.oper },
            r.alias.replace('|', "/")
        ));
    }
    lines.join("\n")
}

fn summarize_bgp_peers(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    #[derive(Default)]
    struct Peer {
        state: String,
        remote_as: Option<u64>,
        in_upd: Option<u64>,
        out_upd: Option<u64>,
        est_secs: Option<u64>,
    }
    let bgp_state = |n: i64| match n {
        1 => "Idle",
        2 => "Connect",
        3 => "Active",
        4 => "OpenSent",
        5 => "OpenConfirm",
        6 => "Established",
        _ => "Unknown",
    };
    let mut local_as = None;
    let mut peers: BTreeMap<String, Peer> = BTreeMap::new();
    for (oid, val) in vbs {
        if oid.starts_with(&[1, 3, 6, 1, 2, 1, 15, 2]) {
            local_as = val.as_u64();
            continue;
        }
        // bgpPeerEntry: 1.3.6.1.2.1.15.3.1.<col>.<ip1>.<ip2>.<ip3>.<ip4>
        if oid.len() >= 15 && oid.starts_with(&[1, 3, 6, 1, 2, 1, 15, 3, 1]) {
            let col = oid[9];
            let peer_ip = format!(
                "{}.{}.{}.{}",
                oid[oid.len() - 4],
                oid[oid.len() - 3],
                oid[oid.len() - 2],
                oid[oid.len() - 1]
            );
            let p = peers.entry(peer_ip).or_default();
            match col {
                2 => {
                    p.state = val
                        .as_i64()
                        .map(|n| bgp_state(n).to_string())
                        .unwrap_or_else(|| val.as_display())
                }
                9 => p.remote_as = val.as_u64(),
                10 => p.in_upd = val.as_u64(),
                11 => p.out_upd = val.as_u64(),
                16 => p.est_secs = val.as_u64(),
                _ => {}
            }
        }
    }
    let mut lines = Vec::new();
    if let Some(asn) = local_as {
        lines.push(format!("- **Local AS:** {asn}"));
    }
    if peers.is_empty() {
        lines.push("Nenhum peer BGP encontrado na BGP4-MIB.".into());
    } else {
        lines.push("| Peer IP | Remote AS | Estado | Uptime (s) | In/Out Updates |".into());
        lines.push("| --- | --- | --- | --- | --- |".into());
        for (ip, p) in peers {
            lines.push(format!(
                "| {ip} | {} | {} | {} | {}/{} |",
                p.remote_as.map_or("-".into(), |a| a.to_string()),
                if p.state.is_empty() { "-" } else { &p.state },
                p.est_secs.map_or("-".into(), |u| u.to_string()),
                p.in_upd.map_or("-".into(), |u| u.to_string()),
                p.out_upd.map_or("-".into(), |u| u.to_string()),
            ));
        }
    }
    lines.join("\n")
}

fn summarize_cpu_mem(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    let mut sys_name = None;
    let mut sys_descr = None;
    let mut uptime = None;
    let mut cpus = Vec::new();
    let mut extras = Vec::new();

    for (oid, val) in vbs {
        if oid.starts_with(&[1, 3, 6, 1, 2, 1, 1, 1]) {
            sys_descr = Some(val.as_display());
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 1, 3]) {
            uptime = Some(val.as_display());
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 1, 5]) {
            sys_name = Some(val.as_display());
        } else if oid.starts_with(&[1, 3, 6, 1, 2, 1, 25, 3, 3, 1, 2])
            || oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 8])
        {
            if let Some(pct) = val.as_u64() {
                cpus.push(format!("{pct}%"));
            }
        } else {
            extras.push(format!("- `{}` = `{}`", format_oid(oid), val.as_display()));
        }
    }

    let mut out = Vec::new();
    if let Some(n) = sys_name {
        out.push(format!("- **Hostname:** {n}"));
    }
    if let Some(d) = sys_descr {
        out.push(format!("- **Descrição:** {d}"));
    }
    if let Some(u) = uptime {
        out.push(format!("- **Uptime:** {u}"));
    }
    if !cpus.is_empty() {
        out.push(format!("- **CPU Load:** {}", cpus.join(", ")));
    }
    if !extras.is_empty() {
        out.push(format!("- **Métricas adicionais:** {} OIDs", extras.len()));
    }
    out.join("\n")
}

fn summarize_grouped_table(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    let mut lines = vec![format!("Coletados **{}** registros SNMP:", vbs.len())];
    for (oid, val) in vbs.iter().take(40) {
        lines.push(format!("- `{}` = `{}`", format_oid(oid), val.as_display()));
    }
    if vbs.len() > 40 {
        lines.push(format!(
            "... (+{} registros na seção de OIDs)",
            vbs.len() - 40
        ));
    }
    lines.join("\n")
}

fn summarize_ubiquiti_rf(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    let mut freq_mhz: Option<i64> = None;
    let mut tx_power: Option<i64> = None;
    let mut signal: Option<i64> = None;
    let mut noise: Option<i64> = None;
    let mut ccq: Option<i64> = None;
    let mut airmax_qual: Option<i64> = None;
    let mut airmax_cap: Option<i64> = None;
    let mut tx_rate: Option<u64> = None;
    let mut rx_rate: Option<u64> = None;
    let mut sta_count: usize = 0;

    for (oid, val) in vbs {
        if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 1, 1, 4]) {
            freq_mhz = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 1, 1, 6]) {
            tx_power = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 5]) {
            signal = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 7]) {
            ccq = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 8]) {
            noise = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 9]) {
            tx_rate = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 10]) {
            rx_rate = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 6, 1, 3]) {
            airmax_qual = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 6, 1, 4]) {
            airmax_cap = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 41112, 1, 4, 7, 1, 3]) {
            sta_count += 1;
        }
    }

    let mut out = Vec::new();
    if let Some(f) = freq_mhz {
        out.push(format!("- **Frequência:** {f} MHz"));
    }
    if let Some(p) = tx_power {
        out.push(format!("- **TX Power:** {p} dBm"));
    }
    if let Some(s) = signal {
        let snr_note = noise
            .map(|n| format!(" (Noise Floor: {n} dBm, SNR: {} dB)", s - n))
            .unwrap_or_default();
        out.push(format!("- **Signal (RSSI):** {s} dBm{snr_note}"));
    } else if let Some(n) = noise {
        out.push(format!("- **Noise Floor:** {n} dBm"));
    }
    if let Some(c) = ccq {
        out.push(format!("- **CCQ:** {c}%"));
    }
    if airmax_qual.is_some() || airmax_cap.is_some() {
        out.push(format!(
            "- **airMAX Quality/Capacity:** {}% / {}%",
            airmax_qual.map_or("-".into(), |v| v.to_string()),
            airmax_cap.map_or("-".into(), |v| v.to_string())
        ));
    }
    if tx_rate.is_some() || rx_rate.is_some() {
        out.push(format!(
            "- **Data Rate TX/RX:** {} / {}",
            tx_rate.map_or("-".into(), |v| v.to_string()),
            rx_rate.map_or("-".into(), |v| v.to_string())
        ));
    }
    if sta_count > 0 {
        out.push(format!("- **Stations associadas:** {sta_count}"));
    }
    if out.is_empty() {
        summarize_grouped_table(vbs)
    } else {
        out.join("\n")
    }
}

fn summarize_cisco_asr_health(vbs: &[(Vec<u32>, SnmpValue)]) -> String {
    #[derive(Default)]
    struct CpuRow {
        s5: Option<u64>,
        m1: Option<u64>,
        m5: Option<u64>,
    }
    #[derive(Default)]
    struct MemRow {
        name: String,
        used: Option<u64>,
        free: Option<u64>,
    }
    #[derive(Default)]
    struct TempRow {
        descr: String,
        celsius: Option<i64>,
        state: Option<i64>,
    }

    let env_state = |n: i64| match n {
        1 => "normal",
        2 => "warning",
        3 => "critical",
        4 => "shutdown",
        5 => "notPresent",
        _ => "notFunctioning",
    };

    let mut cpus: BTreeMap<u32, CpuRow> = BTreeMap::new();
    let mut mems: BTreeMap<u32, MemRow> = BTreeMap::new();
    let mut temps: BTreeMap<u32, TempRow> = BTreeMap::new();
    let mut fan_alerts = Vec::new();
    let mut psu_alerts = Vec::new();
    let mut qfp_entries = 0usize;
    let mut nat_entries = 0usize;
    let mut bng_entries = 0usize;

    for (oid, val) in vbs {
        let Some(&idx) = oid.last() else { continue };
        if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 6]) {
            cpus.entry(idx).or_default().s5 = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 7]) {
            cpus.entry(idx).or_default().m1 = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 8]) {
            cpus.entry(idx).or_default().m5 = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 3]) {
            mems.entry(idx).or_default().name = val.as_display();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 18]) {
            mems.entry(idx).or_default().used = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 20]) {
            mems.entry(idx).or_default().free = val.as_u64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 2]) {
            temps.entry(idx).or_default().descr = val.as_display();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 3]) {
            temps.entry(idx).or_default().celsius = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 6]) {
            temps.entry(idx).or_default().state = val.as_i64();
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 4, 1, 3]) {
            if let Some(st) = val.as_i64().filter(|&s| s != 1 && s != 5) {
                fan_alerts.push(format!("fan#{idx}={}", env_state(st)));
            }
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 5, 1, 3]) {
            if let Some(st) = val.as_i64().filter(|&s| s != 1 && s != 5) {
                psu_alerts.push(format!("psu#{idx}={}", env_state(st)));
            }
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 715, 1, 1, 1]) {
            qfp_entries += 1;
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 9, 532, 1, 1]) {
            nat_entries += 1;
        } else if oid.starts_with(&[1, 3, 6, 1, 4, 1, 9, 10, 14, 1]) {
            bng_entries += 1;
        }
    }

    let mut out = Vec::new();
    for (idx, c) in &cpus {
        out.push(format!(
            "- **CPU #{idx} (5s/1m/5m):** {}% / {}% / {}%",
            c.s5.map_or("-".into(), |v| v.to_string()),
            c.m1.map_or("-".into(), |v| v.to_string()),
            c.m5.map_or("-".into(), |v| v.to_string()),
        ));
    }
    for (idx, m) in &mems {
        let name = if m.name.is_empty() {
            format!("pool#{idx}")
        } else {
            m.name.clone()
        };
        if let (Some(used), Some(free)) = (m.used, m.free) {
            let total = used.saturating_add(free);
            let pct = used.saturating_mul(100).checked_div(total).unwrap_or(0);
            out.push(format!(
                "- **Memória `{name}`:** {pct}% usada ({used} / {total} bytes)"
            ));
        }
    }
    for (idx, t) in &temps {
        let label = if t.descr.is_empty() {
            format!("sensor#{idx}")
        } else {
            t.descr.clone()
        };
        let deg = t.celsius.map_or("-".into(), |c| format!("{c}°C"));
        let st = t.state.map_or("unknown", env_state);
        out.push(format!("- **Temp `{label}`:** {deg} ({st})"));
    }
    if !fan_alerts.is_empty() {
        out.push(format!("- **Alerta Fans:** {}", fan_alerts.join(", ")));
    }
    if !psu_alerts.is_empty() {
        out.push(format!("- **Alerta Fontes:** {}", psu_alerts.join(", ")));
    }
    if qfp_entries > 0 || nat_entries > 0 || bng_entries > 0 {
        out.push(format!(
            "- **Tabelas ASR (QFP/NAT/BNG OIDs):** {qfp_entries} / {nat_entries} / {bng_entries}"
        ));
    }
    if out.is_empty() {
        summarize_grouped_table(vbs)
    } else {
        out.join("\n")
    }
}

// --- SNMPv3 USM via system net-snmp CLI -------------------------------------

fn validate_cli_token(field: &str, val: &str) -> Result<String> {
    let t = val.trim();
    if t.is_empty() {
        return Err(ToolError::BadArguments(format!(
            "{field} não pode ficar vazio"
        )));
    }
    if t.starts_with('-') {
        return Err(ToolError::BadArguments(format!(
            "{field} inválido (não pode começar com '-')"
        )));
    }
    if t.chars().any(char::is_control) {
        return Err(ToolError::BadArguments(format!(
            "{field} contém caractere de controle"
        )));
    }
    Ok(t.to_string())
}

fn run_snmp_v3(
    args: &Value,
    host: &str,
    port: u16,
    timeout: Duration,
    max_reps: i32,
    exec: &Exec<'_>,
) -> Result<String> {
    let user = opt_str(args, "v3_user")
        .ok_or_else(|| ToolError::BadArguments("SNMPv3 exige `v3_user` (usuário USM)".into()))?;
    let user = validate_cli_token("v3_user", user)?;
    let host_clean = validate_cli_token("host", host)?;

    let mut secrets_to_scrub = Vec::new();
    let auth_pass = if let Some(r) = opt_str(args, "auth_secret_ref") {
        let v = zerith_core::secrets::get_secret(r).map_err(|e| {
            ToolError::Failed(format!("segredo '{r}' não encontrado no cofre: {e}"))
        })?;
        exec.host.note_secret(&v);
        secrets_to_scrub.push(v.clone());
        Some(v)
    } else {
        None
    };
    let priv_pass = if let Some(r) = opt_str(args, "priv_secret_ref") {
        let v = zerith_core::secrets::get_secret(r).map_err(|e| {
            ToolError::Failed(format!("segredo '{r}' não encontrado no cofre: {e}"))
        })?;
        exec.host.note_secret(&v);
        secrets_to_scrub.push(v.clone());
        Some(v)
    } else {
        None
    };

    let level = opt_str(args, "v3_level").unwrap_or(match (&auth_pass, &priv_pass) {
        (Some(_), Some(_)) => "authPriv",
        (Some(_), None) => "authNoPriv",
        _ => "noAuthNoPriv",
    });
    if !matches!(level, "noAuthNoPriv" | "authNoPriv" | "authPriv") {
        return Err(ToolError::BadArguments(format!(
            "v3_level inválido: '{level}' (use noAuthNoPriv, authNoPriv ou authPriv)"
        )));
    }

    let auth_proto = validate_cli_token(
        "v3_auth_proto",
        opt_str(args, "v3_auth_proto").unwrap_or("SHA"),
    )?;
    let priv_proto = validate_cli_token(
        "v3_priv_proto",
        opt_str(args, "v3_priv_proto").unwrap_or("AES"),
    )?;

    let action = opt_str(args, "action").unwrap_or(if opt_str(args, "preset").is_some() {
        "bulkwalk"
    } else {
        "get"
    });
    let bin = match action {
        "get" => "snmpget",
        "walk" => "snmpwalk",
        "bulkwalk" => "snmpbulkwalk",
        other => {
            return Err(ToolError::BadArguments(format!(
                "ação SNMP desconhecida: '{other}'"
            )))
        }
    };

    let target_oids: Vec<String> = if let Some(preset_name) = opt_str(args, "preset") {
        resolve_preset(preset_name)?
            .oids
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        collect_oids(args)?.iter().map(|o| format_oid(o)).collect()
    };

    let mut outputs = Vec::new();
    for oid in &target_oids {
        exec.check()?;
        let mut cmd = Command::new(bin);
        cmd.args([
            "-v3",
            "-u",
            &user,
            "-l",
            level,
            "-t",
            &timeout.as_secs().to_string(),
            "-On",
        ]);
        if let Some(ref ap) = auth_pass {
            cmd.args(["-a", &auth_proto, "-A", ap]);
        }
        if let Some(ref pp) = priv_pass {
            cmd.args(["-x", &priv_proto, "-X", pp]);
        }
        if bin == "snmpbulkwalk" {
            cmd.arg(format!("-Cr{max_reps}"));
        }
        cmd.arg(format!("{host_clean}:{port}"));
        cmd.arg(oid);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(ToolError::Failed(format!(
                    "SNMPv3 requer o utilitário `{bin}` (pacote net-snmp) instalado no PATH; \
                     para SNMPv2c use `version: \"2c\"` (nativo em Rust sem dependências externas)"
                )));
            }
            Err(e) => {
                return Err(ToolError::Failed(format!(
                    "falha ao iniciar `{bin}` para SNMPv3: {e}"
                )));
            }
        };

        let status = loop {
            if let Err(err) = exec.check() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    let _ = child.kill();
                    return Err(ToolError::Failed(format!("erro aguardando `{bin}`: {e}")));
                }
            }
        };

        let out = child.wait_with_output().map_err(ToolError::Io)?;
        let mut stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        for s in &secrets_to_scrub {
            stdout = scrub(&stdout, s);
            stderr = scrub(&stderr, s);
        }
        if !status.success() {
            return Err(ToolError::Failed(format!(
                "`{bin}` falhou ({status}): {}",
                stderr.trim()
            )));
        }
        if !stdout.trim().is_empty() {
            outputs.push(stdout.trim().to_string());
        }
    }

    if outputs.is_empty() {
        Ok(format!(
            "SNMPv3 ({bin}) {host_clean}:{port} — nenhum resultado retornado."
        ))
    } else {
        Ok(outputs.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use zerith_providers::Cancel;

    #[test]
    fn ber_round_trip_all_snmp_value_types_and_pdus() {
        let varbinds = vec![
            (
                vec![1, 3, 6, 1, 2, 1, 1, 1, 0],
                SnmpValue::OctetString(b"Huawei VRP".to_vec()),
            ),
            (
                vec![1, 3, 6, 1, 2, 1, 1, 3, 0],
                SnmpValue::TimeTicks(12_345_678),
            ),
            (vec![1, 3, 6, 1, 2, 1, 2, 1, 0], SnmpValue::Integer(-42)),
            (
                vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 5, 1],
                SnmpValue::Gauge32(1_000_000_000),
            ),
            (
                vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 10, 1],
                SnmpValue::Counter32(0xDEAD_BEEF),
            ),
            (
                vec![1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6, 1],
                SnmpValue::Counter64(9_876_543_210_123),
            ),
            (
                vec![1, 3, 6, 1, 2, 1, 4, 20, 1, 1],
                SnmpValue::IpAddress([192, 0, 2, 10]),
            ),
            (
                vec![1, 3, 6, 1, 2, 1, 1, 2, 0],
                SnmpValue::Oid(vec![1, 3, 6, 1, 4, 1, 2011, 2]),
            ),
            (vec![1, 3, 6, 1, 2, 1, 99, 0], SnmpValue::EndOfMibView),
        ];

        for pdu in [
            PduKind::GetRequest,
            PduKind::GetNextRequest,
            PduKind::GetResponse,
            PduKind::GetBulkRequest,
        ] {
            let msg = SnmpMessage {
                version: 1,
                community: "noc-ro".into(),
                pdu_type: pdu,
                request_id: 77_123,
                error_or_non_repeaters: 0,
                index_or_max_reps: if pdu == PduKind::GetBulkRequest {
                    25
                } else {
                    0
                },
                varbinds: varbinds.clone(),
            };
            let encoded = encode_message(&msg).unwrap();
            let decoded = decode_message(&encoded).unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn preset_resolution_and_invalid_preset() {
        for name in [
            "if_table",
            "dom_optical",
            "bgp_peers",
            "cpu_mem",
            "gpon_onu",
            "ubiquiti_rf",
            "cisco_asr_health",
        ] {
            let p = resolve_preset(name).unwrap();
            assert!(!p.oids.is_empty(), "{name}");
            for o in p.oids {
                assert!(parse_oid(o).is_ok(), "{name}: {o}");
            }
        }
        assert!(resolve_preset("desconhecido").is_err());
    }

    #[test]
    fn ubiquiti_rf_and_cisco_asr_health_formatters_render_metrics() {
        let rf_preset = resolve_preset("ubiquiti_rf").unwrap();
        let rf_vbs = vec![
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 1, 1, 4, 1],
                SnmpValue::Integer(5580),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 1, 1, 6, 1],
                SnmpValue::Integer(21),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 5, 1],
                SnmpValue::Integer(-54),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 8, 1],
                SnmpValue::Integer(-91),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 5, 1, 7, 1],
                SnmpValue::Integer(99),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 41112, 1, 4, 6, 1, 4, 1],
                SnmpValue::Integer(88),
            ),
        ];
        let rf_out = format_preset_output(&rf_preset, "10.0.0.10", 161, &rf_vbs);
        assert!(rf_out.contains("5580 MHz"), "{rf_out}");
        assert!(rf_out.contains("-54 dBm"), "{rf_out}");
        assert!(rf_out.contains("SNR: 37 dB"), "{rf_out}");
        assert!(rf_out.contains("CCQ:** 99%"), "{rf_out}");

        let asr_preset = resolve_preset("cisco_asr_health").unwrap();
        let asr_vbs = vec![
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 6, 1],
                SnmpValue::Gauge32(18),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 7, 1],
                SnmpValue::Gauge32(22),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 109, 1, 1, 1, 1, 8, 1],
                SnmpValue::Gauge32(25),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 3, 1],
                SnmpValue::OctetString(b"Processor".to_vec()),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 18, 1],
                SnmpValue::Counter64(6_000_000_000),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 221, 1, 1, 1, 1, 20, 1],
                SnmpValue::Counter64(2_000_000_000),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 2, 1],
                SnmpValue::OctetString(b"R0 Temp: ESP".to_vec()),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 3, 1],
                SnmpValue::Gauge32(52),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 3, 1, 6, 1],
                SnmpValue::Integer(1),
            ),
            (
                vec![1, 3, 6, 1, 4, 1, 9, 9, 13, 1, 4, 1, 3, 2],
                SnmpValue::Integer(3),
            ),
        ];
        let asr_out = format_preset_output(&asr_preset, "10.0.0.1", 161, &asr_vbs);
        assert!(asr_out.contains("18% / 22% / 25%"), "{asr_out}");
        assert!(asr_out.contains("Processor`:** 75% usada"), "{asr_out}");
        assert!(asr_out.contains("52°C (normal)"), "{asr_out}");
        assert!(asr_out.contains("fan#2=critical"), "{asr_out}");
    }

    #[test]
    fn udp_loopback_get_walk_bulkwalk_and_loop_guard() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = sock.local_addr().unwrap().port();
        let seen_communities: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen_clone = Arc::clone(&seen_communities);

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mib: Vec<(Vec<u32>, SnmpValue)> = vec![
                (
                    vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 1],
                    SnmpValue::OctetString(b"ether1".to_vec()),
                ),
                (
                    vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 2],
                    SnmpValue::OctetString(b"sfp-sfpplus1".to_vec()),
                ),
                (vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8, 1], SnmpValue::Integer(1)),
                (vec![1, 3, 6, 1, 2, 1, 2, 2, 1, 8, 2], SnmpValue::Integer(2)),
                // Outside ifDescr subtree to signal end of walk
                (vec![1, 3, 6, 1, 2, 1, 3, 1], SnmpValue::Integer(99)),
            ];
            while let Ok((n, peer)) = sock.recv_from(&mut buf) {
                let req = decode_message(&buf[..n]).unwrap();
                seen_clone.lock().unwrap().push(req.community.clone());
                let vbs = match req.pdu_type {
                    PduKind::GetRequest => req
                        .varbinds
                        .iter()
                        .map(|(oid, _)| {
                            let val = mib
                                .iter()
                                .find(|(k, _)| k == oid)
                                .map(|(_, v)| v.clone())
                                .unwrap_or(SnmpValue::NoSuchObject);
                            (oid.clone(), val)
                        })
                        .collect(),
                    PduKind::GetNextRequest => {
                        let cursor = &req.varbinds[0].0;
                        let next = mib
                            .iter()
                            .find(|(k, _)| k > cursor)
                            .cloned()
                            .unwrap_or((cursor.clone(), SnmpValue::EndOfMibView));
                        vec![next]
                    }
                    PduKind::GetBulkRequest => {
                        let cursor = &req.varbinds[0].0;
                        let reps = req.index_or_max_reps.max(1) as usize;
                        let items: Vec<_> = mib
                            .iter()
                            .filter(|(k, _)| k > cursor)
                            .take(reps)
                            .cloned()
                            .collect();
                        if items.is_empty() {
                            vec![(cursor.clone(), SnmpValue::EndOfMibView)]
                        } else {
                            items
                        }
                    }
                    PduKind::GetResponse => continue,
                };
                let resp = SnmpMessage {
                    version: 1,
                    community: req.community,
                    pdu_type: PduKind::GetResponse,
                    request_id: req.request_id,
                    error_or_non_repeaters: 0,
                    index_or_max_reps: 0,
                    varbinds: vbs,
                };
                let _ = sock.send_to(&encode_message(&resp).unwrap(), peer);
            }
        });

        let cancel = Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(10),
            timeout: Duration::from_secs(10),
            host: &host,
        };

        let get_out = run_snmp_v2c(
            &json!({"action": "get", "oid": "1.3.6.1.2.1.2.2.1.2.1"}),
            "127.0.0.1",
            port,
            "super-secret-comm",
            Duration::from_secs(2),
            10,
            100,
            &exec,
        )
        .unwrap();
        assert!(get_out.contains("ether1"), "{get_out}");

        let walk_out = run_snmp_v2c(
            &json!({"action": "walk", "oid": "1.3.6.1.2.1.2.2.1.2"}),
            "127.0.0.1",
            port,
            "super-secret-comm",
            Duration::from_secs(2),
            10,
            100,
            &exec,
        )
        .unwrap();
        assert!(
            walk_out.contains("ether1") && walk_out.contains("sfp-sfpplus1"),
            "{walk_out}"
        );
        assert!(
            !walk_out.contains("1.3.6.1.2.1.3.1"),
            "must stop at subtree boundary: {walk_out}"
        );

        let preset_out = run_snmp_v2c(
            &json!({"preset": "if_table"}),
            "127.0.0.1",
            port,
            "super-secret-comm",
            Duration::from_secs(2),
            10,
            100,
            &exec,
        )
        .unwrap();
        assert!(preset_out.contains("| 1 | ether1 | -/up |"), "{preset_out}");
        assert!(
            preset_out.contains("| 2 | sfp-sfpplus1 | -/down |"),
            "{preset_out}"
        );
        assert!(!seen_communities.lock().unwrap().is_empty());
    }
}
