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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeviceTransport {
    #[default]
    Ssh,
    Telnet,
    Serial,
}

impl DeviceTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Telnet => "telnet",
            Self::Serial => "serial",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ssh" => Some(Self::Ssh),
            "telnet" => Some(Self::Telnet),
            "serial" | "console" | "com" | "tty" => Some(Self::Serial),
            _ => None,
        }
    }
}

fn default_ssh_port() -> u16 {
    22
}

fn default_baud_rate() -> u32 {
    9600
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
    pub transport: DeviceTransport,
    pub serial_port: Option<String>,
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
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
            transport: DeviceTransport::Ssh,
            serial_port: None,
            baud_rate: 9600,
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
        .filter(|d| {
            !d.name.trim().is_empty()
                && (!d.host.trim().is_empty()
                    || d.serial_port
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty()))
        })
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
        "StrictHostKeyChecking=accept-new".to_string(),
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

/// Builds a native in-process TCP Telnet URI (`telnet://host:port`) handled
/// directly by `terminal::spawn` with RFC 854 IAC negotiation and `TCP_NODELAY`.
pub fn build_telnet_command(dev: &DeviceEntry) -> std::result::Result<String, String> {
    validate_ssh_token("host", &dev.host)?;
    let port = if dev.port == 0 { 23 } else { dev.port };
    Ok(format!("telnet://{}:{port}", dev.host.trim()))
}

fn validate_serial_port(raw: &str) -> std::result::Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("serial_port não pode ficar vazio (ex.: COM3 ou /dev/ttyUSB0)".into());
    }
    let upper = t.to_ascii_uppercase();
    if let Some(num) = upper.strip_prefix("COM") {
        if !num.is_empty() && num.len() <= 3 && num.chars().all(|c| c.is_ascii_digit()) {
            return Ok(upper);
        }
    }
    if (t.starts_with("/dev/tty") || t.starts_with("/dev/cu.") || t.starts_with("/dev/pts/"))
        && t[5..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return Ok(t.to_string());
    }
    Err(format!(
        "serial_port inválido: `{t}` (use COM1..COM999 no Windows ou /dev/ttyUSB0, /dev/ttyS0, /dev/cu.* no Linux/macOS)"
    ))
}

/// Builds a zero-dependency Serial Console command (`COMx` on Windows via .NET
/// `System.IO.Ports.SerialPort`, or `stty` raw bridge on Linux/macOS).
pub fn build_serial_command(dev: &DeviceEntry) -> std::result::Result<String, String> {
    let port_raw = dev
        .serial_port
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| dev.host.trim());
    let port = validate_serial_port(port_raw)?;
    let baud = if dev.baud_rate == 0 {
        9600
    } else {
        dev.baud_rate.clamp(1200, 921_600)
    };
    if port.starts_with("COM") {
        Ok(format!(
            "powershell -NoLogo -NoProfile -NonInteractive -Command \"$p=New-Object System.IO.Ports.SerialPort('{port}',{baud},'None',8,'One');$p.DtrEnable=$true;$p.RtsEnable=$true;$p.ReadTimeout=50;$p.Open();try{{while($p.IsOpen){{try{{$s=$p.ReadExisting();if($s){{[Console]::Write($s)}}}}catch{{}};if([Console]::KeyAvailable){{$k=[Console]::ReadKey($true);$p.Write([string]$k.KeyChar)}};Start-Sleep -Milliseconds 10}}}}finally{{$p.Close()}}\""
        ))
    } else {
        let stty_flag = if cfg!(target_os = "macos") {
            "-f"
        } else {
            "-F"
        };
        Ok(format!(
            "sh -c 'stty {stty_flag} {port} {baud} cs8 -parenb -cstopb raw -echo && (cat {port} & cat > {port})'"
        ))
    }
}

/// Dispatches connection command creation across SSH, Telnet, and Serial Console.
pub fn build_connect_command(dev: &DeviceEntry) -> std::result::Result<String, String> {
    match dev.transport {
        DeviceTransport::Ssh => build_ssh_command(dev),
        DeviceTransport::Telnet => build_telnet_command(dev),
        DeviceTransport::Serial => build_serial_command(dev),
    }
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
            "Abre uma sessão interativa de alta velocidade via SSH, Telnet nativo ou Porta Serial (Console COMx / /dev/ttyUSBx) \
             em um roteador/switch/firewall/OLT do inventário (`device`) ou por parâmetros (`host`/`serial_port`, `transport`, `user`, `vendor`), \
             fazendo login automático (`Username:`/`login:` + `Password:` via `secret_ref` ou `password`), acordando console serial se necessário \
             e desativando a paginação do equipamento automaticamente.",
            json!({"type": "object", "properties": {
                "device": {"type": "string", "description": "nome no inventory.toml"},
                "transport": {"type": "string", "description": "ssh (padrão) | telnet | serial"},
                "host": {"type": "string", "description": "IP/hostname (ou porta serial como COM3 / /dev/ttyUSB0)"},
                "serial_port": {"type": "string", "description": "porta serial explícita (ex.: COM3, /dev/ttyUSB0, /dev/ttyS0)"},
                "baud_rate": {"type": "integer", "description": "velocidade da serial (ex.: 9600, 115200), padrão 9600"},
                "user": {"type": "string"},
                "port": {"type": "integer", "description": "padrão 22 para SSH, 23 para Telnet"},
                "vendor": {"type": "string", "description": "cisco_ios|cisco_nxos|cisco_xr|huawei_vrp|juniper_junos|mikrotik_router_os|fortinet_forti_os|linux|generic"},
                "role": {"type": "string", "description": "core|border|distribution|access|firewall|lab|server"},
                "secret_ref": {"type": "string", "description": "chave no cofre (zerith secret set <nome>)"},
                "password": {"type": "string", "description": "senha direta opcional quando não estiver no cofre"},
                "jump_host": {"type": "string"},
                "legacy_ssh": {"type": "boolean"},
                "disable_paging": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "padrão 25"}
            }}),
            "id do terminal (`term-N`), transporte, banner inicial e confirmação de paginação",
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
        make_with(
            "net_diff",
            "Compara duas configurações de roteador/switch/firewall (`before` e `after`, como caminhos de arquivo \
             ou texto inline) de forma hierárquica por bloco (`interface`, `router bgp`, `ip vrf`, `route-policy`, etc.), \
             ignorando ruído volátil (timestamps, `ntp clock-period`) e gerando automaticamente o bloco de Rollback \
             (`no ...` para Cisco / `undo ...` para Huawei VRP). Por padrão mostra 100% da configuração real sem mascarar \
             nada (`sanitize_secrets: false`); ative `sanitize_secrets: true` apenas quando quiser ocultar senhas/chaves \
             para relatórios externos.",
            json!({"type": "object", "properties": {
                "before": {"type": "string", "description": "caminho do arquivo ou texto da configuração anterior"},
                "after": {"type": "string", "description": "caminho do arquivo ou texto da configuração nova"},
                "vendor": {"type": "string", "description": "cisco_ios|cisco_nxos|cisco_xr|huawei_vrp|juniper_junos|mikrotik_router_os|fortinet_forti_os|generic (auto-detectado se omitido)"},
                "sanitize_secrets": {"type": "boolean", "description": "padrão false (visibilidade total para consultoria); true mascara segredos para relatório externo"}
            }, "required": ["before", "after"]}),
            "diff hierárquico por bloco e script de rollback pronto para colar",
            Permission::Read,
            15,
            true,
            |_, ctx| Ok(Some(Action::Read(ctx.workspace.to_path_buf()))),
            net_diff,
        ),
        make_with(
            "net_probe",
            "Sonda conectividade TCP concorrente, resolução DNS, RTT em ms e banner inicial de gerência/roteamento \
             (SSH, BGP 179, NETCONF 830, Winbox 8291, MikroTik API 8728/8729, HTTP/HTTPS 80/443/8443) para um ou mais hosts \
             sem depender de nmap/nc instalados.",
            json!({"type": "object", "properties": {
                "host": {"type": "string", "description": "hostname ou IP único"},
                "hosts": {"type": "array", "items": {"type": "string"}, "description": "lista de hostnames ou IPs"},
                "ports": {"type": "array", "items": {"type": "integer"}, "description": "padrão [22, 80, 179, 443, 830, 8291, 8443, 8728, 8729]"},
                "timeout_ms": {"type": "integer", "description": "timeout TCP por porta em ms, padrão 1500"},
                "grab_banner": {"type": "boolean", "description": "lê banner inicial (ex.: SSH-2.0-...), padrão true"}
            }}),
            "tabela markdown com DNS, status da porta, RTT em ms e banner do equipamento",
            Permission::Network,
            30,
            true,
            |args, _| {
                let h = opt_str(args, "host").unwrap_or("net_probe");
                Ok(Some(Action::Network(h.to_string())))
            },
            net_probe,
        ),
    ]
}

fn infer_transport(
    explicit: Option<DeviceTransport>,
    host: &str,
    serial_port: Option<&str>,
    port_arg: Option<u16>,
) -> DeviceTransport {
    if let Some(t) = explicit {
        return t;
    }
    if serial_port.is_some_and(|s| !s.trim().is_empty()) {
        return DeviceTransport::Serial;
    }
    let h_up = host.trim().to_ascii_uppercase();
    if h_up.starts_with("COM") && h_up[3..].chars().all(|c| c.is_ascii_digit()) && h_up.len() > 3 {
        return DeviceTransport::Serial;
    }
    if host.trim().starts_with("/dev/tty") || host.trim().starts_with("/dev/cu.") {
        return DeviceTransport::Serial;
    }
    if port_arg == Some(23) {
        return DeviceTransport::Telnet;
    }
    DeviceTransport::Ssh
}

fn resolve_connect_device(args: &Value, ctx: &CallCtx<'_>) -> Result<DeviceEntry> {
    let explicit_transport = opt_str(args, "transport").and_then(DeviceTransport::parse);
    let serial_override = opt_str(args, "serial_port").map(str::to_string);
    let baud_override = args["baud_rate"].as_u64().map(|b| b as u32);
    let port_override = args["port"].as_u64().map(|p| p.min(65535) as u16);

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
        if let Some(t) = explicit_transport {
            dev.transport = t;
        }
        if let Some(sp) = serial_override {
            dev.serial_port = Some(sp);
            if explicit_transport.is_none() {
                dev.transport = DeviceTransport::Serial;
            }
        }
        if let Some(b) = baud_override {
            dev.baud_rate = b;
        }
        if let Some(u) = opt_str(args, "user") {
            dev.user = u.to_string();
        }
        if let Some(p) = port_override {
            dev.port = p;
        } else if dev.transport == DeviceTransport::Telnet && dev.port == 22 {
            dev.port = 23;
        }
        if let Some(s) = opt_str(args, "secret_ref") {
            dev.secret_ref = Some(s.to_string());
        }
        return Ok(dev);
    }

    let host = opt_str(args, "host")
        .or_else(|| opt_str(args, "serial_port"))
        .ok_or_else(|| {
            ToolError::BadArguments(
                "informe `device`, `host` ou `serial_port` para `net_connect`".into(),
            )
        })?
        .to_string();
    let transport = infer_transport(
        explicit_transport,
        &host,
        serial_override.as_deref(),
        port_override,
    );
    let default_port = if transport == DeviceTransport::Telnet {
        23
    } else {
        22
    };
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
        port: port_override.unwrap_or(default_port),
        user,
        vendor,
        role,
        transport,
        serial_port: serial_override,
        baud_rate: baud_override.unwrap_or(9600),
        secret_ref: opt_str(args, "secret_ref").map(str::to_string),
        jump_host: opt_str(args, "jump_host").map(str::to_string),
        legacy_ssh: args["legacy_ssh"].as_bool().unwrap_or(false),
        tags: Vec::new(),
        description: None,
    })
}

fn connect_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    match resolve_connect_device(args, ctx) {
        Ok(dev) => match build_connect_command(&dev) {
            Ok(cmd) => Ok(Some(Action::Shell(cmd))),
            Err(_) => Ok(Some(Action::Shell(format!(
                "{} {}",
                dev.transport.as_str(),
                dev.host
            )))),
        },
        Err(_) => Ok(Some(Action::Shell("net_connect".into()))),
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
                    || d.serial_port
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(q)
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
        let endpoint = match d.transport {
            DeviceTransport::Serial => format!(
                "serial={}@{}bps",
                d.serial_port.as_deref().unwrap_or(&d.host),
                d.baud_rate
            ),
            DeviceTransport::Telnet => format!("telnet={}:{}", d.host, d.port),
            DeviceTransport::Ssh => format!("ssh={}:{}", d.host, d.port),
        };
        let mut meta = vec![
            endpoint,
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

fn banner_requests_username(text: &str) -> bool {
    let tail = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    tail.ends_with("username:")
        || tail.ends_with("login:")
        || tail.ends_with("user:")
        || tail.ends_with("user name:")
        || tail.ends_with("login as:")
}

fn banner_requests_password(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let tail = lower
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("");
    tail.contains("password:") || tail.contains("passphrase")
}

fn net_connect(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let dev = resolve_connect_device(args, ctx)?;
    let cmd = build_connect_command(&dev).map_err(ToolError::BadArguments)?;
    let mut secrets = Vec::new();
    let password = if let Some(direct_pw) = opt_str(args, "password") {
        let val = direct_pw.to_string();
        secrets.push(val.clone());
        Some(val)
    } else if let Some(secret_name) = dev.secret_ref.as_deref() {
        let val = zerith_core::secrets::get_secret(secret_name).map_err(|e| {
            ToolError::Failed(format!(
                "segredo `{secret_name}` não encontrado no cofre ({e}); peça: zerith secret set {secret_name}"
            ))
        })?;
        secrets.push(val.clone());
        Some(val)
    } else {
        None
    };

    let secs = args["timeout_secs"].as_u64().unwrap_or(25).clamp(5, 120);
    let disable_paging = args["disable_paging"].as_bool().unwrap_or(true);
    let re = Regex::new(
        r"(?i)(username\s*:|login\s*:|user\s*name\s*:|password\s*:|passphrase.*:|>|#|\]\s*>|\]\s*$|\$\s*$)",
    )
    .ok();

    let open_timeout = if dev.transport == DeviceTransport::Serial {
        Duration::from_secs(secs.min(3))
    } else {
        Duration::from_secs(secs)
    };

    let (id, mut banner, mut alive) = terminal::open_with_meta(
        OpenSpec {
            command: &cmd,
            cwd: ctx.workspace,
            expect: re.as_ref(),
            timeout_dur: open_timeout,
            auto_page: true,
            secrets,
            vendor: Some(dev.vendor.as_str().into()),
            device: Some(dev.name.clone()),
            host: Some(dev.serial_port.clone().unwrap_or_else(|| dev.host.clone())),
            role: Some(dev.role.as_str().into()),
        },
        exec,
    )?;

    // 1. On Serial Console, an already-booted router often stays silent until an initial Enter (\r) is sent.
    if alive
        && dev.transport == DeviceTransport::Serial
        && (banner.trim().is_empty()
            || banner.contains("[tempo esgotado")
            || banner.contains("[sem saída"))
    {
        let (wake_out, wake_alive) = terminal::send_session_command(
            "net_connect",
            &id,
            Some(""),
            None,
            true,
            re.as_ref(),
            Duration::from_secs(4),
            true,
            exec,
        )?;
        alive = wake_alive;
        if !wake_out.is_empty() {
            banner = wake_out;
        }
    }

    // 2. On Telnet / Serial Console, handle automatic `Username:` / `login:` prompt before `Password:`.
    if alive && banner_requests_username(&banner) && !dev.user.trim().is_empty() {
        let (after_user, user_alive) = terminal::send_session_command(
            "net_connect",
            &id,
            Some(dev.user.trim()),
            None,
            true,
            re.as_ref(),
            Duration::from_secs(secs),
            true,
            exec,
        )?;
        alive = user_alive;
        if !after_user.is_empty() {
            banner.push('\n');
            banner.push_str(&after_user);
        }
    }

    // 3. Handle `Password:` / `Passphrase:` prompt automatically.
    if alive && banner_requests_password(&banner) && password.is_some() {
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

    // 4. Disable CLI pagination automatically once logged in.
    if alive
        && disable_paging
        && !banner_requests_username(&banner)
        && !banner_requests_password(&banner)
    {
        if let Some(paging_cmd) = dev.vendor.disable_paging_cmd() {
            let (pg_out, pg_alive) = terminal::send_session_command(
                "net_connect",
                &id,
                Some(paging_cmd),
                None,
                true,
                None,
                Duration::from_secs(8),
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
        "{id} conectado via {} a {} ({}, vendor={}, role={}){}\n{banner}",
        dev.transport.as_str(),
        dev.name,
        dev.serial_port.as_deref().unwrap_or(&dev.host),
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

fn load_config_input(raw: &str, workspace: &Path) -> String {
    if !raw.contains('\n') && raw.len() < 512 {
        let candidate = if Path::new(raw).is_absolute() {
            std::path::PathBuf::from(raw)
        } else {
            workspace.join(raw)
        };
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return text;
        }
    }
    raw.to_string()
}

fn detect_config_vendor(before: &str, after: &str) -> Vendor {
    let sample = format!("{before}\n{after}");
    let lower = sample.to_ascii_lowercase();
    if lower.contains("sysname ")
        || lower.contains("display current-configuration")
        || lower.contains("screen-length ")
        || lower.contains("\n undo ")
        || lower.contains("route-policy ")
    {
        Vendor::HuaweiVrp
    } else if lower.contains("/interface ")
        || lower.contains("/ip route")
        || lower.contains("/routing bgp")
    {
        Vendor::MikrotikRouterOs
    } else if lower.contains("config system ") && lower.contains("\nnext\n") {
        Vendor::FortinetFortiOs
    } else {
        Vendor::CiscoIos
    }
}

fn is_volatile_config_line(trimmed: &str) -> bool {
    if trimmed.is_empty() || trimmed == "!" || trimmed == "#" {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.starts_with('!') || trimmed.starts_with('#') {
        return true;
    }
    lower.starts_with("ntp clock-period ")
        || lower.starts_with("building configuration")
        || lower.starts_with("current configuration :")
        || lower.starts_with("version ")
        || matches!(lower.as_str(), "end" | "return" | "quit" | "exit")
}

fn is_stanza_header(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "interface ",
        "router ",
        "ip vrf ",
        "vrf definition ",
        "route-map ",
        "route-policy ",
        "ip access-list ",
        "ipv6 access-list ",
        "acl ",
        "line ",
        "bgp ",
        "ospf ",
        "isis ",
        "mpls",
        "vlan ",
        "user-interface ",
        "aaa",
        "address-family ",
        " ipv4-family ",
        "ipv4-family ",
        "policy-based-route ",
        "crypto ",
        "/",
    ];
    PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Optional on-demand secret sanitizer (only runs when the user explicitly sets
/// `sanitize_secrets: true` for sharing reports externally; never enabled by default).
pub fn sanitize_config_line_on_demand(line: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return line.to_string();
    }
    let mut out = Vec::with_capacity(tokens.len());
    let mut mask_rest = false;
    let mut skip_type_digit = false;
    for (i, tok) in tokens.iter().enumerate() {
        if mask_rest {
            if skip_type_digit
                && (tok.len() == 1 && tok.chars().all(|c| c.is_ascii_digit())
                    || *tok == "cipher"
                    || *tok == "simple"
                    || *tok == "encrypted")
            {
                out.push(*tok);
                skip_type_digit = false;
                continue;
            }
            out.push("<REDACTED>");
            break;
        }
        let low = tok.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "password" | "secret" | "cipher" | "pre-shared-key" | "md5" | "authentication-key"
        ) || (low == "community" && i > 0 && tokens[0].eq_ignore_ascii_case("snmp-server"))
        {
            out.push(*tok);
            mask_rest = true;
            skip_type_digit = true;
        } else {
            out.push(*tok);
        }
    }
    out.join(" ")
}

fn parse_config_stanzas(
    text: &str,
) -> (Vec<String>, std::collections::HashMap<String, Vec<String>>) {
    let mut order = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut current_stanza = "[global]".to_string();
    order.push(current_stanza.clone());
    map.insert(current_stanza.clone(), Vec::new());

    for (idx, &raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        if is_volatile_config_line(trimmed) {
            continue;
        }
        let indented = raw.starts_with(' ') || raw.starts_with('\t');
        let next_indented = lines.get(idx + 1).is_some_and(|nxt| {
            (nxt.starts_with(' ') || nxt.starts_with('\t')) && !is_volatile_config_line(nxt.trim())
        });
        let clean = trimmed.to_string();

        if !indented && (is_stanza_header(trimmed) || next_indented) {
            current_stanza = clean;
            if !map.contains_key(&current_stanza) {
                order.push(current_stanza.clone());
                map.insert(current_stanza.clone(), Vec::new());
            }
        } else {
            if !indented {
                current_stanza = "[global]".to_string();
            }
            let entry = map.entry(current_stanza.clone()).or_default();
            if !entry.contains(&clean) {
                entry.push(clean);
            }
        }
    }
    (order, map)
}

fn negate_cli_command(vendor: Vendor, cmd: &str) -> String {
    let neg = if vendor == Vendor::HuaweiVrp {
        "undo"
    } else {
        "no"
    };
    let neg_prefix = format!("{neg} ");
    if let Some(stripped) = cmd.strip_prefix(&neg_prefix) {
        return stripped.trim().to_string();
    }
    // For commands with values where Cisco/Huawei expects key-only or full line, full line `no <cmd>` / `undo <cmd>` is valid on modern IOS/VRP
    format!("{neg} {cmd}")
}

pub fn compute_hierarchical_net_diff(
    before: &str,
    after: &str,
    vendor_override: Option<Vendor>,
    sanitize_secrets: bool,
) -> String {
    let vendor = vendor_override.unwrap_or_else(|| detect_config_vendor(before, after));
    let (before_order, before_map) = parse_config_stanzas(before);
    let (after_order, after_map) = parse_config_stanzas(after);
    let fmt_line = |s: &str| {
        if sanitize_secrets {
            sanitize_config_line_on_demand(s)
        } else {
            s.to_string()
        }
    };

    let mut all_stanzas = before_order.clone();
    for s in &after_order {
        if !all_stanzas.contains(s) {
            all_stanzas.push(s.clone());
        }
    }

    let mut diff_sections = Vec::new();
    let mut rollback_lines = Vec::new();
    let mut changed_blocks = 0usize;
    let mut total_removed = 0usize;
    let mut total_added = 0usize;
    let exit_cmd = if vendor == Vendor::HuaweiVrp {
        "quit"
    } else {
        "exit"
    };

    for stanza in &all_stanzas {
        let b_cmds = before_map.get(stanza);
        let a_cmds = after_map.get(stanza);
        let stanza_disp = fmt_line(stanza);
        match (b_cmds, a_cmds) {
            (None, Some(added_cmds)) => {
                changed_blocks += 1;
                total_added += added_cmds.len().max(1);
                let mut block = format!("[+ bloco novo: {stanza_disp}]");
                for c in added_cmds {
                    block.push_str(&format!("\n  + {}", fmt_line(c)));
                }
                diff_sections.push(block);
                if stanza != "[global]" {
                    rollback_lines.push(negate_cli_command(vendor, &stanza_disp));
                } else {
                    for c in added_cmds {
                        rollback_lines.push(negate_cli_command(vendor, &fmt_line(c)));
                    }
                }
            }
            (Some(removed_cmds), None) => {
                changed_blocks += 1;
                total_removed += removed_cmds.len().max(1);
                let mut block = format!("[- bloco removido: {stanza_disp}]");
                for c in removed_cmds {
                    block.push_str(&format!("\n  - {}", fmt_line(c)));
                }
                diff_sections.push(block);
                if stanza != "[global]" {
                    rollback_lines.push(stanza_disp);
                    for c in removed_cmds {
                        rollback_lines.push(format!("  {}", fmt_line(c)));
                    }
                    rollback_lines.push(exit_cmd.to_string());
                } else {
                    for c in removed_cmds {
                        rollback_lines.push(fmt_line(c));
                    }
                }
            }
            (Some(b_list), Some(a_list)) => {
                let removed: Vec<&String> = b_list.iter().filter(|c| !a_list.contains(c)).collect();
                let added: Vec<&String> = a_list.iter().filter(|c| !b_list.contains(c)).collect();
                if removed.is_empty() && added.is_empty() {
                    continue;
                }
                changed_blocks += 1;
                total_removed += removed.len();
                total_added += added.len();
                let mut block = format!("[~ bloco: {stanza_disp}]");
                for c in &removed {
                    block.push_str(&format!("\n  - {}", fmt_line(c)));
                }
                for c in &added {
                    block.push_str(&format!("\n  + {}", fmt_line(c)));
                }
                diff_sections.push(block);

                if stanza != "[global]" {
                    rollback_lines.push(stanza_disp);
                    for c in &added {
                        rollback_lines
                            .push(format!("  {}", negate_cli_command(vendor, &fmt_line(c))));
                    }
                    for c in &removed {
                        rollback_lines.push(format!("  {}", fmt_line(c)));
                    }
                    rollback_lines.push(exit_cmd.to_string());
                } else {
                    for c in &added {
                        rollback_lines.push(negate_cli_command(vendor, &fmt_line(c)));
                    }
                    for c in &removed {
                        rollback_lines.push(fmt_line(c));
                    }
                }
            }
            (None, None) => {}
        }
    }

    if diff_sections.is_empty() {
        return format!(
            "## Net Diff (vendor: {})\nNenhuma diferença semântica encontrada (ruído volátil ignorado).",
            vendor.as_str()
        );
    }

    format!(
        "## Net Diff (vendor: {}, blocos alterados: {}, -removidos: {}, +adicionados: {})\n\n{}\n\n### Rollback Sugerido\n```text\n{}\n```",
        vendor.as_str(),
        changed_blocks,
        total_removed,
        total_added,
        diff_sections.join("\n\n"),
        rollback_lines.join("\n")
    )
}

fn net_diff(args: &Value, ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let before_arg = str_arg(args, "before")?;
    let after_arg = str_arg(args, "after")?;
    let before = load_config_input(before_arg, ctx.workspace);
    let after = load_config_input(after_arg, ctx.workspace);
    let vendor_override = opt_str(args, "vendor").and_then(Vendor::parse);
    let sanitize_secrets = args["sanitize_secrets"].as_bool().unwrap_or(false);
    Ok(compute_hierarchical_net_diff(
        &before,
        &after,
        vendor_override,
        sanitize_secrets,
    ))
}

fn port_service_label(port: u16) -> &'static str {
    match port {
        22 => "SSH",
        23 => "Telnet",
        53 => "DNS",
        80 => "HTTP",
        161 => "SNMP",
        179 => "BGP",
        443 => "HTTPS",
        830 => "NETCONF",
        3389 => "RDP",
        8080 => "HTTP-Alt",
        8291 => "Winbox",
        8443 => "HTTPS-Alt",
        8728 => "MikroTik-API",
        8729 => "MikroTik-API-SSL",
        _ => "TCP",
    }
}

#[derive(Debug, Clone)]
pub struct ProbeRow {
    pub host: String,
    pub resolved_ip: String,
    pub port: u16,
    pub service: &'static str,
    pub open: bool,
    pub rtt_ms: Option<f64>,
    pub detail: String,
}

pub fn probe_hosts_tcp(
    hosts: &[String],
    ports: &[u16],
    timeout: Duration,
    grab_banner: bool,
) -> Vec<ProbeRow> {
    use std::io::Read;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Instant;

    let mut rows = Vec::with_capacity(hosts.len() * ports.len());
    for host in hosts {
        let host_trimmed = host.trim();
        if host_trimmed.is_empty() {
            continue;
        }
        let resolved = (host_trimmed, 0u16)
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .map(|sa| sa.ip());
        let Some(ip) = resolved else {
            for &port in ports {
                rows.push(ProbeRow {
                    host: host_trimmed.to_string(),
                    resolved_ip: "-".into(),
                    port,
                    service: port_service_label(port),
                    open: false,
                    rtt_ms: None,
                    detail: "falha DNS".into(),
                });
            }
            continue;
        };
        let ip_str = ip.to_string();
        let host_rows: Vec<ProbeRow> = std::thread::scope(|s| {
            let handles: Vec<_> = ports
                .iter()
                .copied()
                .map(|port| {
                    let h_name = host_trimmed.to_string();
                    let ip_display = ip_str.clone();
                    s.spawn(move || {
                        let sock = std::net::SocketAddr::new(ip, port);
                        let started = Instant::now();
                        match TcpStream::connect_timeout(&sock, timeout) {
                            Ok(mut stream) => {
                                let rtt = started.elapsed().as_secs_f64() * 1000.0;
                                let mut banner = String::new();
                                if grab_banner {
                                    let _ =
                                        stream.set_read_timeout(Some(Duration::from_millis(250)));
                                    let mut buf = [0u8; 128];
                                    if let Ok(n) = stream.read(&mut buf) {
                                        if n > 0 {
                                            banner = String::from_utf8_lossy(&buf[..n])
                                                .chars()
                                                .map(|c| {
                                                    if c.is_control() || c == '|' {
                                                        ' '
                                                    } else {
                                                        c
                                                    }
                                                })
                                                .collect::<String>()
                                                .split_whitespace()
                                                .collect::<Vec<_>>()
                                                .join(" ");
                                        }
                                    }
                                }
                                ProbeRow {
                                    host: h_name,
                                    resolved_ip: ip_display,
                                    port,
                                    service: port_service_label(port),
                                    open: true,
                                    rtt_ms: Some(rtt),
                                    detail: if banner.is_empty() {
                                        "conectado".into()
                                    } else {
                                        banner
                                    },
                                }
                            }
                            Err(e) => ProbeRow {
                                host: h_name,
                                resolved_ip: ip_display,
                                port,
                                service: port_service_label(port),
                                open: false,
                                rtt_ms: None,
                                detail: e.kind().to_string(),
                            },
                        }
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        rows.extend(host_rows);
    }
    rows
}

fn net_probe(args: &Value, _ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let mut hosts = Vec::new();
    if let Some(h) = opt_str(args, "host") {
        hosts.push(h.to_string());
    }
    if let Some(arr) = args.get("hosts").and_then(Value::as_array) {
        for v in arr.iter().take(32) {
            if let Some(s) = v.as_str() {
                if !hosts.iter().any(|x| x == s) {
                    hosts.push(s.to_string());
                }
            }
        }
    }
    if hosts.is_empty() {
        return Err(ToolError::BadArguments(
            "informe `host` ou `hosts` para `net_probe`".into(),
        ));
    }

    let default_ports: Vec<u16> = vec![22, 80, 179, 443, 830, 8291, 8443, 8728, 8729];
    let ports: Vec<u16> = args
        .get("ports")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().and_then(|p| u16::try_from(p).ok()))
                .filter(|&p| p > 0)
                .take(32)
                .collect::<Vec<u16>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or(default_ports);

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1500)
        .clamp(100, 10_000);
    let grab_banner = args["grab_banner"].as_bool().unwrap_or(true);

    let rows = probe_hosts_tcp(
        &hosts,
        &ports,
        Duration::from_millis(timeout_ms),
        grab_banner,
    );
    let open_count = rows.iter().filter(|r| r.open).count();
    let mut out = format!(
        "## Net Probe ({} host(s), {} porta(s) abertas de {} testadas)\n\n| Host | IP Resolvido | Porta | Serviço | Status | RTT (ms) | Banner / Detalhe |\n|---|---|---:|---|---|---:|---|",
        hosts.len(),
        open_count,
        rows.len()
    );
    for r in rows {
        let status = if r.open { "OPEN" } else { "CLOSED" };
        let rtt = r
            .rtt_ms
            .map(|ms| format!("{ms:.1}"))
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "\n| {} | {} | {} | {} | {} | {} | {} |",
            r.host, r.resolved_ip, r.port, r.service, status, rtt, r.detail
        ));
    }
    Ok(out)
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

    #[test]
    fn net_diff_groups_by_stanza_generates_rollback_and_respects_sanitize_flag() {
        let before = r#"
! Last configuration change at 12:00:00 UTC
ntp clock-period 17179869
hostname CORE-01
snmp-server community MinhaComunidadePrivada RO
interface GigabitEthernet0/0/1
 description Link-Operadora-A
 ip address 10.10.10.1 255.255.255.252
 no shutdown
router bgp 65001
 neighbor 10.10.10.2 remote-as 65002
 neighbor 10.10.10.2 password 7 SegredoBGP123
"#;
        let after = r#"
! Last configuration change at 14:30:00 UTC
ntp clock-period 17199999
hostname CORE-01
snmp-server community MinhaComunidadeNova RO
interface GigabitEthernet0/0/1
 description Link-Operadora-A-Upgrade
 ip address 10.10.10.1 255.255.255.252
 ip mtu 1500
 no shutdown
router bgp 65001
 neighbor 10.10.10.2 remote-as 65099
 neighbor 10.10.10.2 password 7 SegredoBGPNovo
"#;

        // Default sanitize_secrets=false preserves 100% of secrets for full consulting visibility
        let full = compute_hierarchical_net_diff(before, after, None, false);
        assert!(full.contains("SegredoBGPNovo"), "{full}");
        assert!(full.contains("MinhaComunidadeNova"), "{full}");
        assert!(
            full.contains("[~ bloco: interface GigabitEthernet0/0/1]"),
            "{full}"
        );
        assert!(full.contains("+ ip mtu 1500"), "{full}");
        assert!(full.contains("### Rollback Sugerido"), "{full}");
        assert!(full.contains("no ip mtu 1500"), "{full}");

        // Huawei VRP rollback uses `undo` and `quit`
        let vrp = compute_hierarchical_net_diff(before, after, Some(Vendor::HuaweiVrp), false);
        assert!(vrp.contains("undo ip mtu 1500"), "{vrp}");
        assert!(vrp.contains("\nquit"), "{vrp}");

        // Opt-in sanitize_secrets=true masks secrets for external report sharing
        let sanitized = compute_hierarchical_net_diff(before, after, None, true);
        assert!(!sanitized.contains("SegredoBGPNovo"), "{sanitized}");
        assert!(!sanitized.contains("MinhaComunidadeNova"), "{sanitized}");
        assert!(sanitized.contains("<REDACTED>"), "{sanitized}");
    }

    #[test]
    fn net_probe_detects_open_port_banner_and_closed_port() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open_port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let _ = sock.write_all(b"SSH-2.0-ROSSSH\r\n");
            }
        });

        let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_port = closed_listener.local_addr().unwrap().port();
        drop(closed_listener);

        let rows = probe_hosts_tcp(
            &["127.0.0.1".to_string()],
            &[open_port, closed_port],
            Duration::from_millis(800),
            true,
        );
        assert_eq!(rows.len(), 2);
        let open_row = rows.iter().find(|r| r.port == open_port).unwrap();
        assert!(open_row.open);
        assert!(open_row.rtt_ms.is_some());
        assert!(
            open_row.detail.contains("SSH-2.0-ROSSSH"),
            "{:?}",
            open_row.detail
        );

        let closed_row = rows.iter().find(|r| r.port == closed_port).unwrap();
        assert!(!closed_row.open);
    }

    #[test]
    fn telnet_and_serial_builders_validate_and_format_commands() {
        let mut dev = DeviceEntry {
            name: "olt-01".into(),
            host: "10.0.0.50".into(),
            port: 23,
            user: "admin".into(),
            transport: DeviceTransport::Telnet,
            ..Default::default()
        };
        assert_eq!(
            build_connect_command(&dev).unwrap(),
            "telnet://10.0.0.50:23"
        );

        dev.transport = DeviceTransport::Ssh;
        let ssh_cmd = build_connect_command(&dev).unwrap();
        assert!(
            ssh_cmd.contains("StrictHostKeyChecking=accept-new"),
            "{ssh_cmd}"
        );

        dev.transport = DeviceTransport::Serial;
        dev.serial_port = Some("COM4".into());
        dev.baud_rate = 115200;
        let win_ser = build_connect_command(&dev).unwrap();
        assert!(win_ser.contains("'COM4',115200"), "{win_ser}");

        dev.serial_port = Some("/dev/ttyUSB0".into());
        dev.baud_rate = 9600;
        let nix_ser = build_connect_command(&dev).unwrap();
        assert!(nix_ser.contains("/dev/ttyUSB0 9600"), "{nix_ser}");

        dev.serial_port = Some("COM4;calc".into());
        assert!(build_connect_command(&dev).is_err());
        dev.serial_port = Some("/dev/ttyUSB0;rm".into());
        assert!(build_connect_command(&dev).is_err());
    }

    #[test]
    fn native_telnet_handshake_logs_in_and_disables_paging_fast() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                // Send Telnet IAC DO TERMINAL-TYPE (FF FD 18), IAC DO NAWS (FF FD 1F) and Username prompt
                let _ = sock
                    .write_all(b"\xFF\xFD\x18\xFF\xFD\x1FUser Access Verification\r\nUsername: ");
                let _ = sock.flush();
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                // Read line (may contain IAC reply bytes before "netops")
                let _ = reader.read_line(&mut line);
                let _ = sock.write_all(b"Password: ");
                let _ = sock.flush();
                line.clear();
                let _ = reader.read_line(&mut line);
                let _ = sock.write_all(b"\r\nCORE-RTR-01#");
                let _ = sock.flush();
                line.clear();
                let _ = reader.read_line(&mut line);
                if line.contains("terminal length 0") {
                    let _ = sock.write_all(b"CORE-RTR-01#");
                    let _ = sock.flush();
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });

        let ws = std::env::current_dir().unwrap();
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Bypass,
            &ws,
        );
        let ctx = CallCtx {
            workspace: &ws,
            policy: &policy,
            memory: None,
            scope: None,
        };
        let cancel = zerith_providers::Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: std::time::Instant::now() + Duration::from_secs(15),
            timeout: Duration::from_secs(15),
            host: &host,
        };

        let started = std::time::Instant::now();
        let res = net_connect(
            &json!({
                "transport": "telnet",
                "host": "127.0.0.1",
                "port": port,
                "user": "netops",
                "password": "SuperSecretRouterPw",
                "vendor": "cisco_ios",
                "disable_paging": true
            }),
            &ctx,
            &exec,
        )
        .unwrap();
        let elapsed = started.elapsed();

        assert!(res.contains("conectado via telnet"), "{res}");
        assert!(res.contains("CORE-RTR-01#"), "{res}");
        assert!(
            res.contains("[paginação desativada: terminal length 0]"),
            "{res}"
        );
        assert!(
            !res.contains("SuperSecretRouterPw"),
            "password must be scrubbed from terminal echo: {res}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "telnet handshake took too long: {elapsed:?}"
        );
        let term_id = res.split_whitespace().next().unwrap();
        let _ = crate::terminal::close_terminal(term_id);
    }
}
