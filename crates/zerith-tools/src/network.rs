//! Network consulting tools: device inventory (`.zerith/inventory.toml`),
//! keyring-backed SSH connect (`net_connect`), multi-vendor command risk
//! classification (`classify_network_command`), and safe batch execution
//! (`terminal_batch`).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::terminal::{self, OpenSpec};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    CiscoIos,
    CiscoNxos,
    CiscoXr,
    HuaweiVrp,
    JuniperJunos,
    MikrotikRouterOs,
    FortinetFortiOs,
    Linux,
    #[default]
    Generic,
}

impl Vendor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CiscoIos => "cisco_ios",
            Self::CiscoNxos => "cisco_nxos",
            Self::CiscoXr => "cisco_xr",
            Self::HuaweiVrp => "huawei_vrp",
            Self::JuniperJunos => "juniper_junos",
            Self::MikrotikRouterOs => "mikrotik_router_os",
            Self::FortinetFortiOs => "fortinet_forti_os",
            Self::Linux => "linux",
            Self::Generic => "generic",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "cisco" | "ios" | "cisco_ios" | "iosxe" | "cisco_iosxe" => Some(Self::CiscoIos),
            "nxos" | "cisco_nxos" => Some(Self::CiscoNxos),
            "iosxr" | "xr" | "cisco_xr" | "cisco_iosxr" => Some(Self::CiscoXr),
            "huawei" | "vrp" | "huawei_vrp" => Some(Self::HuaweiVrp),
            "juniper" | "junos" | "juniper_junos" => Some(Self::JuniperJunos),
            "mikrotik" | "routeros" | "mikrotik_router_os" | "mikrotik_routeros" => {
                Some(Self::MikrotikRouterOs)
            }
            "fortinet" | "fortios" | "fortigate" | "fortinet_forti_os" | "fortinet_fortios" => {
                Some(Self::FortinetFortiOs)
            }
            "linux" | "frr" | "vyos" => Some(Self::Linux),
            "generic" | "other" => Some(Self::Generic),
            _ => None,
        }
    }

    pub fn disable_paging_cmd(self) -> Option<&'static str> {
        match self {
            Self::CiscoIos | Self::CiscoNxos | Self::CiscoXr => Some("terminal length 0"),
            Self::HuaweiVrp => Some("screen-length 0 temporary"),
            Self::JuniperJunos => Some("set cli screen-length 0"),
            Self::FortinetFortiOs => Some("config system console\nset output standard\nend"),
            Self::MikrotikRouterOs | Self::Linux | Self::Generic => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Core,
    Border,
    Distribution,
    Access,
    Firewall,
    #[default]
    Lab,
    Server,
}

impl DeviceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Border => "border",
            Self::Distribution => "distribution",
            Self::Access => "access",
            Self::Firewall => "firewall",
            Self::Lab => "lab",
            Self::Server => "server",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "core" => Some(Self::Core),
            "border" | "edge" | "bgp" => Some(Self::Border),
            "distribution" | "dist" => Some(Self::Distribution),
            "access" => Some(Self::Access),
            "firewall" | "fw" => Some(Self::Firewall),
            "lab" | "staging" | "test" => Some(Self::Lab),
            "server" | "linux" | "host" => Some(Self::Server),
            _ => None,
        }
    }

    pub fn is_critical(self) -> bool {
        matches!(self, Self::Core | Self::Border | Self::Firewall)
    }
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceEntry {
    pub name: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    pub vendor: Vendor,
    pub role: DeviceRole,
    pub secret_ref: Option<String>,
    pub jump_host: Option<String>,
    pub legacy_ssh: bool,
    pub tags: Vec<String>,
    pub description: Option<String>,
}

impl Default for DeviceEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: 22,
            user: String::new(),
            vendor: Vendor::Generic,
            role: DeviceRole::Lab,
            secret_ref: None,
            jump_host: None,
            legacy_ssh: false,
            tags: Vec::new(),
            description: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct InventoryFile {
    devices: Vec<DeviceEntry>,
}

pub fn parse_inventory(toml_str: &str) -> std::result::Result<Vec<DeviceEntry>, String> {
    let parsed: InventoryFile =
        toml::from_str(toml_str).map_err(|e| format!("inventory.toml inválido: {e}"))?;
    Ok(parsed
        .devices
        .into_iter()
        .filter(|d| !d.name.trim().is_empty() && !d.host.trim().is_empty())
        .collect())
}

/// Loads devices from global `<root>/inventory.toml` and `<workspace>/.zerith/inventory.toml`.
/// Workspace entries override global entries with the same `name`.
pub fn load_inventory(
    workspace: &Path,
    global_root: Option<&Path>,
) -> std::result::Result<Vec<DeviceEntry>, String> {
    let mut by_name: HashMap<String, DeviceEntry> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut merge = |path: &Path| -> std::result::Result<(), String> {
        if !path.is_file() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("erro lendo {}: {e}", path.display()))?;
        for dev in parse_inventory(&raw)? {
            let key = dev.name.trim().to_ascii_lowercase();
            if !by_name.contains_key(&key) {
                order.push(key.clone());
            }
            by_name.insert(key, dev);
        }
        Ok(())
    };
    if let Some(root) = global_root {
        merge(&root.join("inventory.toml"))?;
    }
    merge(&workspace.join(".zerith").join("inventory.toml"))?;
    Ok(order
        .into_iter()
        .filter_map(|k| by_name.remove(&k))
        .collect())
}

fn validate_ssh_token(field: &str, value: &str) -> std::result::Result<(), String> {
    let t = value.trim();
    if t.is_empty() {
        return Err(format!("{field} não pode ficar vazio"));
    }
    if t.starts_with('-') {
        return Err(format!("{field} inválido (não pode começar com `-`): {t}"));
    }
    if t.chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, ';' | '&' | '|' | '`' | '$'))
    {
        return Err(format!("{field} contém caractere inválido: {t}"));
    }
    Ok(())
}

/// Builds a strict, flag-injection-free `ssh` invocation for [`DeviceEntry`].
pub fn build_ssh_command(dev: &DeviceEntry) -> std::result::Result<String, String> {
    validate_ssh_token("host", &dev.host)?;
    let user = if dev.user.trim().is_empty() {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "admin".into())
    } else {
        dev.user.trim().to_string()
    };
    validate_ssh_token("user", &user)?;
    let mut parts = vec![
        "ssh".to_string(),
        "-tt".to_string(),
        "-o".to_string(),
        "ConnectTimeout=15".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
    ];
    if dev.port != 22 && dev.port != 0 {
        parts.push("-p".to_string());
        parts.push(dev.port.to_string());
    }
    if let Some(jump) = dev
        .jump_host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        validate_ssh_token("jump_host", jump)?;
        parts.push("-J".to_string());
        parts.push(jump.to_string());
    }
    if dev.legacy_ssh {
        parts.extend([
            "-o".to_string(),
            "KexAlgorithms=+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1".to_string(),
            "-o".to_string(),
            "HostKeyAlgorithms=+ssh-rsa".to_string(),
            "-o".to_string(),
            "PubkeyAcceptedAlgorithms=+ssh-rsa".to_string(),
        ]);
    }
    parts.push(format!("{user}@{}", dev.host.trim()));
    Ok(parts.join(" "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NetCmdRisk {
    ReadOnly,
    Mutation,
    Dangerous,
}

fn is_dangerous_segment(low: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "reload",
        "reboot",
        "poweroff",
        "halt",
        "erase",
        "write erase",
        "wr erase",
        "delete ",
        "format ",
        "squeeze ",
        "reset saved-configuration",
        "reset saved",
        "request system reboot",
        "request system halt",
        "request system power-off",
        "request system zeroize",
        "execute factoryreset",
        "execute reboot",
        "execute shutdown",
        "/system reboot",
        "/system shutdown",
        "/system reset-configuration",
        "no router bgp",
        "no router ospf",
        "clear bgp",
        "clear ip bgp",
        "clear route",
        "reset bgp",
        "rm -rf",
        "mkfs",
        "dd if=",
        "iptables -f",
        "ifdown ",
    ];
    if PREFIXES
        .iter()
        .any(|p| low == p.trim() || low.starts_with(p))
    {
        return true;
    }
    if low == "shutdown" || low == "shut" || low.ends_with(" shutdown") {
        // `no shutdown` and `show ...` are safe/mutation, never bare shutdown.
        if !low.starts_with("no ") && !low.starts_with("show ") && !low.starts_with("display ") {
            return true;
        }
    }
    false
}

fn is_safe_pipe_filter(rhs: &str) -> bool {
    let tok = rhs.split_whitespace().next().unwrap_or("");
    matches!(
        tok,
        "i" | "inc"
            | "include"
            | "e"
            | "exc"
            | "exclude"
            | "b"
            | "beg"
            | "begin"
            | "sec"
            | "section"
            | "count"
            | "match"
            | "grep"
            | "egrep"
            | "head"
            | "tail"
            | "no-more"
            | "nomore"
    )
}

fn classify_single_segment(vendor: Vendor, seg: &str) -> NetCmdRisk {
    let low = seg
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if low.is_empty() {
        return NetCmdRisk::ReadOnly;
    }
    if is_dangerous_segment(&low) {
        return NetCmdRisk::Dangerous;
    }
    // Check safe CLI pipe filter on a single `|` (`show ... | inc ...`)
    if let Some((lhs, rhs)) = low.split_once('|') {
        if !rhs.contains('|') && is_safe_pipe_filter(rhs) {
            return classify_single_segment(vendor, lhs);
        }
        return NetCmdRisk::Mutation;
    }

    // Mutating keywords anywhere invalidate read-only classification.
    if low.contains(" set ")
        || low.contains(" add ")
        || low.contains(" remove ")
        || low.contains(" delete ")
    {
        // Exception: `set cli screen-length` is read-only paging control.
        if !low.starts_with("set cli ") {
            return NetCmdRisk::Mutation;
        }
    }

    match vendor {
        Vendor::MikrotikRouterOs => {
            if (low.starts_with('/') || low.starts_with("tool "))
                && (low.ends_with(" print")
                    || low.contains(" print ")
                    || low.ends_with(" export")
                    || low == "export")
                && !low.contains("file=")
            {
                return NetCmdRisk::ReadOnly;
            }
            if low.starts_with("ping ")
                || low.starts_with("/ping ")
                || low.starts_with("/tool traceroute")
            {
                return NetCmdRisk::ReadOnly;
            }
            NetCmdRisk::Mutation
        }
        Vendor::FortinetFortiOs => {
            if low.starts_with("get ")
                || low.starts_with("show ")
                || low.starts_with("diagnose hardware ")
                || low.starts_with("diagnose netlink ")
                || low.starts_with("diagnose ip ")
                || low.starts_with("execute ping")
                || low.starts_with("execute traceroute")
            {
                return NetCmdRisk::ReadOnly;
            }
            NetCmdRisk::Mutation
        }
        Vendor::Linux => {
            const LINUX_RO: &[&str] = &[
                "ip addr",
                "ip -br ",
                "ip route",
                "ip -s link",
                "ip link show",
                "ip neigh",
                "ss ",
                "netstat ",
                "ping ",
                "traceroute ",
                "mtr ",
                "dig ",
                "nslookup ",
                "bgpctl show",
                "uname",
                "uptime",
                "df ",
                "free",
                "ls ",
                "cat ",
            ];
            if LINUX_RO
                .iter()
                .any(|p| low == p.trim() || low.starts_with(p))
                && !low.contains('>')
            {
                return NetCmdRisk::ReadOnly;
            }
            if low.starts_with("vtysh -c ") && (low.contains("\"show ") || low.contains("'show ")) {
                return NetCmdRisk::ReadOnly;
            }
            NetCmdRisk::Mutation
        }
        _ => {
            const RO_PREFIXES: &[&str] = &[
                "show ",
                "show",
                "display ",
                "dis ",
                "ping ",
                "traceroute ",
                "tracert ",
                "mtr ",
                "dir",
                "pwd",
                "terminal length ",
                "terminal width ",
                "screen-length ",
                "set cli screen-length",
                "set cli screen-width",
                "file list ",
                "request support information",
            ];
            if RO_PREFIXES
                .iter()
                .any(|p| low == p.trim() || low.starts_with(p))
            {
                return NetCmdRisk::ReadOnly;
            }
            if (low == "sh" || low.starts_with("sh ")) && vendor != Vendor::Generic {
                return NetCmdRisk::ReadOnly;
            }
            NetCmdRisk::Mutation
        }
    }
}

/// Classifies one or more CLI commands for a network device by worst-case risk.
pub fn classify_network_command(vendor: Vendor, cmd: &str) -> NetCmdRisk {
    let mut worst = NetCmdRisk::ReadOnly;
    for line in cmd.split(['\n', '\r', ';']) {
        for piece in line.split("&&").flat_map(|s| s.split("||")) {
            let risk = classify_single_segment(vendor, piece);
            if risk > worst {
                worst = risk;
            }
        }
    }
    worst
}

fn has_cli_error(output: &str) -> Option<&'static str> {
    let low = output.to_ascii_lowercase();
    for (pat, label) in [
        ("% invalid input", "comando inválido"),
        ("% incomplete command", "comando incompleto"),
        ("% ambiguous command", "comando ambíguo"),
        (
            "error: unrecognized command",
            "comando não reconhecido (VRP)",
        ),
        ("error: incomplete command", "comando incompleto (VRP)"),
        ("error: wrong parameter", "parâmetro inválido (VRP)"),
        ("syntax error", "erro de sintaxe"),
        ("bad command name", "comando inválido (RouterOS)"),
        ("command parse error", "erro de sintaxe (FortiOS)"),
        ("unknown command", "comando desconhecido"),
    ] {
        if low.contains(pat) {
            return Some(label);
        }
    }
    None
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make_with(
            "net_inventory",
            "Lista dispositivos do inventário de rede (`.zerith/inventory.toml` e global). \
             Suporta filtros por `tag`, `vendor`, `role` ou `query`. Nunca expõe senhas ou segredos.",
            json!({"type": "object", "properties": {
                "tag": {"type": "string"},
                "vendor": {"type": "string"},
                "role": {"type": "string"},
                "query": {"type": "string"}
            }}),
            "lista de dispositivos com host, usuário, vendor, role e flags de conexão",
            Permission::Read,
            5,
            true,
            |_, ctx| Ok(Some(Action::Read(ctx.workspace.join(".zerith")))),
            net_inventory,
        ),
        make_with(
            "net_connect",
            "Abre uma sessão SSH em um roteador/switch/firewall do inventário (`device`) ou \
             por parâmetros (`host`, `user`, `vendor`), usando `secret_ref` do cofre do sistema \
             sem revelar a senha e desativando paginação do equipamento automaticamente.",
            json!({"type": "object", "properties": {
                "device": {"type": "string", "description": "nome no inventory.toml"},
                "host": {"type": "string"},
                "user": {"type": "string"},
                "port": {"type": "integer", "description": "padrão 22"},
                "vendor": {"type": "string", "description": "cisco_ios|cisco_nxos|cisco_xr|huawei_vrp|juniper_junos|mikrotik_router_os|fortinet_forti_os|linux|generic"},
                "role": {"type": "string", "description": "core|border|distribution|access|firewall|lab|server"},
                "secret_ref": {"type": "string", "description": "chave no cofre (zerith secret set <nome>)"},
                "jump_host": {"type": "string"},
                "legacy_ssh": {"type": "boolean"},
                "disable_paging": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "padrão 25"}
            }}),
            "id do terminal (`term-N`), banner inicial e confirmação de paginação",
            Permission::Shell,
            130,
            false,
            connect_action,
            net_connect,
        ),
        make_with(
            "terminal_batch",
            "Executa uma sequência de comandos em um terminal aberto (`terminal_open` ou `net_connect`), \
             separando a saída por comando com detecção automática de paginação e de erros de CLI.",
            json!({"type": "object", "properties": {
                "id": {"type": "string"},
                "commands": {"type": "array", "items": {"type": "string"}},
                "stop_on_error": {"type": "boolean", "description": "padrão true"},
                "auto_page": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "timeout por comando, padrão 20"}
            }, "required": ["id", "commands"]}),
            "saída estruturada de cada comando",
            Permission::Shell,
            610,
            false,
            batch_action,
            terminal_batch,
        ),
    ]
}

fn resolve_connect_device(args: &Value, ctx: &CallCtx<'_>) -> Result<DeviceEntry> {
    if let Some(name) = opt_str(args, "device") {
        let root = zerith_core::Paths::resolve().root().to_path_buf();
        let inv = load_inventory(ctx.workspace, Some(&root)).map_err(ToolError::Failed)?;
        let mut dev = inv
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                ToolError::Failed(format!(
                    "dispositivo `{name}` não encontrado no inventory.toml"
                ))
            })?;
        if let Some(u) = opt_str(args, "user") {
            dev.user = u.to_string();
        }
        if let Some(p) = args["port"].as_u64() {
            dev.port = p.min(65535) as u16;
        }
        if let Some(s) = opt_str(args, "secret_ref") {
            dev.secret_ref = Some(s.to_string());
        }
        return Ok(dev);
    }
    let host = str_arg(args, "host")?.to_string();
    let user = opt_str(args, "user").unwrap_or("").to_string();
    let vendor = opt_str(args, "vendor")
        .and_then(Vendor::parse)
        .unwrap_or(Vendor::Generic);
    let role = opt_str(args, "role")
        .and_then(DeviceRole::parse)
        .unwrap_or(DeviceRole::Lab);
    Ok(DeviceEntry {
        name: host.clone(),
        host,
        port: args["port"].as_u64().unwrap_or(22).min(65535) as u16,
        user,
        vendor,
        role,
        secret_ref: opt_str(args, "secret_ref").map(str::to_string),
        jump_host: opt_str(args, "jump_host").map(str::to_string),
        legacy_ssh: args["legacy_ssh"].as_bool().unwrap_or(false),
        tags: Vec::new(),
        description: None,
    })
}

fn connect_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    match resolve_connect_device(args, ctx) {
        Ok(dev) => match build_ssh_command(&dev) {
            Ok(cmd) => Ok(Some(Action::Shell(cmd))),
            Err(_) => Ok(Some(Action::Shell(format!("ssh {}", dev.host)))),
        },
        Err(_) => Ok(Some(Action::Shell("ssh net_connect".into()))),
    }
}

fn batch_action(args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
    let id = args["id"].as_str().unwrap_or("");
    let prog = terminal::terminal_program(id).unwrap_or_else(|| "terminal".into());
    let cmds: Vec<&str> = args["commands"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if cmds.is_empty() {
        return Ok(None);
    }
    Ok(Some(Action::Shell(format!(
        "[{id}: {prog}] batch: {}",
        cmds.join("; ")
    ))))
}

fn net_inventory(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let devices = load_inventory(ctx.workspace, Some(&root)).map_err(ToolError::Failed)?;
    let tag_f = opt_str(args, "tag").map(str::to_ascii_lowercase);
    let vendor_f = opt_str(args, "vendor").and_then(Vendor::parse);
    let role_f = opt_str(args, "role").and_then(DeviceRole::parse);
    let query_f = opt_str(args, "query").map(str::to_ascii_lowercase);

    let filtered: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| {
            if let Some(t) = &tag_f {
                if !d.tags.iter().any(|x| x.to_ascii_lowercase() == *t) {
                    return false;
                }
            }
            if let Some(v) = vendor_f {
                if d.vendor != v {
                    return false;
                }
            }
            if let Some(r) = role_f {
                if d.role != r {
                    return false;
                }
            }
            if let Some(q) = &query_f {
                let hit = d.name.to_ascii_lowercase().contains(q)
                    || d.host.to_ascii_lowercase().contains(q)
                    || d.description
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(q);
                if !hit {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        return Ok("nenhum dispositivo encontrado em .zerith/inventory.toml".into());
    }
    let mut out = Vec::with_capacity(filtered.len());
    for d in filtered {
        let mut meta = vec![
            format!("{}:{}", d.host, d.port),
            format!("user={}", if d.user.is_empty() { "-" } else { &d.user }),
            format!("vendor={}", d.vendor.as_str()),
            format!("role={}", d.role.as_str()),
        ];
        if let Some(j) = &d.jump_host {
            meta.push(format!("jump={j}"));
        }
        if d.legacy_ssh {
            meta.push("legacy_ssh=true".into());
        }
        if d.secret_ref.is_some() {
            meta.push("secret_ref=configurado".into());
        }
        if !d.tags.is_empty() {
            meta.push(format!("tags=[{}]", d.tags.join(",")));
        }
        out.push(format!("- **{}** ({})", d.name, meta.join(" · ")));
    }
    Ok(out.join("\n"))
}

fn net_connect(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let dev = resolve_connect_device(args, ctx)?;
    let cmd = build_ssh_command(&dev).map_err(ToolError::BadArguments)?;
    let mut secrets = Vec::new();
    let password = match dev.secret_ref.as_deref() {
        Some(secret_name) => {
            let val = zerith_core::secrets::get_secret(secret_name).map_err(|e| {
                ToolError::Failed(format!(
                    "segredo `{secret_name}` não encontrado no cofre ({e}); peça: zerith secret set {secret_name}"
                ))
            })?;
            secrets.push(val.clone());
            Some(val)
        }
        None => None,
    };

    let secs = args["timeout_secs"].as_u64().unwrap_or(25).clamp(5, 120);
    let disable_paging = args["disable_paging"].as_bool().unwrap_or(true);
    let re = Regex::new(r"(?i)(password:|passphrase.*:|>|#|\]\s*>|\$\s*$)").ok();

    let (id, mut banner, mut alive) = terminal::open_with_meta(
        OpenSpec {
            command: &cmd,
            cwd: ctx.workspace,
            expect: re.as_ref(),
            timeout_dur: Duration::from_secs(secs),
            auto_page: true,
            secrets,
            vendor: Some(dev.vendor.as_str().into()),
            device: Some(dev.name.clone()),
            host: Some(dev.host.clone()),
            role: Some(dev.role.as_str().into()),
        },
        exec,
    )?;

    if alive {
        let lower = banner.to_ascii_lowercase();
        if (lower.contains("password:") || lower.contains("passphrase")) && password.is_some() {
            let (after_pw, pw_alive) = terminal::send_session_command(
                "net_connect",
                &id,
                None,
                password,
                true,
                None,
                Duration::from_secs(secs),
                true,
                exec,
            )?;
            alive = pw_alive;
            if !after_pw.is_empty() {
                banner.push('\n');
                banner.push_str(&after_pw);
            }
        }
    }

    if alive && disable_paging {
        if let Some(paging_cmd) = dev.vendor.disable_paging_cmd() {
            let (pg_out, pg_alive) = terminal::send_session_command(
                "net_connect",
                &id,
                Some(paging_cmd),
                None,
                true,
                None,
                Duration::from_secs(10),
                true,
                exec,
            )?;
            alive = pg_alive;
            if !pg_out.is_empty() {
                banner.push_str(&format!("\n[paginação desativada: {paging_cmd}]\n{pg_out}"));
            }
        }
    }

    Ok(format!(
        "{id} conectado a {} ({}, vendor={}, role={}){}\n{banner}",
        dev.name,
        dev.host,
        dev.vendor.as_str(),
        dev.role.as_str(),
        if alive { "" } else { " — sessão encerrou" }
    ))
}

fn terminal_batch(args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    let cmds: Vec<String> = args["commands"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if cmds.is_empty() {
        return Err(ToolError::BadArguments(
            "commands precisa ter ao menos um comando".into(),
        ));
    }
    let stop_on_error = args["stop_on_error"].as_bool().unwrap_or(true);
    let auto_page = args["auto_page"].as_bool().unwrap_or(true);
    let per_cmd_secs = args["timeout_secs"].as_u64().unwrap_or(20).clamp(2, 300);

    let mut sections = Vec::with_capacity(cmds.len());
    for (idx, cmd) in cmds.iter().enumerate() {
        if exec.cancel.is_cancelled() {
            sections.push(format!(
                "### [{}/{}] {cmd}\n[cancelado]",
                idx + 1,
                cmds.len()
            ));
            break;
        }
        let (out, alive) = terminal::send_session_command(
            "terminal_batch",
            id,
            Some(cmd),
            None,
            true,
            None,
            Duration::from_secs(per_cmd_secs),
            auto_page,
            exec,
        )?;
        sections.push(format!("### {cmd}\n{out}"));
        if !alive {
            sections.push("[sessão encerrou durante o lote]".into());
            break;
        }
        if let Some(err_kind) = has_cli_error(&out) {
            if stop_on_error {
                sections.push(format!(
                    "[lote interrompido no comando {}/{}: {err_kind}]",
                    idx + 1,
                    cmds.len()
                ));
                break;
            }
        }
    }
    Ok(sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_parses_and_merges_workspace_over_global() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(ws.join(".zerith")).unwrap();
        std::fs::write(
            root.join("inventory.toml"),
            r#"
[[devices]]
name = "core-01"
host = "10.0.0.1"
user = "admin"
vendor = "cisco_ios"
role = "core"
tags = ["global"]
"#,
        )
        .unwrap();
        std::fs::write(
            ws.join(".zerith/inventory.toml"),
            r#"
[[devices]]
name = "core-01"
host = "10.0.0.99"
user = "netops"
vendor = "huawei_vrp"
role = "border"
secret_ref = "netops_key"
jump_host = "bastion@192.0.2.5"
legacy_ssh = true
tags = ["bgp"]
"#,
        )
        .unwrap();

        let list = load_inventory(&ws, Some(&root)).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].host, "10.0.0.99");
        assert_eq!(list[0].vendor, Vendor::HuaweiVrp);
        assert_eq!(list[0].role, DeviceRole::Border);
        assert!(list[0].legacy_ssh);

        let ssh = build_ssh_command(&list[0]).unwrap();
        assert!(ssh.contains("-J bastion@192.0.2.5"), "{ssh}");
        assert!(ssh.contains("diffie-hellman-group14-sha1"), "{ssh}");
        assert!(ssh.contains("netops@10.0.0.99"), "{ssh}");
    }

    #[test]
    fn ssh_builder_rejects_flag_and_shell_injection() {
        let mut dev = DeviceEntry {
            host: "-oProxyCommand=id".into(),
            user: "admin".into(),
            ..Default::default()
        };
        assert!(build_ssh_command(&dev).is_err());
        dev.host = "10.0.0.1;id".into();
        assert!(build_ssh_command(&dev).is_err());
    }

    #[test]
    fn network_command_classifier_distinguishes_readonly_mutation_and_dangerous() {
        assert_eq!(
            classify_network_command(Vendor::CiscoIos, "show ip bgp summary | inc Estab"),
            NetCmdRisk::ReadOnly
        );
        assert_eq!(
            classify_network_command(Vendor::HuaweiVrp, "display bgp peer"),
            NetCmdRisk::ReadOnly
        );
        assert_eq!(
            classify_network_command(Vendor::JuniperJunos, "show route 192.0.2.0/24"),
            NetCmdRisk::ReadOnly
        );
        assert_eq!(
            classify_network_command(Vendor::MikrotikRouterOs, "/ip route print detail"),
            NetCmdRisk::ReadOnly
        );
        assert_eq!(
            classify_network_command(Vendor::FortinetFortiOs, "get system status"),
            NetCmdRisk::ReadOnly
        );

        assert_eq!(
            classify_network_command(Vendor::CiscoIos, "configure terminal"),
            NetCmdRisk::Mutation
        );
        assert_eq!(
            classify_network_command(
                Vendor::MikrotikRouterOs,
                "/ip route add dst-address=0.0.0.0/0 gateway=10.0.0.1"
            ),
            NetCmdRisk::Mutation
        );
        assert_eq!(
            classify_network_command(Vendor::CiscoIos, "show run; reload"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::HuaweiVrp, "reset saved-configuration"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::JuniperJunos, "request system reboot"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::MikrotikRouterOs, "/system reset-configuration"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::FortinetFortiOs, "execute factoryreset"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::CiscoIos, "shutdown"),
            NetCmdRisk::Dangerous
        );
        assert_eq!(
            classify_network_command(Vendor::CiscoIos, "no shutdown"),
            NetCmdRisk::Mutation
        );
    }

    #[test]
    fn vendor_disable_paging_cmds_match_platforms() {
        assert_eq!(
            Vendor::CiscoIos.disable_paging_cmd(),
            Some("terminal length 0")
        );
        assert_eq!(
            Vendor::HuaweiVrp.disable_paging_cmd(),
            Some("screen-length 0 temporary")
        );
        assert_eq!(
            Vendor::JuniperJunos.disable_paging_cmd(),
            Some("set cli screen-length 0")
        );
        assert_eq!(Vendor::MikrotikRouterOs.disable_paging_cmd(), None);
    }
}
