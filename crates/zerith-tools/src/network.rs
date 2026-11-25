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
    pub client: Option<String>,
    pub site: Option<String>,
    pub vpn: Option<String>,
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
            client: None,
            site: None,
            vpn: None,
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

impl DeviceEntry {
    pub fn composite_key(&self) -> String {
        let c = self
            .client
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let n = self.name.trim().to_ascii_lowercase();
        format!("{c}::{n}")
    }

    pub fn display_qualified_name(&self) -> String {
        match self
            .client
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(c) => format!("{c}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
/// Indexed by `(client, name)` so different clients can share device names (`core-01`, `pve-01`)
/// and overlapping private IPs (`10.0.0.1`) without collision.
pub fn load_inventory(
    workspace: &Path,
    global_root: Option<&Path>,
) -> std::result::Result<Vec<DeviceEntry>, String> {
    let mut by_key: HashMap<String, DeviceEntry> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut merge = |path: &Path| -> std::result::Result<(), String> {
        if !path.is_file() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("erro lendo {}: {e}", path.display()))?;
        for dev in parse_inventory(&raw)? {
            let key = dev.composite_key();
            if !by_key.contains_key(&key) {
                order.push(key.clone());
            }
            by_key.insert(key, dev);
        }
        Ok(())
    };
    if let Some(root) = global_root {
        merge(&root.join("inventory.toml"))?;
    }
    merge(&workspace.join(".zerith").join("inventory.toml"))?;
    Ok(order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect())
}

/// The `secret_ref` of the inventory device called `name` (global inventory
/// and the current folder's), for a login that must not show the password.
pub fn device_secret_ref(name: &str) -> Option<String> {
    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let cwd = std::env::current_dir().ok()?;
    load_inventory(&cwd, Some(&root))
        .ok()?
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(name))
        .and_then(|d| d.secret_ref)
}

pub fn derive_secret_ref_name(client: Option<&str>, name: &str) -> String {
    let sanitize = |s: &str| -> String {
        s.trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .split('_')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("_")
    };
    let n = sanitize(name);
    match client.map(sanitize).filter(|c| !c.is_empty()) {
        Some(c) => format!("net_{c}_{n}"),
        None => format!("net_{n}"),
    }
}

/// Inserts or updates a [`DeviceEntry`] inside `inventory_path` (`inventory.toml`),
/// matching by `(client, name)`.
pub fn upsert_inventory_device(
    inventory_path: &Path,
    dev: DeviceEntry,
) -> std::result::Result<(), String> {
    if dev.name.trim().is_empty() {
        return Err("name do dispositivo não pode ficar vazio".into());
    }
    let mut list = if inventory_path.is_file() {
        let raw = std::fs::read_to_string(inventory_path)
            .map_err(|e| format!("erro lendo {}: {e}", inventory_path.display()))?;
        parse_inventory(&raw)?
    } else {
        Vec::new()
    };
    let target_key = dev.composite_key();
    if let Some(existing) = list.iter_mut().find(|d| d.composite_key() == target_key) {
        *existing = dev;
    } else {
        list.push(dev);
    }
    if let Some(parent) = inventory_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("erro criando pasta {}: {e}", parent.display()))?;
    }
    let serialized = toml::to_string_pretty(&InventoryFile { devices: list })
        .map_err(|e| format!("erro serializando inventory.toml: {e}"))?;
    std::fs::write(inventory_path, serialized)
        .map_err(|e| format!("erro gravando {}: {e}", inventory_path.display()))
}

/// Removes a device matching `(client, name)` from `inventory_path`.
pub fn remove_inventory_device(
    inventory_path: &Path,
    client: Option<&str>,
    name: &str,
) -> std::result::Result<bool, String> {
    if !inventory_path.is_file() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(inventory_path)
        .map_err(|e| format!("erro lendo {}: {e}", inventory_path.display()))?;
    let mut list = parse_inventory(&raw)?;
    let before = list.len();
    let c_norm = client.unwrap_or("").trim().to_ascii_lowercase();
    let n_norm = name.trim().to_ascii_lowercase();
    list.retain(|d| {
        let dc = d
            .client
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let dn = d.name.trim().to_ascii_lowercase();
        if client.is_some() {
            !(dc == c_norm && dn == n_norm)
        } else {
            dn != n_norm
        }
    });
    if list.len() == before {
        return Ok(false);
    }
    let serialized = toml::to_string_pretty(&InventoryFile { devices: list })
        .map_err(|e| format!("erro serializando inventory.toml: {e}"))?;
    std::fs::write(inventory_path, serialized)
        .map_err(|e| format!("erro gravando {}: {e}", inventory_path.display()))?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardProfile {
    pub name: String,
    pub path: std::path::PathBuf,
    pub address: Option<String>,
    pub allowed_ips: Vec<String>,
    pub endpoint: Option<String>,
}

pub fn parse_wireguard_conf_summary(
    conf_text: &str,
) -> (Option<String>, Vec<String>, Option<String>) {
    let mut address = None;
    let mut allowed_ips = Vec::new();
    let mut endpoint = None;
    for line in conf_text.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        match key.as_str() {
            "address" => address = Some(val.to_string()),
            "allowedips" => {
                for cidr in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if !allowed_ips.iter().any(|x| x == cidr) {
                        allowed_ips.push(cidr.to_string());
                    }
                }
            }
            "endpoint" => endpoint = Some(val.to_string()),
            _ => {}
        }
    }
    (address, allowed_ips, endpoint)
}

fn validate_vpn_name(raw: &str) -> std::result::Result<String, String> {
    let t = raw.trim().trim_end_matches(".conf");
    if t.is_empty() {
        return Err("nome do túnel VPN não pode ficar vazio".into());
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(format!(
            "nome de VPN inválido `{t}` (use apenas letras, números, `-` e `_`)"
        ));
    }
    Ok(t.to_string())
}

pub fn save_wireguard_profile(
    root: &Path,
    vpn_name: &str,
    conf_text_or_path: &str,
    workspace: &Path,
) -> std::result::Result<WireGuardProfile, String> {
    let clean_name = validate_vpn_name(vpn_name)?;
    let content = load_config_input(conf_text_or_path, workspace);
    if !content.to_ascii_lowercase().contains("[interface]") {
        return Err("configuração WireGuard inválida: precisa conter a seção `[Interface]`".into());
    }
    let dir = root.join("vpn");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("erro criando pasta de VPNs {}: {e}", dir.display()))?;
    let path = dir.join(format!("{clean_name}.conf"));
    std::fs::write(&path, &content)
        .map_err(|e| format!("erro salvando VPN {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let (address, allowed_ips, endpoint) = parse_wireguard_conf_summary(&content);
    Ok(WireGuardProfile {
        name: clean_name,
        path,
        address,
        allowed_ips,
        endpoint,
    })
}

pub fn list_wireguard_profiles(
    workspace: &Path,
    global_root: Option<&Path>,
) -> Vec<WireGuardProfile> {
    let mut by_name: HashMap<String, WireGuardProfile> = HashMap::new();
    let mut order = Vec::new();
    let mut scan_dir = |dir: &Path| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("conf") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let (address, allowed_ips, endpoint) = parse_wireguard_conf_summary(&text);
            let key = stem.to_ascii_lowercase();
            if !by_name.contains_key(&key) {
                order.push(key.clone());
            }
            by_name.insert(
                key,
                WireGuardProfile {
                    name: stem.to_string(),
                    path: p,
                    address,
                    allowed_ips,
                    endpoint,
                },
            );
        }
    };
    if let Some(root) = global_root {
        scan_dir(&root.join("vpn"));
    }
    scan_dir(&workspace.join(".zerith").join("vpn"));
    order
        .into_iter()
        .filter_map(|k| by_name.remove(&k))
        .collect()
}

fn validate_ssh_token(field: &str, value: &str) -> std::result::Result<(), String> {
    let t = value.trim();
    if t.is_empty() {
        return Err(format!("{field} não pode ficar vazio"));
    }
    if t.starts_with('-') {
        return Err(format!("{field} inválido (não pode começar com `-`): {t}"));
    }
    // `+` only in the user: RouterOS console options (`admin+ct80w`).
    if !t.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '-' | ':' | '@' | '[' | ']' | '%' | ',')
            || (c == '+' && field == "user")
    }) {
        return Err(format!("{field} contém caractere inválido: {t}"));
    }
    Ok(())
}

/// RouterOS console login options, after `+` in the login name (manual,
/// "Command Line Interface" > "Console login options": `login_name ::=
/// user_name [ '+' parameters ]`, number before letter): c = no colors,
/// e = dumb terminal (no line redraw: the echo comes back clean instead of
/// the prompt drawn in the middle of it), t = no terminal auto-detection,
/// 511w = width (long commands neither wrap nor scroll), 4098h = height.
/// netmiko 4.8.0 (MIT), `netmiko/mikrotik/mikrotik_ssh.py`
/// `_modify_connection_params`, sends `+ct511w4098h`; the `e` is the one in
/// the community.routeros SSH guide (`+cet512w`).
const ROUTEROS_LOGIN_OPTS: &str = "+cet511w4098h";

/// The name typed at login: RouterOS over ssh/telnet gets
/// [`ROUTEROS_LOGIN_OPTS`] unless the user already carries `+` options.
/// Only on the wire: the inventory and the vault keep the plain user.
fn login_user(dev: &DeviceEntry, user: &str) -> String {
    if dev.vendor == Vendor::MikrotikRouterOs
        && dev.transport != DeviceTransport::Serial
        && !user.contains('+')
    {
        format!("{user}{ROUTEROS_LOGIN_OPTS}")
    } else {
        user.to_string()
    }
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
    parts.push(format!("{}@{}", login_user(dev, &user), dev.host.trim()));
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

/// A command about to be typed on a device, made to end on its own.
/// `Ok((cmd, note))`: what to send and, when the Zerith changed or flagged
/// it, a line for the result (`[ajustado: count=4 acrescentado]`).
/// `Err(msg)`: never ends and has no stop parameter; not sent.
///
/// RouterOS (docs.mikrotik.com): `/ping` without `count=`, `/tool torch` and
/// `/tool sniffer quick` without `duration=`, `/tool traceroute` without
/// `count=`, `monitor`/`monitor-traffic` without `once` run until `q`;
/// `print follow`/`follow-only` and `print interval=` never stop.
/// Cisco/Huawei/Juniper pagers are handled by `Vendor::disable_paging_cmd`;
/// their `ping` stops by itself (5 packets).
pub fn prepare_network_command(
    vendor: Vendor,
    cmd: &str,
) -> std::result::Result<(String, Option<String>), String> {
    let low = cmd.trim().to_ascii_lowercase();
    // `/ip/route/check` and `/ip route check` are the same path in v7.
    let words: Vec<&str> = low
        .split_whitespace()
        .flat_map(|w| {
            let parts: Vec<&str> = if w.contains('=') {
                vec![w]
            } else {
                w.split('/').filter(|p| !p.is_empty()).collect()
            };
            parts
        })
        .collect();
    let has = |w: &str| words.contains(&w);
    let param = |p: &str| words.iter().any(|w| w.starts_with(p));
    let first = words.first().copied().unwrap_or("");
    let tool = |name: &str| first == name || (first == "tool" && words.get(1) == Some(&name));
    let add = |extra: &str| {
        Ok((
            format!("{} {extra}", cmd.trim_end()),
            Some(format!("[ajustado: {extra} acrescentado]")),
        ))
    };
    match vendor {
        Vendor::MikrotikRouterOs => {
            if has("print") && (has("follow") || has("follow-only") || param("follow")) {
                return Err("`print follow` não termina; use `print` com `where` (ex.: \
                            /log print where topics~\"error\") ou `print count-only`"
                    .into());
            }
            if has("print") && param("interval=") {
                return Err(
                    "`print interval=` repete para sempre; tire o `interval=` para uma leitura"
                        .into(),
                );
            }
            if tool("ping") && !param("count=") {
                return add("count=4");
            }
            if (tool("torch") || (has("sniffer") && has("quick"))) && !param("duration=") {
                return add("duration=5");
            }
            if tool("traceroute") && !param("count=") {
                return add("count=1");
            }
            if (has("monitor") || has("monitor-traffic"))
                && !has("once")
                && !param("once=")
                && !param("duration=")
            {
                return add("once");
            }
            if has("route") && has("check") {
                return Ok((
                    cmd.to_string(),
                    Some(
                        "[contínuo no RouterOS 7: o Zerith interrompe após o primeiro resultado]"
                            .into(),
                    ),
                ));
            }
            // RouterOS has no "paging off" for the session: a long `print`
            // stops every screen on `-- [Q quit|D dump|down]`, one round
            // trip each (a 4k-line route table is ~70). `without-paging`
            // exists since v6. Plain CLI paths only: inside `[...]` or
            // with `as-value` the print is a script value, not a screen.
            // Right after `print`: everything after `where` is the filter.
            let print_at = cmd
                .to_ascii_lowercase()
                .find(" print")
                .map(|i| i + " print".len())
                .filter(|&i| cmd[i..].is_empty() || cmd[i..].starts_with(' '));
            if let Some(i) = print_at.filter(|_| {
                low.starts_with('/')
                    && !low.contains('[')
                    && !has("without-paging")
                    && !has("as-value")
                    && !has("count-only")
            }) {
                return Ok((
                    format!("{} without-paging{}", &cmd[..i], cmd[i..].trim_end()),
                    None,
                ));
            }
        }
        Vendor::Linux => {
            if low.starts_with("ping ") && !words.iter().any(|w| w.starts_with("-c")) {
                return Ok((
                    format!("ping -c 4 {}", cmd.trim()[4..].trim_start()),
                    Some("[ajustado: -c 4 acrescentado]".into()),
                ));
            }
            if (first == "tail" || first == "journalctl")
                && words.iter().any(|w| *w == "-f" || *w == "--follow")
                || first == "watch"
            {
                return Err(format!(
                    "`{first}` contínuo não termina; leia uma vez (tail -n 200, journalctl -n 200)"
                ));
            }
        }
        // Gancho: VRP8/CE aceita `display ... | no-more`, mas o VRP5 não;
        // `screen-length 0 temporary` (disable_paging_cmd) cobre os dois.
        Vendor::HuaweiVrp => {}
        _ => {}
    }
    Ok((cmd.to_string(), None))
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
            "Inventário de equipamentos, servidores e VMs por cliente (`.zerith/inventory.toml` e o global). \
             `action`: `list` (padrão; filtros `client`, `site`, `vpn`, `tag`, `vendor`, `role`, `query`), \
             `upsert` (cadastra ou atualiza; `password` vai para o cofre de segredos), `remove`, \
             `save_vpn` (grava o `.conf` WireGuard do cliente em `~/.zerith/vpn/<vpn>.conf`). \
             Use antes de conectar, para achar host, usuário e VPN.",
            json!({"type": "object", "properties": {
                "action": {"type": "string", "description": "list (padrão) | upsert | remove | save_vpn"},
                "name": {"type": "string", "description": "nome do equipamento/VM (ex.: core-01, pve-01, olt-centro)"},
                "client": {"type": "string", "description": "nome do cliente/empresa (ex.: TelecomSul, HospitalVida)"},
                "site": {"type": "string", "description": "POP / Datacenter / Filial (ex.: DC-Curitiba, POP-Norte)"},
                "vpn": {"type": "string", "description": "nome do túnel WireGuard associado ao cliente (ex.: wg-telecomsul)"},
                "vpn_config": {"type": "string", "description": "texto do .conf WireGuard ou caminho do arquivo"},
                "host": {"type": "string", "description": "IP/hostname ou porta serial"},
                "port": {"type": "integer", "description": "porta SSH/Telnet (padrão 22 ou 23)"},
                "user": {"type": "string"},
                "password": {"type": "string", "description": "upsert: vai para o cofre de segredos e vira secret_ref"},
                "secret_ref": {"type": "string"},
                "transport": {"type": "string", "description": "ssh | telnet | serial"},
                "serial_port": {"type": "string"},
                "baud_rate": {"type": "integer"},
                "vendor": {"type": "string"},
                "role": {"type": "string"},
                "jump_host": {"type": "string"},
                "legacy_ssh": {"type": "boolean"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "description": {"type": "string"},
                "scope": {"type": "string", "description": "global (padrão) | workspace"},
                "tag": {"type": "string", "description": "filtro por tag em action=list"},
                "query": {"type": "string", "description": "filtro livre em action=list"}
            }}),
            "list: equipamentos encontrados; demais ações: confirmação",
            Permission::Read,
            10,
            true,
            |args, ctx| {
                let act = opt_str(args, "action").unwrap_or("list");
                if matches!(act, "upsert" | "remove" | "save_vpn") {
                    Ok(Some(Action::Write(ctx.workspace.join(".zerith/inventory.toml"))))
                } else {
                    Ok(Some(Action::Read(ctx.workspace.join(".zerith"))))
                }
            },
            net_inventory,
        ),
        make_with(
            "net_vpn",
            "Túneis WireGuard por cliente: `status` (padrão), `list`, `switch`, `up`, `down`. \
             `switch`/`up` derruba o túnel do cliente anterior (faixas privadas podem colidir), fecha os terminais dele \
             e sobe o do cliente pedido.",
            json!({"type": "object", "properties": {
                "action": {"type": "string", "description": "status (padrão) | list | switch | up | down"},
                "vpn": {"type": "string", "description": "nome do perfil WireGuard (ex.: wg-telecomsul)"},
                "client": {"type": "string", "description": "cliente (o túnel vem do inventário)"},
                "vpn_config": {"type": "string", "description": "opcional: texto ou caminho do .conf para salvar antes de subir"}
            }}),
            "túneis ativos com AllowedIPs, ou confirmação da troca",
            Permission::Shell,
            45,
            false,
            |args, _| {
                let act = opt_str(args, "action").unwrap_or("status");
                let v = opt_str(args, "vpn").or_else(|| opt_str(args, "client")).unwrap_or("");
                Ok(Some(Action::Shell(format!("net_vpn {act} {v}").trim().to_string())))
            },
            net_vpn,
        ),
        make_with(
            "net_connect",
            "Abre uma sessão interativa por SSH, Telnet ou serial (COMx, /dev/ttyUSBx) com um equipamento, servidor ou VM: \
             pelo inventário (`device`, com `client` se o nome se repete) ou por `host`/`serial_port`, `transport`, `user`, `vendor`. \
             Sobe antes a VPN WireGuard do cliente, se houver. Com `remember` (padrão quando há `client`), grava o equipamento \
             no inventário e a senha no cofre. Depois use `terminal_batch` com o id devolvido.",
            json!({"type": "object", "properties": {
                "device": {"type": "string", "description": "nome no inventory.toml (ex.: core-01 ou TelecomSul/core-01)"},
                "client": {"type": "string", "description": "cliente/empresa dono do equipamento (ex.: TelecomSul)"},
                "site": {"type": "string", "description": "POP / Datacenter / Filial opcional"},
                "vpn": {"type": "string", "description": "túnel WireGuard do cliente (sobe antes de conectar)"},
                "auto_vpn": {"type": "boolean", "description": "sobe ou troca a VPN WireGuard do cliente se preciso, padrão true"},
                "remember": {"type": "boolean", "description": "grava o equipamento no inventário e a senha no cofre ao conectar (padrão true quando há client)"},
                "transport": {"type": "string", "description": "ssh (padrão) | telnet | serial"},
                "host": {"type": "string", "description": "IP/hostname (ou porta serial como COM3 / /dev/ttyUSB0)"},
                "serial_port": {"type": "string", "description": "porta serial explícita (ex.: COM3, /dev/ttyUSB0, /dev/ttyS0)"},
                "baud_rate": {"type": "integer", "description": "velocidade da serial (ex.: 9600, 115200), padrão 9600"},
                "user": {"type": "string"},
                "port": {"type": "integer", "description": "padrão 22 para SSH, 23 para Telnet"},
                "vendor": {"type": "string", "description": "cisco_ios|cisco_nxos|cisco_xr|huawei_vrp|juniper_junos|mikrotik_router_os|fortinet_forti_os|linux|generic"},
                "role": {"type": "string", "description": "core|border|distribution|access|firewall|lab|server"},
                "secret_ref": {"type": "string", "description": "chave no cofre (zerith secret set <nome>)"},
                "password": {"type": "string", "description": "senha direta (vai para o cofre se remember=true)"},
                "jump_host": {"type": "string"},
                "legacy_ssh": {"type": "boolean"},
                "disable_paging": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "padrão 25"}
            }}),
            "id do terminal (`term-N`), cliente, VPN, transporte, banner e se a paginação foi desligada",
            Permission::Shell,
            130,
            false,
            connect_action,
            net_connect,
        ),
        make_with(
            "terminal_batch",
            "Roda uma lista de comandos num terminal aberto (`terminal_open` ou `net_connect`) e devolve a saída \
             separada por comando, passando a paginação e marcando erros de CLI. Comando contínuo (monitor, \
             `print follow`, ping sem fim) ou que estoura o prazo é interrompido (q/Ctrl+C) e o lote segue.",
            json!({"type": "object", "properties": {
                "id": {"type": "string"},
                // Só texto no schema: `anyOf` é recusado por parte dos provedores
                // (Gemini, alguns compatíveis com OpenAI). `batch_items` ainda aceita
                // `{"cmd", "timeout_secs"}` para quem mandar.
                "commands": {"type": "array", "description": "um comando por item; para um prazo maior use timeout_secs", "items": {"type": "string"}},
                "stop_on_error": {"type": "boolean", "description": "padrão true"},
                "auto_page": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "prazo por comando, padrão 20, máximo 300"}
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
            "Compara duas configurações de equipamento (`before` e `after`: caminho ou texto) por bloco \
             (`interface`, `router bgp`, `route-policy`...), ignora ruído (timestamps, `ntp clock-period`) \
             e monta o rollback (`no ...` Cisco, `undo ...` Huawei). Segredos aparecem como estão; \
             `sanitize_secrets: true` os mascara para relatório externo.",
            json!({"type": "object", "properties": {
                "before": {"type": "string", "description": "caminho do arquivo ou texto da configuração anterior"},
                "after": {"type": "string", "description": "caminho do arquivo ou texto da configuração nova"},
                "vendor": {"type": "string", "description": "cisco_ios|cisco_nxos|cisco_xr|huawei_vrp|juniper_junos|mikrotik_router_os|fortinet_forti_os|generic (auto-detectado se omitido)"},
                "sanitize_secrets": {"type": "boolean", "description": "padrão false; true mascara senhas e chaves"}
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
            "Testa portas TCP de gerência em paralelo (SSH 22, Telnet 23, BGP 179, NETCONF 830, Proxmox 8006, \
             Winbox 8291, API MikroTik 8728/8729, HTTP/HTTPS 80/443/8443) em `host`, `hosts` ou `subnet` \
             (`10.0.0.1-20`, `10.0.0.0/28`), com DNS, RTT e banner. Use para descobrir o que responde numa rede. \
             Com `auto_register_client`, deduz o tipo pelo banner/porta e cadastra os hosts no inventário do cliente.",
            json!({"type": "object", "properties": {
                "host": {"type": "string", "description": "hostname ou IP único"},
                "hosts": {"type": "array", "items": {"type": "string"}, "description": "lista de hostnames ou IPs"},
                "subnet": {"type": "string", "description": "sub-rede CIDR (ex.: 10.0.0.0/28) ou faixa de IPs (ex.: 10.0.0.1-30), até 64 IPs"},
                "ports": {"type": "array", "items": {"type": "integer"}, "description": "padrão [22, 23, 80, 179, 443, 830, 8006, 8291, 8443, 8728, 8729]"},
                "timeout_ms": {"type": "integer", "description": "timeout TCP por porta em ms, padrão 1200"},
                "grab_banner": {"type": "boolean", "description": "lê banner inicial (ex.: SSH-2.0-...), padrão true"},
                "auto_register_client": {"type": "string", "description": "cadastra os hosts que responderem neste cliente do inventário"},
                "site": {"type": "string", "description": "site para o cadastro"},
                "vpn": {"type": "string", "description": "túnel WireGuard opcional para vincular aos hosts descobertos"},
                "user": {"type": "string", "description": "usuário padrão opcional para os hosts descobertos (ex.: admin ou root)"}
            }}),
            "tabela com DNS, estado de cada porta, RTT em ms, banner e o que foi cadastrado",
            Permission::Network,
            60,
            true,
            |args, _| {
                let h = opt_str(args, "host")
                    .or_else(|| opt_str(args, "subnet"))
                    .unwrap_or("net_probe");
                Ok(Some(Action::Network(h.to_string())))
            },
            net_probe,
        ),
        make_with(
            "net_broadcast",
            "Roda um comando em paralelo em vários terminais abertos (`ids`; ou os de um `client` ou `vendor`; \
             sem filtro, todos) e devolve a saída de cada um lado a lado, com o tempo de resposta.",
            json!({"type": "object", "properties": {
                "command": {"type": "string", "description": "comando a executar em paralelo em todos os terminais selecionados"},
                "ids": {"type": "array", "items": {"type": "string"}, "description": "lista opcional de IDs (ex.: [\"term-1\", \"term-2\"]); se omitido, usa todos os terminais ativos ou filtrados por client/vendor"},
                "client": {"type": "string", "description": "filtra apenas terminais abertos deste cliente"},
                "vendor": {"type": "string", "description": "filtra apenas terminais abertos deste vendor"},
                "auto_page": {"type": "boolean", "description": "padrão true"},
                "timeout_secs": {"type": "integer", "description": "timeout por host em segundos, padrão 20"}
            }, "required": ["command"]}),
            "saída de cada terminal com o tempo em ms",
            Permission::Shell,
            300,
            false,
            |args, _| {
                let cmd = opt_str(args, "command").unwrap_or("");
                Ok(Some(Action::Shell(format!("[broadcast] {cmd}"))))
            },
            net_broadcast,
        ),
        make_with(
            "net_backup",
            "Baixa a configuração (`running-config`, `current-configuration`, `/export`...) de terminais abertos \
             (`id`, `ids` ou `client`), grava em `backups/<cliente>/<equipamento>/latest.cfg` com histórico por data \
             e compara com o backup anterior (como `net_diff`).",
            json!({"type": "object", "properties": {
                "id": {"type": "string", "description": "ID de um terminal aberto (ex.: term-1)"},
                "ids": {"type": "array", "items": {"type": "string"}, "description": "lista de terminais abertos para fazer backup em lote"},
                "client": {"type": "string", "description": "faz backup de todos os terminais abertos deste cliente"},
                "command": {"type": "string", "description": "comando customizado opcional (auto-detectado pelo vendor se omitido)"},
                "timeout_secs": {"type": "integer", "description": "padrão 45"}
            }}),
            "caminhos dos backups gravados e o diff contra o anterior",
            Permission::Shell,
            300,
            false,
            |args, _| {
                let target = opt_str(args, "id")
                    .or_else(|| opt_str(args, "client"))
                    .unwrap_or("all");
                Ok(Some(Action::Shell(format!("net_backup {target}"))))
            },
            net_backup,
        ),
        make_with(
            "serial_ports",
            "Lista as portas seriais e cabos console USB-serial do computador (`COMn` no Windows com o adaptador \
             FTDI/CH340/PL2303; `/dev/ttyUSB*`, `/dev/ttyACM*`, `/dev/ttyS*`, `/dev/cu.*` no Linux/macOS). \
             Use antes de `net_connect` com `transport: serial`.",
            json!({"type": "object", "properties": {}}),
            "uma porta serial por linha",
            Permission::Read,
            10,
            true,
            |_, _| Ok(None),
            serial_ports,
        ),
        make_with(
            "net_transfer",
            "Copia arquivos por SCP/SFTP entre o computador (`local_path`) e um equipamento, servidor ou VM \
             (`remote_path`), em `direction` `upload` ou `download`: backups, `.pcap`, logs, firmware. \
             Acha o destino pelo inventário (`device`, `client`) ou por `host`/`user`/`port` e sobe a VPN do cliente antes.",
            json!({"type": "object", "properties": {
                "direction": {"type": "string", "description": "upload | download"},
                "device": {"type": "string", "description": "nome no inventory.toml (ex.: pve-01 ou TelecomSul/pve-01)"},
                "client": {"type": "string", "description": "nome do cliente"},
                "host": {"type": "string"},
                "port": {"type": "integer", "description": "padrão 22"},
                "user": {"type": "string"},
                "local_path": {"type": "string", "description": "caminho do arquivo/pasta no computador local"},
                "remote_path": {"type": "string", "description": "caminho do arquivo/pasta no servidor/roteador remoto"},
                "auto_vpn": {"type": "boolean", "description": "padrão true"}
            }, "required": ["direction", "local_path", "remote_path"]}),
            "tamanho em bytes e tempo da cópia",
            Permission::Shell,
            300,
            false,
            |args, _| {
                let dir = opt_str(args, "direction").unwrap_or("transfer");
                let loc = opt_str(args, "local_path").unwrap_or("");
                let rem = opt_str(args, "remote_path").unwrap_or("");
                Ok(Some(Action::Shell(format!("net_transfer {dir} {loc} <-> {rem}"))))
            },
            net_transfer,
        ),
        make_with(
            "net_path",
            "Mede o caminho até `host` (ou `device` + `client`): perda, latência e jitter (min/avg/max ms), \
             maior MTU sem fragmentar (referências: 1500 Ethernet, 1492 PPPoE, 1420 WireGuard, 1400 IPsec) \
             e, com `traceroute`, os saltos. Use para enlace lento, perda ou suspeita de MTU.",
            json!({"type": "object", "properties": {
                "host": {"type": "string", "description": "IP ou hostname de destino"},
                "device": {"type": "string", "description": "opcional: nome do dispositivo no inventory.toml"},
                "client": {"type": "string", "description": "opcional: cliente para resolver o dispositivo e chavear a VPN WireGuard"},
                "count": {"type": "integer", "description": "quantidade de sondas de latência/jitter, padrão 4 (máx 10)"},
                "discover_mtu": {"type": "boolean", "description": "testa Path MTU com bit Don't-Fragment, padrão true"},
                "traceroute": {"type": "boolean", "description": "executa traceroute/tracert, padrão false"},
                "port": {"type": "integer", "description": "porta TCP fallback para medir RTT/jitter se ICMP for bloqueado, padrão 22"}
            }}),
            "perda, latência, jitter, MTU máximo e saltos",
            Permission::Network,
            60,
            true,
            |args, _| {
                let h = opt_str(args, "host")
                    .or_else(|| opt_str(args, "device"))
                    .unwrap_or("net_path");
                Ok(Some(Action::Network(h.to_string())))
            },
            net_path,
        ),
    ]
}

fn active_vpn_slot() -> &'static std::sync::Mutex<Option<String>> {
    static V: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    V.get_or_init(Default::default)
}

/// Queries the OS (`wg show interfaces`) and Zerith's tracked WireGuard state
/// to list currently active WireGuard tunnel names.
pub fn detect_active_wireguard_tunnels() -> Vec<String> {
    let mut active = Vec::new();
    if let Ok(out) = std::process::Command::new("wg")
        .args(["show", "interfaces"])
        .output()
    {
        if out.status.success() {
            for tok in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if !tok.is_empty() && !active.iter().any(|x: &String| x.eq_ignore_ascii_case(tok)) {
                    active.push(tok.to_string());
                }
            }
        }
    }
    if let Ok(g) = active_vpn_slot().lock() {
        if let Some(tracked) = g.as_ref() {
            if !active.iter().any(|x| x.eq_ignore_ascii_case(tracked)) {
                active.push(tracked.clone());
            }
        }
    }
    active
}

fn run_wg_tunnel_down(
    vpn_name: &str,
    conf_path: Option<&Path>,
) -> std::result::Result<String, String> {
    let clean = validate_vpn_name(vpn_name)?;
    let target_arg = conf_path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| clean.clone());
    let res = if cfg!(windows) {
        std::process::Command::new("wireguard.exe")
            .args(["/uninstalltunnelservice", &clean])
            .output()
    } else {
        std::process::Command::new("wg-quick")
            .args(["down", &target_arg])
            .output()
    };
    if let Ok(mut g) = active_vpn_slot().lock() {
        if g.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(&clean)) {
            *g = None;
        }
    }
    match res {
        Ok(out) if out.status.success() => Ok(format!("túnel `{clean}` desligado")),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Ok(format!(
                "túnel `{clean}` marcado como desligado ({})",
                if stderr.is_empty() {
                    "serviço parado".into()
                } else {
                    stderr
                }
            ))
        }
        Err(e) => Ok(format!(
            "túnel `{clean}` desativado no estado local (CLI wg ausente: {e})"
        )),
    }
}

fn run_wg_tunnel_up(profile: &WireGuardProfile) -> std::result::Result<String, String> {
    let path_str = profile.path.to_string_lossy().into_owned();
    let res = if cfg!(windows) {
        std::process::Command::new("wireguard.exe")
            .args(["/installtunnelservice", &path_str])
            .output()
    } else {
        std::process::Command::new("wg-quick")
            .args(["up", &path_str])
            .output()
    };
    if let Ok(mut g) = active_vpn_slot().lock() {
        *g = Some(profile.name.clone());
    }
    match res {
        Ok(out) if out.status.success() => Ok(format!(
            "túnel WireGuard `{}` ativado ({})",
            profile.name,
            if profile.allowed_ips.is_empty() {
                "sem AllowedIPs".into()
            } else {
                format!("AllowedIPs: {}", profile.allowed_ips.join(", "))
            }
        )),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Ok(format!(
                "túnel `{}` solicitado ({}) — detalhe CLI: {}",
                profile.name,
                profile.path.display(),
                if stderr.is_empty() { "ok".into() } else { stderr }
            ))
        }
        Err(e) => Ok(format!(
            "perfil `{}` registrado como ativo ({}) — binário wireguard/wg-quick não encontrado no PATH ({e})",
            profile.name,
            profile.path.display()
        )),
    }
}

/// Switches exclusively to `vpn_name`: turns off any other active WireGuard tunnel,
/// closes open terminals belonging to other VPNs, and brings up `vpn_name`.
pub fn ensure_exclusive_wireguard_vpn(
    vpn_name: &str,
    workspace: &Path,
    global_root: &Path,
) -> std::result::Result<String, String> {
    let clean = validate_vpn_name(vpn_name)?;
    let profiles = list_wireguard_profiles(workspace, Some(global_root));
    let active = detect_active_wireguard_tunnels();
    let mut notes = Vec::new();

    // 1. Close terminals attached to other VPNs and bring down any other active WireGuard tunnel
    let closed_terms = terminal::close_other_vpn_terminals(&clean);
    if !closed_terms.is_empty() {
        notes.push(format!(
            "terminais do cliente anterior encerrados: {}",
            closed_terms.join(", ")
        ));
    }
    for act in &active {
        if !act.eq_ignore_ascii_case(&clean) {
            let old_path = profiles
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(act))
                .map(|p| p.path.as_path());
            if let Ok(msg) = run_wg_tunnel_down(act, old_path) {
                notes.push(msg);
            }
        }
    }

    // 2. If target tunnel is already active, keep it
    if active.iter().any(|a| a.eq_ignore_ascii_case(&clean)) {
        if let Ok(mut g) = active_vpn_slot().lock() {
            *g = Some(clean.clone());
        }
        notes.push(format!("VPN `{clean}` já está ativa"));
        return Ok(notes.join(" · "));
    }

    // 3. Bring up target tunnel if profile exists
    if let Some(prof) = profiles
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(&clean))
    {
        let up_msg = run_wg_tunnel_up(prof)?;
        notes.push(up_msg);
    } else {
        if let Ok(mut g) = active_vpn_slot().lock() {
            *g = Some(clean.clone());
        }
        notes.push(format!(
            "VPN `{clean}` selecionada (nenhum arquivo `{clean}.conf` em {}/vpn; envie o .conf com `net_inventory(action=\"save_vpn\")` para gerenciar o serviço do SO)",
            global_root.display()
        ));
    }
    Ok(notes.join(" · "))
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

pub fn find_inventory_device(
    inv: &[DeviceEntry],
    device_query: &str,
    client_filter: Option<&str>,
) -> std::result::Result<DeviceEntry, String> {
    let (inline_client, target_name) = match device_query.trim().split_once('/') {
        Some((c, n)) if !c.trim().is_empty() && !n.trim().is_empty() => (Some(c.trim()), n.trim()),
        _ => (None, device_query.trim()),
    };
    let effective_client = client_filter
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(inline_client);

    let matches: Vec<&DeviceEntry> = inv
        .iter()
        .filter(|d| {
            if !d.name.eq_ignore_ascii_case(target_name) {
                return false;
            }
            if let Some(cf) = effective_client {
                d.client
                    .as_deref()
                    .is_some_and(|dc| dc.trim().eq_ignore_ascii_case(cf))
            } else {
                true
            }
        })
        .collect();

    match matches.len() {
        0 => Err(format!(
            "dispositivo `{}` não encontrado no inventory.toml",
            if let Some(c) = effective_client {
                format!("{c}/{target_name}")
            } else {
                target_name.to_string()
            }
        )),
        1 => Ok(matches[0].clone()),
        n => {
            let candidates: Vec<String> = matches
                .iter()
                .map(|d| {
                    let qual = d.display_qualified_name();
                    let site = d
                        .site
                        .as_deref()
                        .map(|s| format!(" · site={s}"))
                        .unwrap_or_default();
                    let vpn = d
                        .vpn
                        .as_deref()
                        .map(|v| format!(" · vpn={v}"))
                        .unwrap_or_default();
                    format!("`{qual}` ({}{site}{vpn})", d.host)
                })
                .collect();
            Err(format!(
                "o dispositivo `{target_name}` existe em {n} clientes diferentes: {}. Informe o parâmetro `client` (ou `device: \"Cliente/{target_name}\"`) para não conectar no cliente errado",
                candidates.join(", ")
            ))
        }
    }
}

fn resolve_connect_device(args: &Value, ctx: &CallCtx<'_>) -> Result<DeviceEntry> {
    let explicit_transport = opt_str(args, "transport").and_then(DeviceTransport::parse);
    let serial_override = opt_str(args, "serial_port").map(str::to_string);
    let baud_override = args["baud_rate"].as_u64().map(|b| b as u32);
    let port_override = args["port"].as_u64().map(|p| p.min(65535) as u16);
    let client_arg = opt_str(args, "client").map(str::to_string);
    let site_arg = opt_str(args, "site").map(str::to_string);
    let vpn_arg = opt_str(args, "vpn").map(str::to_string);

    if let Some(name) = opt_str(args, "device") {
        let root = zerith_core::Paths::resolve().root().to_path_buf();
        let inv = load_inventory(ctx.workspace, Some(&root)).map_err(ToolError::Failed)?;
        let mut dev =
            find_inventory_device(&inv, name, client_arg.as_deref()).map_err(ToolError::Failed)?;
        if let Some(c) = client_arg {
            dev.client = Some(c);
        }
        if let Some(s) = site_arg {
            dev.site = Some(s);
        }
        if let Some(v) = vpn_arg {
            dev.vpn = Some(v);
        }
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
    let name = opt_str(args, "name").unwrap_or(&host).to_string();
    Ok(DeviceEntry {
        name,
        client: client_arg,
        site: site_arg,
        vpn: vpn_arg,
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
    let cmds: Vec<String> = batch_items(args).into_iter().map(|(c, _)| c).collect();
    if cmds.is_empty() {
        return Ok(None);
    }
    Ok(Some(Action::Shell(format!(
        "[{id}: {prog}] batch: {}",
        cmds.join("; ")
    ))))
}

fn inventory_target_path(args: &Value, workspace: &Path, global_root: &Path) -> std::path::PathBuf {
    if opt_str(args, "scope").is_some_and(|s| s.eq_ignore_ascii_case("workspace")) {
        workspace.join(".zerith").join("inventory.toml")
    } else {
        global_root.join("inventory.toml")
    }
}

fn net_inventory(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let action = opt_str(args, "action")
        .unwrap_or("list")
        .trim()
        .to_ascii_lowercase();

    match action.as_str() {
        "save_vpn" => {
            let vpn_name = opt_str(args, "vpn")
                .or_else(|| opt_str(args, "name"))
                .or_else(|| opt_str(args, "client"))
                .ok_or_else(|| {
                    ToolError::BadArguments(
                        "informe `vpn` (ou `client`) e `vpn_config` para `save_vpn`".into(),
                    )
                })?;
            let vpn_cfg = str_arg(args, "vpn_config")?;
            let prof = save_wireguard_profile(&root, vpn_name, vpn_cfg, ctx.workspace)
                .map_err(ToolError::Failed)?;
            return Ok(format!(
                "VPN WireGuard `{}` salva automaticamente em `{}` (Address: {}, AllowedIPs: [{}], Endpoint: {})",
                prof.name,
                prof.path.display(),
                prof.address.as_deref().unwrap_or("-"),
                prof.allowed_ips.join(", "),
                prof.endpoint.as_deref().unwrap_or("-")
            ));
        }
        "upsert" => {
            let name = opt_str(args, "name")
                .or_else(|| opt_str(args, "device"))
                .or_else(|| opt_str(args, "host"))
                .ok_or_else(|| {
                    ToolError::BadArguments(
                        "informe `name` (e `host` ou `serial_port`) para `upsert`".into(),
                    )
                })?
                .to_string();
            let host = opt_str(args, "host")
                .or_else(|| opt_str(args, "serial_port"))
                .unwrap_or(&name)
                .to_string();
            let client = opt_str(args, "client").map(str::to_string);
            let site = opt_str(args, "site").map(str::to_string);
            let mut vpn = opt_str(args, "vpn").map(str::to_string);

            // If vpn_config was passed inline during device onboarding, save the WireGuard profile automatically!
            let mut vpn_note = String::new();
            if let Some(vpn_cfg) = opt_str(args, "vpn_config") {
                let v_name = vpn
                    .clone()
                    .or_else(|| {
                        client
                            .as_ref()
                            .map(|c| format!("wg-{}", c.to_ascii_lowercase()))
                    })
                    .unwrap_or_else(|| format!("wg-{}", name.to_ascii_lowercase()));
                let prof = save_wireguard_profile(&root, &v_name, vpn_cfg, ctx.workspace)
                    .map_err(ToolError::Failed)?;
                vpn = Some(prof.name.clone());
                vpn_note = format!(
                    " + VPN `{}` salva ({})",
                    prof.name,
                    prof.allowed_ips.join(", ")
                );
            }

            // If password was provided, store it in the OS vault automatically and link secret_ref!
            let mut secret_ref = opt_str(args, "secret_ref").map(str::to_string);
            let mut vault_note = String::new();
            if let Some(pw) = opt_str(args, "password") {
                let s_name = secret_ref
                    .clone()
                    .unwrap_or_else(|| derive_secret_ref_name(client.as_deref(), &name));
                match zerith_core::secrets::set_secret(&s_name, pw) {
                    Ok(()) => {
                        secret_ref = Some(s_name.clone());
                        vault_note = format!(" + senha gravada no cofre (`{s_name}`)");
                    }
                    Err(e) => {
                        vault_note = format!(" (aviso cofre: {e})");
                    }
                }
            }

            let explicit_transport = opt_str(args, "transport").and_then(DeviceTransport::parse);
            let serial_port = opt_str(args, "serial_port").map(str::to_string);
            let port_arg = args["port"].as_u64().map(|p| p.min(65535) as u16);
            let transport =
                infer_transport(explicit_transport, &host, serial_port.as_deref(), port_arg);
            let default_port = if transport == DeviceTransport::Telnet {
                23
            } else {
                22
            };
            let tags: Vec<String> = args
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let entry = DeviceEntry {
                name: name.clone(),
                client: client.clone(),
                site,
                vpn,
                host,
                port: port_arg.unwrap_or(default_port),
                user: opt_str(args, "user").unwrap_or("admin").to_string(),
                vendor: opt_str(args, "vendor")
                    .and_then(Vendor::parse)
                    .unwrap_or(Vendor::Generic),
                role: opt_str(args, "role")
                    .and_then(DeviceRole::parse)
                    .unwrap_or(DeviceRole::Lab),
                transport,
                serial_port,
                baud_rate: args["baud_rate"].as_u64().unwrap_or(9600) as u32,
                secret_ref,
                jump_host: opt_str(args, "jump_host").map(str::to_string),
                legacy_ssh: args["legacy_ssh"].as_bool().unwrap_or(false),
                tags,
                description: opt_str(args, "description").map(str::to_string),
            };
            let inv_path = inventory_target_path(args, ctx.workspace, &root);
            let qual = entry.display_qualified_name();
            upsert_inventory_device(&inv_path, entry).map_err(ToolError::Failed)?;
            return Ok(format!(
                "dispositivo `{qual}` cadastrado/atualizado automaticamente em `{}`{vault_note}{vpn_note}",
                inv_path.display()
            ));
        }
        "remove" => {
            let name = opt_str(args, "name")
                .or_else(|| opt_str(args, "device"))
                .ok_or_else(|| ToolError::BadArguments("informe `name` para `remove`".into()))?;
            let client = opt_str(args, "client");
            let inv_path = inventory_target_path(args, ctx.workspace, &root);
            let removed =
                remove_inventory_device(&inv_path, client, name).map_err(ToolError::Failed)?;
            return if removed {
                Ok(format!(
                    "dispositivo `{}` removido de `{}`",
                    client
                        .map(|c| format!("{c}/{name}"))
                        .unwrap_or_else(|| name.to_string()),
                    inv_path.display()
                ))
            } else {
                Ok(format!(
                    "dispositivo `{name}` não encontrado em `{}`",
                    inv_path.display()
                ))
            };
        }
        _ => {}
    }

    let devices = load_inventory(ctx.workspace, Some(&root)).map_err(ToolError::Failed)?;
    let client_f = opt_str(args, "client").map(str::to_ascii_lowercase);
    let site_f = opt_str(args, "site").map(str::to_ascii_lowercase);
    let vpn_f = opt_str(args, "vpn").map(str::to_ascii_lowercase);
    let tag_f = opt_str(args, "tag").map(str::to_ascii_lowercase);
    let vendor_f = opt_str(args, "vendor").and_then(Vendor::parse);
    let role_f = opt_str(args, "role").and_then(DeviceRole::parse);
    let query_f = opt_str(args, "query").map(str::to_ascii_lowercase);

    let filtered: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| {
            if let Some(cf) = &client_f {
                if !d
                    .client
                    .as_deref()
                    .is_some_and(|c| c.to_ascii_lowercase().contains(cf))
                {
                    return false;
                }
            }
            if let Some(sf) = &site_f {
                if !d
                    .site
                    .as_deref()
                    .is_some_and(|s| s.to_ascii_lowercase().contains(sf))
                {
                    return false;
                }
            }
            if let Some(vf) = &vpn_f {
                if !d
                    .vpn
                    .as_deref()
                    .is_some_and(|v| v.to_ascii_lowercase().contains(vf))
                {
                    return false;
                }
            }
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
                    || d.client
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(q)
                    || d.site
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(q)
                    || d.vpn
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(q)
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
        return Ok("nenhum dispositivo encontrado no inventário (use `net_inventory` com `action: \"upsert\"` para a IA cadastrar automaticamente)".into());
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
        let mut meta = Vec::new();
        if let Some(c) = &d.client {
            meta.push(format!("client={c}"));
        }
        if let Some(s) = &d.site {
            meta.push(format!("site={s}"));
        }
        if let Some(v) = &d.vpn {
            meta.push(format!("vpn={v}"));
        }
        meta.extend([
            endpoint,
            format!("user={}", if d.user.is_empty() { "-" } else { &d.user }),
            format!("vendor={}", d.vendor.as_str()),
            format!("role={}", d.role.as_str()),
        ]);
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
        out.push(format!(
            "- **{}** ({})",
            d.display_qualified_name(),
            meta.join(" · ")
        ));
    }
    Ok(out.join("\n"))
}

fn net_vpn(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let action = opt_str(args, "action")
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();

    // Optionally save vpn_config before switching/activating
    if let Some(vpn_cfg) = opt_str(args, "vpn_config") {
        let v_name = opt_str(args, "vpn")
            .or_else(|| opt_str(args, "client"))
            .ok_or_else(|| {
                ToolError::BadArguments("informe `vpn` ao passar `vpn_config`".into())
            })?;
        save_wireguard_profile(&root, v_name, vpn_cfg, ctx.workspace).map_err(ToolError::Failed)?;
    }

    let resolve_target_vpn = || -> Result<String> {
        if let Some(v) = opt_str(args, "vpn") {
            return Ok(v.to_string());
        }
        if let Some(client) = opt_str(args, "client") {
            let inv = load_inventory(ctx.workspace, Some(&root)).unwrap_or_default();
            if let Some(v) = inv
                .iter()
                .find(|d| {
                    d.client
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(client))
                        && d.vpn.is_some()
                })
                .and_then(|d| d.vpn.clone())
            {
                return Ok(v);
            }
            return Ok(format!("wg-{}", client.trim().to_ascii_lowercase()));
        }
        Err(ToolError::BadArguments(
            "informe `vpn` ou `client` para a operação de VPN".into(),
        ))
    };

    match action.as_str() {
        "switch" | "up" => {
            let target = resolve_target_vpn()?;
            let msg = ensure_exclusive_wireguard_vpn(&target, ctx.workspace, &root)
                .map_err(ToolError::Failed)?;
            Ok(msg)
        }
        "down" => {
            let profiles = list_wireguard_profiles(ctx.workspace, Some(&root));
            let targets = if opt_str(args, "vpn").is_some() || opt_str(args, "client").is_some() {
                vec![resolve_target_vpn()?]
            } else {
                detect_active_wireguard_tunnels()
            };
            if targets.is_empty() {
                return Ok("nenhum túnel WireGuard ativo no momento".into());
            }
            let mut msgs = Vec::new();
            for t in targets {
                let p_path = profiles
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&t))
                    .map(|p| p.path.as_path());
                msgs.push(run_wg_tunnel_down(&t, p_path).map_err(ToolError::Failed)?);
            }
            Ok(msgs.join(" · "))
        }
        _ => {
            let active = detect_active_wireguard_tunnels();
            let profiles = list_wireguard_profiles(ctx.workspace, Some(&root));
            let mut lines = Vec::new();
            lines.push(format!(
                "## WireGuard VPN Status\n- **Túneis ativos agora:** {}",
                if active.is_empty() {
                    "nenhum".into()
                } else {
                    active.join(", ")
                }
            ));
            if profiles.is_empty() {
                lines.push("- **Perfis salvos:** nenhum (use `net_inventory(action=\"save_vpn\", vpn=\"wg-cliente\", vpn_config=\"...\")` para salvar)".into());
            } else {
                lines.push("- **Perfis salvos:**".into());
                for p in profiles {
                    let is_on = active.iter().any(|a| a.eq_ignore_ascii_case(&p.name));
                    lines.push(format!(
                        "  - **{}** [{}] — Address: `{}` · AllowedIPs: `[{}]` · Endpoint: `{}`",
                        p.name,
                        if is_on { "ATIVO" } else { "inativo" },
                        p.address.as_deref().unwrap_or("-"),
                        p.allowed_ips.join(", "),
                        p.endpoint.as_deref().unwrap_or("-")
                    ));
                }
            }
            Ok(lines.join("\n"))
        }
    }
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
    let mut dev = resolve_connect_device(args, ctx)?;
    let root = zerith_core::Paths::resolve().root().to_path_buf();

    // Automatic exclusive WireGuard VPN switch before connecting if device has a VPN assigned
    let auto_vpn = args["auto_vpn"].as_bool().unwrap_or(true);
    let mut vpn_status_note = String::new();
    if auto_vpn {
        if let Some(vpn_name) = dev.vpn.as_deref().filter(|s| !s.trim().is_empty()) {
            let msg = ensure_exclusive_wireguard_vpn(vpn_name, ctx.workspace, &root)
                .map_err(ToolError::Failed)?;
            vpn_status_note = format!("[VPN: {msg}]\n");
        }
    }

    let cmd = build_connect_command(&dev).map_err(ToolError::BadArguments)?;
    let mut secrets = Vec::new();
    let direct_password = opt_str(args, "password").map(str::to_string);
    let password = if let Some(ref direct_pw) = direct_password {
        secrets.push(direct_pw.clone());
        Some(direct_pw.clone())
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
            device: Some(dev.display_qualified_name()),
            host: Some(dev.serial_port.clone().unwrap_or_else(|| dev.host.clone())),
            role: Some(dev.role.as_str().into()),
            client: dev.client.clone(),
            vpn: dev.vpn.clone(),
            transport: Some(dev.transport.as_str().into()),
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
            Some(&login_user(&dev, dev.user.trim())),
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

    // 5. Auto-Learn: if `remember` is true (or `client` was specified on an ad-hoc connection),
    // automatically save the device in inventory.toml and its password in the OS vault!
    let remember = args["remember"]
        .as_bool()
        .unwrap_or_else(|| opt_str(args, "device").is_none() && dev.client.is_some());
    if alive && remember {
        if let Some(ref pw) = direct_password {
            let s_name = dev
                .secret_ref
                .clone()
                .unwrap_or_else(|| derive_secret_ref_name(dev.client.as_deref(), &dev.name));
            if zerith_core::secrets::set_secret(&s_name, pw).is_ok() {
                dev.secret_ref = Some(s_name);
            }
        }
        let inv_path = root.join("inventory.toml");
        if upsert_inventory_device(&inv_path, dev.clone()).is_ok() {
            banner.push_str(&format!(
                "\n[inventário: `{}` salvo automaticamente em {}]",
                dev.display_qualified_name(),
                inv_path.display()
            ));
        }
    }

    // A session that drops later is reopened on the same device. A password
    // typed in this call is never kept: only the vault entry it may have
    // been saved to above; without one the session is not reopened.
    if alive && (direct_password.is_none() || dev.secret_ref.is_some()) {
        let mut again = args.clone();
        if let Some(o) = again.as_object_mut() {
            o.remove("password");
            o.insert("remember".into(), Value::Bool(false));
            if direct_password.is_some() {
                o.insert("secret_ref".into(), json!(dev.secret_ref));
            }
        }
        terminal::remember_reopen(&id, again);
    }

    Ok(format!(
        "{vpn_status_note}{id} conectado via {} a {} ({}, vendor={}, role={}{}{}){}\n{banner}",
        dev.transport.as_str(),
        dev.display_qualified_name(),
        dev.serial_port.as_deref().unwrap_or(&dev.host),
        dev.vendor.as_str(),
        dev.role.as_str(),
        dev.site
            .as_deref()
            .map(|s| format!(", site={s}"))
            .unwrap_or_default(),
        dev.vpn
            .as_deref()
            .map(|v| format!(", vpn={v}"))
            .unwrap_or_default(),
        if alive { "" } else { " — sessão encerrou" }
    ))
}

/// Before typing into terminal `id`: a `net_connect` session of this
/// conversation that dropped (ssh timeout, idle reap, the previous turn's
/// device rebooted) is opened again on the same device and keeps its id, so
/// the model's next call just works. Returns the note to put on top of the
/// result; `None` when the terminal is alive or cannot be reopened (the
/// send then fails with the list of open terminals). Idea from Hermes
/// Agent's `process_registry.py` (MIT): a session's processes outlive the
/// call that started them.
pub(crate) fn reopen_if_dropped(
    id: &str,
    ctx: &CallCtx<'_>,
    exec: &Exec<'_>,
) -> Result<Option<String>> {
    let Some(args) = terminal::reopen_args(id, exec.host.session().as_deref()) else {
        return Ok(None);
    };
    let out = net_connect(&args, ctx, exec).map_err(|e| {
        ToolError::Failed(format!(
            "terminal {id} tinha caído e não reabriu ({e}); chame net_connect de novo"
        ))
    })?;
    let Some(new_id) = out.lines().find_map(|l| {
        l.split_whitespace()
            .next()
            .filter(|w| w.starts_with("term-"))
    }) else {
        return Ok(None);
    };
    terminal::rename_terminal(new_id, id);
    Ok(Some(format!(
        "[terminal {id} reaberto: a sessão anterior tinha caído]\n"
    )))
}

/// `commands` items: a string, or `{"cmd": "...", "timeout_secs": N}` for a
/// command with its own deadline.
fn batch_items(args: &Value) -> Vec<(String, Option<u64>)> {
    args["commands"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some((s.trim().to_string(), None)),
                    _ => Some((
                        v["cmd"].as_str()?.trim().to_string(),
                        v["timeout_secs"].as_u64(),
                    )),
                })
                .filter(|(s, _)| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn terminal_batch(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    let items = batch_items(args);
    if items.is_empty() {
        return Err(ToolError::BadArguments(
            "commands precisa ter ao menos um comando".into(),
        ));
    }
    let reopened = reopen_if_dropped(id, ctx, exec)?;
    let cmds: Vec<&String> = items.iter().map(|(c, _)| c).collect();
    let stop_on_error = args["stop_on_error"].as_bool().unwrap_or(true);
    let auto_page = args["auto_page"].as_bool().unwrap_or(true);
    let per_cmd_secs = args["timeout_secs"].as_u64().unwrap_or(20).clamp(2, 300);
    let vendor = terminal::terminal_meta(id)
        .and_then(|(v, _, _)| v)
        .and_then(|v| Vendor::parse(&v))
        .unwrap_or_default();
    let mut paging_off = false;

    let mut sections = Vec::with_capacity(cmds.len());
    for (idx, (cmd, own_secs)) in items.iter().enumerate() {
        let secs = own_secs.unwrap_or(per_cmd_secs).clamp(2, 300);
        if exec.cancel.is_cancelled() {
            sections.push(format!(
                "### [{}/{}] {cmd}\n[cancelado]",
                idx + 1,
                cmds.len()
            ));
            break;
        }
        let (sent, adjusted) = match prepare_network_command(vendor, cmd) {
            Ok(p) => p,
            Err(why) => {
                sections.push(format!("### {cmd}\n[não enviado pelo Zerith: {why}]"));
                continue;
            }
        };
        let (out, alive) = terminal::send_session_command_with(
            "terminal_batch",
            id,
            Some(&sent),
            None,
            true,
            None,
            Duration::from_secs(secs),
            auto_page,
            true,
            exec,
        )?;
        let note = adjusted.map(|n| format!("{n}\n")).unwrap_or_default();
        sections.push(format!("### {sent}\n{note}{out}"));
        if !alive {
            sections.push("[sessão encerrou durante o lote]".into());
            break;
        }
        // Output still pending: the next command would get it.
        if out.contains(terminal::UNSYNCED_NOTE) {
            sections.push(format!(
                "[lote interrompido no comando {}/{}: a sessão não voltou ao prompt]",
                idx + 1,
                cmds.len()
            ));
            break;
        }
        // The pager showed up: paging was never turned off on this session
        // (opened by terminal_open, or the login skipped it). Turn it off
        // once so the rest of the batch comes in one piece.
        if !paging_off && out.contains(terminal::PAGED_NOTE) {
            if let Some(pg) = vendor.disable_paging_cmd() {
                paging_off = true;
                terminal::send_session_command(
                    "terminal_batch",
                    id,
                    Some(pg),
                    None,
                    true,
                    None,
                    Duration::from_secs(8),
                    true,
                    exec,
                )?;
                sections.push(format!("[paginação desativada pelo Zerith: {pg}]"));
            }
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
    Ok(format!(
        "{}{}",
        reopened.unwrap_or_default(),
        sections.join("\n\n")
    ))
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

fn load_config_input_checked(raw: &str, ctx: &CallCtx<'_>) -> Result<String> {
    if !raw.contains('\n') && raw.len() < 512 {
        let candidate = ctx.resolve(raw);
        match ctx
            .policy
            .check_tool("read_file", &Action::Read(candidate.clone()))
        {
            zerith_security::Verdict::Allow => {}
            zerith_security::Verdict::Deny(r) => return Err(ToolError::Denied(r)),
            zerith_security::Verdict::Ask(r) => {
                return Err(ToolError::Denied(format!("aprovação necessária: {r}")))
            }
        }
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return Ok(text);
        }
    }
    Ok(raw.to_string())
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
    let before = load_config_input_checked(before_arg, ctx)?;
    let after = load_config_input_checked(after_arg, ctx)?;
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
        8006 => "Proxmox-VE",
        8080 => "HTTP-Alt",
        8291 => "Winbox",
        8443 => "HTTPS-Alt",
        8728 => "MikroTik-API",
        8729 => "MikroTik-API-SSL",
        _ => "TCP",
    }
}

/// Expands an IPv4 CIDR (`10.0.0.0/28`) or last-octet range (`10.0.0.1-20`)
/// into up to `max_hosts` IP strings.
pub fn expand_subnet_or_range(raw: &str, max_hosts: usize) -> Vec<String> {
    let trimmed = raw.trim();
    if let Some((ip_part, prefix_part)) = trimmed.split_once('/') {
        let Ok(ipv4) = ip_part.trim().parse::<std::net::Ipv4Addr>() else {
            return vec![trimmed.to_string()];
        };
        let Ok(prefix) = prefix_part.trim().parse::<u32>() else {
            return vec![trimmed.to_string()];
        };
        if prefix >= 32 {
            return vec![ipv4.to_string()];
        }
        let clamped_prefix = prefix.clamp(24, 31);
        let mask = if clamped_prefix == 0 {
            0
        } else {
            !0u32 << (32 - clamped_prefix)
        };
        let base = u32::from(ipv4) & mask;
        let count = 1u32 << (32 - clamped_prefix);
        let (start_off, end_off) = if clamped_prefix <= 30 && count > 2 {
            (1, count - 1)
        } else {
            (0, count)
        };
        return (start_off..end_off)
            .take(max_hosts)
            .map(|off| std::net::Ipv4Addr::from(base + off).to_string())
            .collect();
    }
    if let Some((prefix_dots, range_part)) = trimmed.rsplit_once('.') {
        if let Some((start_s, end_s)) = range_part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start_s.trim().parse::<u8>(), end_s.trim().parse::<u8>()) {
                if s <= e {
                    return (s..=e)
                        .take(max_hosts)
                        .map(|last| format!("{prefix_dots}.{last}"))
                        .collect();
                }
            }
        }
    }
    vec![trimmed.to_string()]
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

pub fn fingerprint_probed_host(
    host_rows: &[ProbeRow],
) -> Option<(Vendor, DeviceRole, DeviceTransport, u16, Vec<String>)> {
    let open_rows: Vec<&ProbeRow> = host_rows.iter().filter(|r| r.open).collect();
    if open_rows.is_empty() {
        return None;
    }
    let has_port = |p: u16| open_rows.iter().any(|r| r.port == p);
    let banners = open_rows
        .iter()
        .map(|r| r.detail.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    if has_port(8006) {
        return Some((
            Vendor::Linux,
            DeviceRole::Server,
            DeviceTransport::Ssh,
            if has_port(22) { 22 } else { 8006 },
            vec!["proxmox".into(), "auto-discovered".into()],
        ));
    }
    if has_port(8291)
        || has_port(8728)
        || has_port(8729)
        || banners.contains("rosssh")
        || banners.contains("mikrotik")
    {
        let transport = if has_port(22) {
            DeviceTransport::Ssh
        } else if has_port(23) {
            DeviceTransport::Telnet
        } else {
            DeviceTransport::Ssh
        };
        let port = if transport == DeviceTransport::Telnet {
            23
        } else {
            22
        };
        return Some((
            Vendor::MikrotikRouterOs,
            DeviceRole::Border,
            transport,
            port,
            vec!["mikrotik".into(), "auto-discovered".into()],
        ));
    }
    if banners.contains("huawei") || (has_port(830) && !banners.contains("juniper")) {
        return Some((
            Vendor::HuaweiVrp,
            DeviceRole::Core,
            if has_port(22) {
                DeviceTransport::Ssh
            } else {
                DeviceTransport::Telnet
            },
            if has_port(22) { 22 } else { 23 },
            vec!["huawei".into(), "auto-discovered".into()],
        ));
    }
    if banners.contains("cisco") {
        return Some((
            Vendor::CiscoIos,
            DeviceRole::Core,
            if has_port(22) {
                DeviceTransport::Ssh
            } else {
                DeviceTransport::Telnet
            },
            if has_port(22) { 22 } else { 23 },
            vec!["cisco".into(), "auto-discovered".into()],
        ));
    }
    if banners.contains("fortissh") || has_port(8443) {
        return Some((
            Vendor::FortinetFortiOs,
            DeviceRole::Firewall,
            DeviceTransport::Ssh,
            22,
            vec!["fortigate".into(), "auto-discovered".into()],
        ));
    }
    if has_port(22) {
        return Some((
            Vendor::Linux,
            DeviceRole::Server,
            DeviceTransport::Ssh,
            22,
            vec!["linux".into(), "auto-discovered".into()],
        ));
    }
    if has_port(23) {
        return Some((
            Vendor::Generic,
            DeviceRole::Access,
            DeviceTransport::Telnet,
            23,
            vec!["telnet".into(), "auto-discovered".into()],
        ));
    }
    Some((
        Vendor::Generic,
        DeviceRole::Server,
        DeviceTransport::Ssh,
        open_rows[0].port,
        vec!["auto-discovered".into()],
    ))
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
        for expanded in expand_subnet_or_range(h, 64) {
            if !hosts.iter().any(|x| x == &expanded) {
                hosts.push(expanded);
            }
        }
    }
    if let Some(sub) = opt_str(args, "subnet") {
        for expanded in expand_subnet_or_range(sub, 64) {
            if !hosts.iter().any(|x| x == &expanded) && hosts.len() < 64 {
                hosts.push(expanded);
            }
        }
    }
    if let Some(arr) = args.get("hosts").and_then(Value::as_array) {
        for v in arr.iter().take(64) {
            if let Some(s) = v.as_str() {
                for expanded in expand_subnet_or_range(s, 64) {
                    if !hosts.iter().any(|x| x == &expanded) && hosts.len() < 64 {
                        hosts.push(expanded);
                    }
                }
            }
        }
    }
    if hosts.is_empty() {
        return Err(ToolError::BadArguments(
            "informe `host`, `hosts` ou `subnet` para `net_probe`".into(),
        ));
    }

    let default_ports: Vec<u16> = vec![22, 23, 80, 179, 443, 830, 8006, 8291, 8443, 8728, 8729];
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
        .unwrap_or(1200)
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
    for r in &rows {
        // When scanning multiple hosts (subnet sweep), only print OPEN rows or all rows if <= 3 hosts
        if hosts.len() > 3 && !r.open {
            continue;
        }
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

    // Auto-register discovered hosts into client's inventory when `auto_register_client` is provided
    if let Some(client_name) =
        opt_str(args, "auto_register_client").filter(|s| !s.trim().is_empty())
    {
        let root = zerith_core::Paths::resolve().root().to_path_buf();
        let inv_path = root.join("inventory.toml");
        let site = opt_str(args, "site").map(str::to_string);
        let vpn = opt_str(args, "vpn").map(str::to_string);
        let mut registered = Vec::new();

        for h in &hosts {
            let h_rows: Vec<ProbeRow> = rows.iter().filter(|r| &r.host == h).cloned().collect();
            let Some((vendor, role, transport, port, tags)) = fingerprint_probed_host(&h_rows)
            else {
                continue;
            };
            let prefix = if tags.iter().any(|t| t == "proxmox") {
                "pve"
            } else {
                match vendor {
                    Vendor::MikrotikRouterOs => "mk",
                    Vendor::HuaweiVrp => "huawei",
                    Vendor::CiscoIos => "cisco",
                    Vendor::FortinetFortiOs => "fw",
                    Vendor::Linux => "vm",
                    _ => "host",
                }
            };
            let last_oct = h.rsplit('.').next().unwrap_or(h);
            let auto_name = format!("{prefix}-{last_oct}");
            let default_user = opt_str(args, "user")
                .unwrap_or(if vendor == Vendor::Linux {
                    "root"
                } else {
                    "admin"
                })
                .to_string();
            let entry = DeviceEntry {
                name: auto_name.clone(),
                client: Some(client_name.to_string()),
                site: site.clone(),
                vpn: vpn.clone(),
                host: h.clone(),
                port,
                user: default_user,
                vendor,
                role,
                transport,
                tags,
                ..Default::default()
            };
            if upsert_inventory_device(&inv_path, entry).is_ok() {
                registered.push(format!(
                    "`{client_name}/{auto_name}` ({h} · {})",
                    vendor.as_str()
                ));
            }
        }
        if !registered.is_empty() {
            out.push_str(&format!(
                "\n\n### Auto-Cadastro no Inventário ({})\nDispositivos registrados automaticamente: {}",
                inv_path.display(),
                registered.join(", ")
            ));
        }
    }

    Ok(out)
}

fn select_target_terminals(args: &Value) -> Result<Vec<terminal::TerminalSnapshot>> {
    let active = terminal::list_active_terminals();
    let explicit_ids: Vec<String> = if let Some(single) = opt_str(args, "id") {
        vec![single.to_string()]
    } else if let Some(arr) = args.get("ids").and_then(Value::as_array) {
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };
    let client_f = opt_str(args, "client").map(str::to_ascii_lowercase);
    let vendor_f = opt_str(args, "vendor").map(str::to_ascii_lowercase);

    let selected: Vec<terminal::TerminalSnapshot> = active
        .into_iter()
        .filter(|t| {
            if !t.alive {
                return false;
            }
            if !explicit_ids.is_empty() {
                return explicit_ids.iter().any(|id| id == &t.id);
            }
            if let Some(cf) = &client_f {
                if !t
                    .client
                    .as_deref()
                    .is_some_and(|c| c.to_ascii_lowercase().contains(cf))
                {
                    return false;
                }
            }
            if let Some(vf) = &vendor_f {
                if !t
                    .vendor
                    .as_deref()
                    .is_some_and(|v| v.to_ascii_lowercase().contains(vf))
                {
                    return false;
                }
            }
            true
        })
        .collect();

    if selected.is_empty() {
        return Err(ToolError::Failed(
            "nenhum terminal ativo compatível encontrado (abra sessões com `net_connect` primeiro)"
                .into(),
        ));
    }
    Ok(selected)
}

type BroadcastRow = (terminal::TerminalSnapshot, Result<(String, bool)>, u128);

fn net_broadcast(args: &Value, _ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let command = str_arg(args, "command")?;
    let targets = select_target_terminals(args)?;
    let auto_page = args["auto_page"].as_bool().unwrap_or(true);
    let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(20).clamp(2, 300);

    let results: Vec<BroadcastRow> = std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .into_iter()
            .map(|snap| {
                s.spawn(move || {
                    let started = std::time::Instant::now();
                    let res = terminal::send_session_command(
                        "net_broadcast",
                        &snap.id,
                        Some(command),
                        None,
                        true,
                        None,
                        Duration::from_secs(timeout_secs),
                        auto_page,
                        exec,
                    );
                    (snap, res, started.elapsed().as_millis())
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    let mut sections = Vec::with_capacity(results.len() + 1);
    sections.push(format!(
        "## Net Broadcast (`{command}` em {} host(s) em paralelo)",
        results.len()
    ));
    for (snap, res, ms) in results {
        let title = snap.device.as_deref().unwrap_or(&snap.command);
        let host_label = snap.host.as_deref().unwrap_or("-");
        match res {
            Ok((out, _alive)) => {
                sections.push(format!(
                    "### {} ({} · {} · {} ms)\n```text\n{}\n```",
                    title, snap.id, host_label, ms, out
                ));
            }
            Err(e) => {
                sections.push(format!(
                    "### {} ({} · {} · ERRO)\n{}",
                    title, snap.id, host_label, e
                ));
            }
        }
    }
    Ok(sections.join("\n\n"))
}

fn default_backup_command_for_vendor(vendor_str: Option<&str>) -> &'static str {
    match vendor_str
        .and_then(Vendor::parse)
        .unwrap_or(Vendor::Generic)
    {
        Vendor::CiscoIos | Vendor::CiscoNxos | Vendor::CiscoXr => "show running-config",
        Vendor::HuaweiVrp => "display current-configuration",
        Vendor::JuniperJunos => "show configuration",
        Vendor::MikrotikRouterOs => "/export",
        Vendor::FortinetFortiOs => "show full-configuration",
        Vendor::Linux => "uname -a && ip -br addr && ip route",
        Vendor::Generic => "show running-config",
    }
}

fn sanitize_path_component(raw: &str) -> String {
    let s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "default".into()
    } else {
        s
    }
}

fn net_backup(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let targets = select_target_terminals(args)?;
    let custom_cmd = opt_str(args, "command");
    let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(45).clamp(5, 300);
    let mut reports = Vec::with_capacity(targets.len());

    for snap in targets {
        let cmd =
            custom_cmd.unwrap_or_else(|| default_backup_command_for_vendor(snap.vendor.as_deref()));
        let (cfg_out, _alive) = terminal::send_session_command(
            "net_backup",
            &snap.id,
            Some(cmd),
            None,
            true,
            None,
            Duration::from_secs(timeout_secs),
            true,
            exec,
        )?;
        let client_dir = sanitize_path_component(snap.client.as_deref().unwrap_or("geral"));
        let dev_raw = snap
            .device
            .as_deref()
            .and_then(|d| d.rsplit('/').next())
            .or(snap.host.as_deref())
            .unwrap_or(&snap.id);
        let dev_dir = sanitize_path_component(dev_raw);
        let dir = ctx
            .workspace
            .join("backups")
            .join(&client_dir)
            .join(&dev_dir);
        std::fs::create_dir_all(&dir)?;

        let latest_path = dir.join("latest.cfg");
        let previous = std::fs::read_to_string(&latest_path).ok();
        if let Some(ref prev_text) = previous {
            let _ = std::fs::write(dir.join("previous.cfg"), prev_text);
        }
        std::fs::write(&latest_path, &cfg_out)?;

        let vendor = snap.vendor.as_deref().and_then(Vendor::parse);
        let diff_summary = match previous {
            Some(prev_text) => compute_hierarchical_net_diff(&prev_text, &cfg_out, vendor, false),
            None => {
                "Primeiro backup registrado para este equipamento (sem versão anterior para diff)."
                    .into()
            }
        };
        reports.push(format!(
            "### Backup `{}` ({})\n- **Arquivo salvo:** `{}` ({} bytes)\n\n{}",
            snap.device.as_deref().unwrap_or(&snap.id),
            cmd,
            latest_path.display(),
            cfg_out.len(),
            diff_summary
        ));
    }
    Ok(reports.join("\n\n---\n\n"))
}

pub fn discover_local_serial_ports() -> Vec<(String, String)> {
    let mut found = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match '\\(COM\\d+\\)' } | ForEach-Object { $_.Name }",
            ])
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let l = line.trim();
                if let Some(start) = l.rfind("(COM") {
                    if let Some(end) = l[start..].find(')') {
                        let com = l[start + 1..start + end].trim().to_string();
                        if !found.iter().any(|(c, _)| c == &com) {
                            found.push((com, l.to_string()));
                        }
                    }
                }
            }
        }
        if found.is_empty() {
            if let Ok(out) = std::process::Command::new("powershell")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[System.IO.Ports.SerialPort]::GetPortNames()",
                ])
                .output()
            {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let com = line.trim();
                    if com.starts_with("COM") && !found.iter().any(|(c, _)| c == com) {
                        found.push((com.to_string(), "Porta Serial Windows".into()));
                    }
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(entries) = std::fs::read_dir("/dev") {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("ttyUSB")
                    || name.starts_with("ttyACM")
                    || name.starts_with("cu.usb")
                    || name.starts_with("tty.usb")
                {
                    found.push((
                        format!("/dev/{name}"),
                        "Adaptador USB-Serial Console".into(),
                    ));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn serial_ports(_args: &Value, _ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let ports = discover_local_serial_ports();
    if ports.is_empty() {
        return Ok(
            "Nenhuma porta serial/console detectada no momento (conecte o cabo USB-Serial FTDI/CH340/PL2303 e tente novamente).".into(),
        );
    }
    let mut lines = vec!["## Portas Seriais / Console Detectadas".to_string()];
    for (port, desc) in ports {
        lines.push(format!(
            "- **`{port}`** — {desc} (conectar com `net_connect(transport=\"serial\", serial_port=\"{port}\", baud_rate=9600)`)"
        ));
    }
    Ok(lines.join("\n"))
}

pub fn build_scp_args(
    dev: &DeviceEntry,
    direction: &str,
    local_path: &Path,
    remote_path: &str,
) -> std::result::Result<Vec<String>, String> {
    validate_ssh_token("host", &dev.host)?;
    let user = if dev.user.trim().is_empty() {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "root".into())
    } else {
        dev.user.trim().to_string()
    };
    validate_ssh_token("user", &user)?;
    let rem_trimmed = remote_path.trim();
    if rem_trimmed.is_empty() || rem_trimmed.chars().any(|c| c.is_control()) {
        return Err("remote_path inválido".into());
    }

    let mut args = vec![
        "-r".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "ConnectTimeout=15".to_string(),
    ];
    if dev.port != 22 && dev.port != 0 {
        args.push("-P".to_string());
        args.push(dev.port.to_string());
    }
    if let Some(jump) = dev
        .jump_host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        validate_ssh_token("jump_host", jump)?;
        args.push("-J".to_string());
        args.push(jump.to_string());
    }
    if dev.legacy_ssh {
        args.extend([
            "-o".to_string(),
            "KexAlgorithms=+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1".to_string(),
            "-o".to_string(),
            "HostKeyAlgorithms=+ssh-rsa".to_string(),
            "-o".to_string(),
            "PubkeyAcceptedAlgorithms=+ssh-rsa".to_string(),
        ]);
    }
    let remote_spec = format!("{user}@{}:{rem_trimmed}", dev.host.trim());
    let local_str = local_path.to_string_lossy().into_owned();
    match direction.trim().to_ascii_lowercase().as_str() {
        "upload" | "up" | "put" | "push" => {
            args.push(local_str);
            args.push(remote_spec);
            Ok(args)
        }
        "download" | "down" | "get" | "pull" => {
            args.push(remote_spec);
            args.push(local_str);
            Ok(args)
        }
        other => Err(format!(
            "direction inválido `{other}` (use `upload` ou `download`)"
        )),
    }
}

fn net_transfer(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let direction = str_arg(args, "direction")?;
    let local_raw = str_arg(args, "local_path")?;
    let remote_raw = str_arg(args, "remote_path")?;
    let local_path = ctx.resolve(local_raw);

    let dir_lower = direction.trim().to_ascii_lowercase();
    let is_download = matches!(dir_lower.as_str(), "download" | "down" | "get" | "pull");
    let is_upload = matches!(dir_lower.as_str(), "upload" | "up" | "put" | "push");
    if is_download {
        match ctx
            .policy
            .check_tool("write_file", &Action::Write(local_path.clone()))
        {
            zerith_security::Verdict::Allow => {}
            zerith_security::Verdict::Deny(r) => return Err(ToolError::Denied(r)),
            zerith_security::Verdict::Ask(r) => {
                return Err(ToolError::Denied(format!("aprovação necessária: {r}")))
            }
        }
        exec.host.before_write(&local_path)?;
    } else if is_upload {
        match ctx
            .policy
            .check_tool("read_file", &Action::Read(local_path.clone()))
        {
            zerith_security::Verdict::Allow => {}
            zerith_security::Verdict::Deny(r) => return Err(ToolError::Denied(r)),
            zerith_security::Verdict::Ask(r) => {
                return Err(ToolError::Denied(format!("aprovação necessária: {r}")))
            }
        }
    } else {
        return Err(ToolError::BadArguments(format!(
            "direction inválido `{direction}` (use `upload` ou `download`)"
        )));
    }

    let dev = resolve_connect_device(args, ctx)?;
    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let auto_vpn = args["auto_vpn"].as_bool().unwrap_or(true);
    let mut vpn_note = String::new();
    if auto_vpn {
        if let Some(vpn_name) = dev.vpn.as_deref().filter(|s| !s.trim().is_empty()) {
            let msg = ensure_exclusive_wireguard_vpn(vpn_name, ctx.workspace, &root)
                .map_err(ToolError::Failed)?;
            vpn_note = format!("[VPN: {msg}]\n");
        }
    }

    if is_download {
        if let Some(parent) = local_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    } else if !local_path.exists() {
        return Err(ToolError::Failed(format!(
            "local_path não existe para upload: {}",
            local_path.display()
        )));
    }

    let scp_args = build_scp_args(&dev, direction, &local_path, remote_raw)
        .map_err(ToolError::BadArguments)?;
    let started = std::time::Instant::now();
    let out = std::process::Command::new("scp")
        .args(&scp_args)
        .output()
        .map_err(|e| ToolError::Failed(format!("falha ao executar `scp`: {e}")))?;
    let elapsed_ms = started.elapsed().as_millis();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(ToolError::Failed(format!(
            "scp falhou ({}): {}",
            out.status,
            if stderr.is_empty() {
                "verifique chave SSH ou permissão no destino".into()
            } else {
                stderr
            }
        )));
    }
    let size_bytes = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    Ok(format!(
        "{vpn_note}## Net Transfer ({})\n- **Dispositivo:** `{}` ({})\n- **Local:** `{}` ({} bytes)\n- **Remoto:** `{}`\n- **Tempo:** {} ms",
        direction.to_ascii_uppercase(),
        dev.display_qualified_name(),
        dev.host,
        local_path.display(),
        size_bytes,
        remote_raw,
        elapsed_ms
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub struct RttStats {
    pub sent: usize,
    pub received: usize,
    pub loss_pct: f64,
    pub min_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub jitter_ms: f64,
}

pub fn compute_rtt_jitter_stats(sent: usize, samples_ms: &[f64]) -> RttStats {
    let received = samples_ms.len();
    let loss_pct = if sent == 0 {
        0.0
    } else {
        ((sent.saturating_sub(received)) as f64 / sent as f64) * 100.0
    };
    if samples_ms.is_empty() {
        return RttStats {
            sent,
            received: 0,
            loss_pct,
            min_ms: 0.0,
            avg_ms: 0.0,
            max_ms: 0.0,
            jitter_ms: 0.0,
        };
    }
    let mut min_ms = f64::MAX;
    let mut max_ms = 0.0f64;
    let mut sum = 0.0f64;
    let mut diff_sum = 0.0f64;
    for (i, &v) in samples_ms.iter().enumerate() {
        if v < min_ms {
            min_ms = v;
        }
        if v > max_ms {
            max_ms = v;
        }
        sum += v;
        if i > 0 {
            diff_sum += (v - samples_ms[i - 1]).abs();
        }
    }
    let avg_ms = sum / received as f64;
    let jitter_ms = if received > 1 {
        diff_sum / (received - 1) as f64
    } else {
        0.0
    };
    RttStats {
        sent,
        received,
        loss_pct,
        min_ms,
        avg_ms,
        max_ms,
        jitter_ms,
    }
}

fn ping_with_df_mtu(host: &str, total_ip_mtu: u16) -> bool {
    let payload = total_ip_mtu.saturating_sub(28);
    let payload_str = payload.to_string();
    let out = if cfg!(windows) {
        std::process::Command::new("ping")
            .args(["-n", "1", "-w", "900", "-f", "-l", &payload_str, host])
            .output()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("ping")
            .args(["-c", "1", "-W", "900", "-D", "-s", &payload_str, host])
            .output()
    } else {
        std::process::Command::new("ping")
            .args(["-c", "1", "-W", "1", "-M", "do", "-s", &payload_str, host])
            .output()
    };
    let Ok(res) = out else {
        return false;
    };
    if !res.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&res.stdout).to_ascii_lowercase();
    !stdout.contains("fragment")
        && !stdout.contains("100% loss")
        && !stdout.contains("100% packet loss")
}

fn discover_path_mtu(host: &str) -> Option<(u16, &'static str)> {
    const CANDIDATES: &[(u16, &str)] = &[
        (1500, "Ethernet Padrão (1500) — sem encapsulamento redutor"),
        (1492, "PPPoE (1492) — overhead de 8 bytes PPPoE"),
        (1472, "VLAN / MPLS / QinQ (1472)"),
        (
            1420,
            "WireGuard VPN Padrão (1420) — overhead de 80 bytes WireGuard",
        ),
        (1400, "IPsec / L2TP / VXLAN (1400)"),
        (1360, "GRE + IPsec / MPLS Duplo (1360)"),
        (1280, "IPv6 Mínimo / Túnel Restrito (1280)"),
    ];
    for &(mtu, label) in CANDIDATES {
        if ping_with_df_mtu(host, mtu) {
            return Some((mtu, label));
        }
    }
    None
}

fn net_path(args: &Value, ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Instant;

    let root = zerith_core::Paths::resolve().root().to_path_buf();
    let (target_host, target_port, label_prefix) = if opt_str(args, "device").is_some() {
        let dev = resolve_connect_device(args, ctx)?;
        if let Some(vpn_name) = dev.vpn.as_deref().filter(|s| !s.trim().is_empty()) {
            let _ = ensure_exclusive_wireguard_vpn(vpn_name, ctx.workspace, &root);
        }
        (
            dev.host.clone(),
            args["port"].as_u64().map(|p| p as u16).unwrap_or(dev.port),
            format!("{} ({})", dev.display_qualified_name(), dev.host),
        )
    } else {
        let h = str_arg(args, "host")?.trim().to_string();
        let p = args["port"].as_u64().unwrap_or(22).min(65535) as u16;
        (h.clone(), p, h)
    };
    validate_ssh_token("host", &target_host).map_err(ToolError::BadArguments)?;

    let count = args["count"].as_u64().unwrap_or(4).clamp(1, 10) as usize;
    let discover_mtu = args["discover_mtu"].as_bool().unwrap_or(true);
    let run_trace = args["traceroute"].as_bool().unwrap_or(false);

    // 1. Measure multi-sample RTT & Jitter via TCP connect probes
    let mut samples_ms = Vec::with_capacity(count);
    if let Some(addr) = (target_host.as_str(), target_port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
    {
        for i in 0..count {
            let started = Instant::now();
            if TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).is_ok() {
                samples_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            if i + 1 < count {
                std::thread::sleep(Duration::from_millis(60));
            }
        }
    }
    let stats = compute_rtt_jitter_stats(count, &samples_ms);

    let mut lines = vec![format!("## Net Path Diagnostics — `{label_prefix}`")];
    lines.push(format!(
        "- **Latência & Jitter (TCP/{target_port}):** enviados={}, recebidos={}, perda=`{:.0}%` · min=`{:.1} ms` · avg=`{:.1} ms` · max=`{:.1} ms` · **jitter=`{:.1} ms`**",
        stats.sent, stats.received, stats.loss_pct, stats.min_ms, stats.avg_ms, stats.max_ms, stats.jitter_ms
    ));

    // 2. Automatic Path MTU Discovery (Don't-Fragment ICMP)
    if discover_mtu {
        match discover_path_mtu(&target_host) {
            Some((mtu, profile)) => {
                let mss = mtu.saturating_sub(40);
                lines.push(format!(
                    "- **Path MTU Descoberto (DF=1):** **`{mtu} bytes`** ({profile}) · TCP MSS recomendado: `ip tcp adjust-mss {mss}`"
                ));
            }
            None => {
                lines.push(
                    "- **Path MTU (DF=1):** ICMP bloqueado ou indisponível no caminho (use TCP MSS clamping preventivo `1380` se houver suspeita de blackhole em túnel)".into(),
                );
            }
        }
    }

    // 3. Optional Traceroute
    if run_trace {
        let trace_out = if cfg!(windows) {
            std::process::Command::new("tracert")
                .args(["-d", "-h", "15", "-w", "800", &target_host])
                .output()
        } else {
            std::process::Command::new("traceroute")
                .args(["-n", "-m", "15", "-w", "1", &target_host])
                .output()
        };
        if let Ok(out) = trace_out {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !text.is_empty() {
                lines.push(format!("\n### Traceroute\n```text\n{text}\n```"));
            }
        }
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descrição diz o que faz, quando usar e o que devolve; propaganda só
    /// gasta schema em toda requisição.
    #[test]
    fn network_descriptions_carry_no_marketing() {
        for t in all() {
            let s = t.spec();
            let text = format!("{} {} {}", s.description, s.returns, s.parameters).to_lowercase();
            for word in [
                "alta velocidade",
                "poderos",
                "100%",
                "automaticamente",
                "!\"",
            ] {
                assert!(!text.contains(word), "{}: {word}", s.name);
            }
            assert!(s.description.chars().count() < 600, "{}", s.name);
        }
    }

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
        for bad in [
            "10.0.0.1;id",
            "host > /tmp/pwned",
            "host</etc/passwd",
            "host$(id)",
            "host`id`",
            "host(1)",
            "host'x",
            "host\"x",
            "host\\x",
            "host*",
        ] {
            dev.host = bad.into();
            assert!(build_ssh_command(&dev).is_err(), "{bad}");
        }
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

    /// A router on a local socket (telnet transport, no IAC): prompt, then
    /// per command: `route check` loops a RouterOS monitor frame until `q`;
    /// `/slow` prints changing lines until Ctrl+C; `show run` pages three
    /// times on ` --More-- `; anything else answers `ok: <cmd>`.
    fn fake_router(prompt: &'static str) -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = l.accept() else { return };
            s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            // Err: connection gone; Ok(None): nothing typed yet.
            let key = |s: &mut std::net::TcpStream| -> std::result::Result<Option<u8>, ()> {
                let mut b = [0u8; 1];
                match s.read(&mut b) {
                    Ok(0) => Err(()),
                    Ok(_) => Ok(Some(b[0])),
                    Err(_) => Ok(None),
                }
            };
            let wait_for =
                |s: &mut std::net::TcpStream,
                 want: u8,
                 tick: &mut dyn FnMut(&mut std::net::TcpStream)| {
                    let until = std::time::Instant::now() + Duration::from_secs(30);
                    while std::time::Instant::now() < until {
                        match key(s) {
                            Err(()) => return false,
                            Ok(Some(b)) if b == want => return true,
                            _ => tick(s),
                        }
                    }
                    false
                };
            let _ = s.write_all(prompt.as_bytes());
            let mut line = Vec::new();
            loop {
                let b = match key(&mut s) {
                    Err(()) => return,
                    Ok(None) => continue,
                    Ok(Some(b)) => b,
                };
                if b != b'\r' && b != b'\n' {
                    line.push(b);
                    continue;
                }
                if line.is_empty() {
                    continue;
                }
                let cmd = String::from_utf8_lossy(&std::mem::take(&mut line)).to_string();
                let _ = s.write_all(format!("{cmd}\r\n").as_bytes());
                if cmd.contains("route check") {
                    let ok = wait_for(&mut s, b'q', &mut |s| {
                        let _ = s.write_all(
                            b"      status: ok\r\n   interface: sfp-sfpplus1\r\n     nexthop: 179.108.156.1\r\n-- [Q quit|D dump|C-z pause]\r\n",
                        );
                    });
                    if !ok {
                        return;
                    }
                } else if cmd == "/slow" {
                    let mut n = 0;
                    if !wait_for(&mut s, 3, &mut |s| {
                        n += 1;
                        let _ = s.write_all(format!("aguardando {n}\r\n").as_bytes());
                        std::thread::sleep(Duration::from_millis(200));
                    }) {
                        return;
                    }
                    let _ = s.write_all(b"^C\r\n");
                } else if cmd == "show run" {
                    for page in 0..3 {
                        let _ = s.write_all(
                            format!("linha {page}a\r\nlinha {page}b\r\n --More-- ").as_bytes(),
                        );
                        if !wait_for(&mut s, b' ', &mut |_| {}) {
                            return;
                        }
                    }
                    let _ = s.write_all(b"\r\nend\r\n");
                } else if let Some(x) = cmd
                    .strip_prefix(":put \"")
                    .and_then(|r| r.strip_suffix('"'))
                {
                    let _ = s.write_all(format!("{x}\r\n").as_bytes());
                } else {
                    let _ = s.write_all(format!("ok: {cmd}\r\n").as_bytes());
                }
                let _ = s.write_all(prompt.as_bytes());
            }
        });
        port
    }

    fn with_fake_router(
        prompt: &'static str,
        vendor: &str,
        f: impl FnOnce(&str, &CallCtx<'_>, &Exec<'_>),
    ) {
        let port = fake_router(prompt);
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
            deadline: std::time::Instant::now() + Duration::from_secs(60),
            timeout: Duration::from_secs(60),
            host: &host,
        };
        let target = format!("telnet://127.0.0.1:{port}");
        let (id, _, alive) = terminal::open_with_meta(
            OpenSpec {
                command: &target,
                cwd: &ws,
                expect: None,
                timeout_dur: Duration::from_secs(5),
                auto_page: true,
                secrets: Vec::new(),
                vendor: Some(vendor.into()),
                device: None,
                host: None,
                role: None,
                client: None,
                vpn: None,
                transport: Some("telnet".into()),
            },
            &exec,
        )
        .unwrap();
        assert!(alive);
        f(&id, &ctx, &exec);
        let _ = terminal::close_terminal(&id);
    }

    /// A RouterOS on a local socket that behaves like a real one over ssh:
    /// the echo comes in two pieces with the prompt redrawn in the middle
    /// (`/ip fir\r[admin@MK] > /ip firewall ...`) and a pause right after the
    /// redrawn prompt; each answer takes 30 ms; a `print` without
    /// `without-paging` stops every 60 lines on `-- [Q quit|D dump|down]`
    /// until a space comes back. `/ip route print` has 600 rows, other
    /// commands 5. Every answer ends with `fim: <cmd>`.
    fn fake_routeros() -> u16 {
        use std::io::{Read, Write};
        const PROMPT: &[u8] = b"[admin@MK] > ";
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = l.accept() else { return };
            let _ = s.set_nodelay(true);
            let _ = s.write_all(PROMPT);
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            while let Ok(1) = s.read(&mut b) {
                if b[0] != b'\r' && b[0] != b'\n' {
                    line.push(b[0]);
                    continue;
                }
                if line.is_empty() {
                    continue;
                }
                let cmd = String::from_utf8_lossy(&std::mem::take(&mut line)).to_string();
                let cut = cmd.len().min(7);
                let _ = s.write_all(format!("{}\r", &cmd[..cut]).as_bytes());
                let _ = s.write_all(PROMPT);
                std::thread::sleep(Duration::from_millis(150));
                let _ = s.write_all(format!("{cmd}\r\n").as_bytes());
                std::thread::sleep(Duration::from_millis(30));
                if let Some(x) = cmd
                    .strip_prefix(":put \"")
                    .and_then(|r| r.strip_suffix('"'))
                {
                    let _ = s.write_all(format!("{x}\r\n").as_bytes());
                    let _ = s.write_all(PROMPT);
                    continue;
                }
                let rows = if cmd.contains("route print") { 600 } else { 5 };
                let paged = cmd.contains("print") && !cmd.contains("without-paging");
                for i in 0..rows {
                    if paged && i > 0 && i % 60 == 0 {
                        let _ = s.write_all(b"-- [Q quit|D dump|down]");
                        loop {
                            match s.read(&mut b) {
                                Ok(1) if b[0] == b' ' => break,
                                Ok(1) => {}
                                _ => return,
                            }
                        }
                        std::thread::sleep(Duration::from_millis(30));
                        let _ = s.write_all(b"\r\x1b[K");
                    }
                    let _ = s.write_all(
                        format!("{i:>4}   dst-address=10.0.{i}.0/24 gateway=179.108.156.1 distance=1\r\n")
                            .as_bytes(),
                    );
                }
                let _ = s.write_all(format!("fim: {cmd}\r\n").as_bytes());
                let _ = s.write_all(PROMPT);
            }
        });
        port
    }

    #[test]
    fn a_batch_of_six_routeros_prints_ends_on_the_prompt_fast() {
        let port = fake_routeros();
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
            deadline: std::time::Instant::now() + Duration::from_secs(60),
            timeout: Duration::from_secs(60),
            host: &host,
        };
        let target = format!("telnet://127.0.0.1:{port}");
        let (id, _, _) = terminal::open_with_meta(
            OpenSpec {
                command: &target,
                cwd: &ws,
                expect: None,
                timeout_dur: Duration::from_secs(5),
                auto_page: true,
                secrets: Vec::new(),
                vendor: Some("mikrotik_router_os".into()),
                device: None,
                host: None,
                role: None,
                client: None,
                vpn: None,
                transport: Some("telnet".into()),
            },
            &exec,
        )
        .unwrap();
        let cmds = [
            "/ip firewall filter print terse",
            "/ip firewall nat print terse",
            "/ip route print terse",
            "/interface print",
            "/ip address print",
            "/system resource print",
        ];
        let started = std::time::Instant::now();
        let out = terminal_batch(&json!({"id": id, "commands": cmds}), &ctx, &exec).unwrap();
        let took = started.elapsed();
        let _ = terminal::close_terminal(&id);
        eprintln!("lote de 6 comandos no RouterOS falso: {took:?}");
        // Every section holds its own answer and only its own.
        let sections: Vec<&str> = out.split("### ").skip(1).collect();
        assert_eq!(sections.len(), 6, "{out}");
        for (sec, cmd) in sections.iter().zip(cmds) {
            // The header is what was sent (`print` gets `without-paging`).
            let sent = sec.lines().next().unwrap();
            assert!(
                sent.starts_with(&cmd[..cmd.find(" print").unwrap()]),
                "{sec}"
            );
            assert!(sec.contains(&format!("fim: {sent}")), "{cmd}: {sec}");
            assert_eq!(sec.matches("fim: ").count(), 1, "{cmd}: {sec}");
        }
        assert!(out.contains(" 599   dst-address=10.0.599.0/24"), "{out}");
        assert!(!out.contains("__ZB_END_"), "{out}");
        // The end marker costs one more round trip per command (here the
        // 150 ms redraw of its echo).
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    /// RouterOS on a local socket as a real CCR (7.24) looks over ssh.
    /// Without console options the echo is redrawn with the prompt in the
    /// middle (`/ip firewall con\r[joao@Masterlink] > /ip firewall ...`) and
    /// cut at 48 columns with the line's end drawn over it; a login name
    /// ending in `+cet511w4098h` (dumb, 511 columns) gets the plain echo.
    /// One line at a time (what is typed meanwhile waits), Ctrl+C clears the
    /// line. `connection print count-only` takes 8 s and ignores Ctrl+C/`q`,
    /// then prints `1`; `:put "X"` prints X; anything else `ok: <cmd>`.
    /// `login`: asks `Login:`/`Password:` and records the name typed.
    fn fake_routeros_cli(login: Option<std::sync::Arc<std::sync::Mutex<String>>>) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        const PROMPT: &str = "[joao@Masterlink] > ";
        const COLS: usize = 48;
        fn read_line(s: &mut TcpStream) -> Option<String> {
            let (mut line, mut b) = (Vec::new(), [0u8; 1]);
            loop {
                if !matches!(s.read(&mut b), Ok(1)) {
                    return None;
                }
                match b[0] {
                    b'\r' | b'\n' if !line.is_empty() => {
                        return Some(String::from_utf8_lossy(&line).into())
                    }
                    b'\r' | b'\n' => {}
                    3 => {
                        line.clear();
                        let _ = s.write_all(format!("\r\n{PROMPT}").as_bytes());
                    }
                    c => line.push(c),
                }
            }
        }
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = l.accept() else { return };
            let _ = s.set_nodelay(true);
            let mut dumb = false;
            if let Some(seen) = &login {
                let _ = s.write_all(b"Login: ");
                let Some(user) = read_line(&mut s) else {
                    return;
                };
                dumb = user.ends_with("+cet511w4098h");
                *seen.lock().unwrap() = user;
                let _ = s.write_all(b"Password: ");
                if read_line(&mut s).is_none() {
                    return;
                }
                let _ = s.write_all(b"\r\n");
            }
            let _ = s.write_all(PROMPT.as_bytes());
            while let Some(cmd) = read_line(&mut s) {
                if dumb {
                    let _ = s.write_all(format!("{cmd}\r\n").as_bytes());
                } else {
                    let first = &cmd[..cmd.len().min(24)];
                    let _ = s.write_all(format!("{first}\r{PROMPT}").as_bytes());
                    std::thread::sleep(Duration::from_millis(150));
                    let shown = &cmd[..cmd.len().min(COLS)];
                    let end = if cmd.len() > COLS {
                        &cmd[cmd.len() - 2..]
                    } else {
                        ""
                    };
                    let _ = s.write_all(format!("{shown}\r{end}\r\n").as_bytes());
                }
                let answer = if let Some(x) = cmd
                    .strip_prefix(":put \"")
                    .and_then(|r| r.strip_suffix('"'))
                {
                    x.to_string()
                } else if cmd.contains("connection print count-only") {
                    std::thread::sleep(Duration::from_secs(8));
                    "1".into()
                } else {
                    format!("ok: {cmd}")
                };
                let _ = s.write_all(format!("{answer}\r\n{PROMPT}").as_bytes());
            }
        });
        port
    }

    /// Workspace, bypass policy and a 60 s deadline for a test call.
    fn with_env(f: impl FnOnce(&CallCtx<'_>, &Exec<'_>)) {
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
            deadline: std::time::Instant::now() + Duration::from_secs(60),
            timeout: Duration::from_secs(60),
            host: &host,
        };
        f(&ctx, &exec);
    }

    #[test]
    fn routeros_login_asks_for_a_dumb_wide_console() {
        let mut dev = DeviceEntry {
            host: "10.0.0.1".into(),
            user: "admin".into(),
            vendor: Vendor::MikrotikRouterOs,
            ..Default::default()
        };
        let ssh = |d: &DeviceEntry| build_ssh_command(d).unwrap();
        assert!(
            ssh(&dev).ends_with(" admin+cet511w4098h@10.0.0.1"),
            "{}",
            ssh(&dev)
        );
        dev.user = "admin+ct80w".into();
        assert!(
            ssh(&dev).ends_with(" admin+ct80w@10.0.0.1"),
            "{}",
            ssh(&dev)
        );
        dev.user = "admin".into();
        dev.vendor = Vendor::CiscoIos;
        assert!(ssh(&dev).ends_with(" admin@10.0.0.1"), "{}", ssh(&dev));

        // Telnet: the name typed carries the options; the device answers
        // with a plain echo, no prompt redrawn inside it.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let port = fake_routeros_cli(Some(seen.clone()));
        with_env(|ctx, exec| {
            let out = net_connect(
                &json!({"transport": "telnet", "host": "127.0.0.1", "port": port,
                        "user": "joao", "password": "senha-teste",
                        "vendor": "mikrotik", "remember": false}),
                ctx,
                exec,
            )
            .unwrap();
            let id = out.split_whitespace().next().unwrap().to_string();
            assert_eq!(*seen.lock().unwrap(), "joao+cet511w4098h", "{out}");
            assert!(!out.contains("senha-teste"), "{out}");
            let cmd =
                "/ip firewall address-list print without-paging terse where address=45.232.65.250";
            let res = terminal_batch(&json!({"id": id, "commands": [cmd]}), ctx, exec).unwrap();
            let _ = terminal::close_terminal(&id);
            assert!(res.contains(&format!("ok: {cmd}")), "{res}");
            assert!(!res.contains("Masterlink] > /ip"), "{res}");
        });
    }

    /// The batch measured on a CCR1036 (RouterOS 7.24.2, ssh without console
    /// options): prompt redrawn inside the echo, echo cut at the terminal
    /// width, and a conntrack count that answers after 8 s with a 5 s
    /// deadline. Every section keeps its own answer.
    #[test]
    fn a_routeros_batch_with_a_slow_command_keeps_each_output_in_place() {
        let port = fake_routeros_cli(None);
        with_env(|ctx, exec| {
            let target = format!("telnet://127.0.0.1:{port}");
            let (id, _, _) = terminal::open_with_meta(
                OpenSpec {
                    command: &target,
                    cwd: ctx.workspace,
                    expect: None,
                    timeout_dur: Duration::from_secs(5),
                    auto_page: true,
                    secrets: Vec::new(),
                    vendor: Some("mikrotik_router_os".into()),
                    device: None,
                    host: None,
                    role: None,
                    client: None,
                    vpn: None,
                    transport: Some("telnet".into()),
                },
                exec,
            )
            .unwrap();
            let slow =
                r#"/ip firewall connection print count-only where dst-address~"179.108.156.6""#;
            let cmds = [
                "/system identity print without-paging",
                "/ip route print without-paging terse where dst-address=45.232.65.248/30",
                slow,
                "/ip firewall address-list print without-paging terse where address=45.232.65.250",
                "/ip firewall nat print without-paging terse where src-address=45.232.65.250",
                "/ip firewall mangle print terse without-paging",
                "/ip address print without-paging terse",
                "/interface print without-paging terse where running",
                "/ip firewall filter print without-paging terse where chain=forward",
                "/system resource print without-paging",
            ];
            let items: Vec<Value> = cmds
                .iter()
                .map(|c| {
                    if *c == slow {
                        json!({"cmd": c, "timeout_secs": 5})
                    } else {
                        json!(c)
                    }
                })
                .collect();
            let started = std::time::Instant::now();
            let out = terminal_batch(&json!({"id": id, "commands": items}), ctx, exec).unwrap();
            let took = started.elapsed();
            let _ = terminal::close_terminal(&id);
            eprintln!("lote de 10 com um lento (8 s, prazo 5 s): {took:?}\n{out}");
            let sections: Vec<&str> = out.split("### ").skip(1).collect();
            assert_eq!(sections.len(), 10, "{out}");
            for (sec, cmd) in sections.iter().zip(cmds) {
                let (head, body) = sec.split_once('\n').unwrap();
                assert_eq!(head, cmd, "{out}");
                if cmd == slow {
                    assert_eq!(body.lines().next(), Some("1"), "{sec}");
                } else {
                    assert_eq!(body.lines().next(), Some(&*format!("ok: {cmd}")), "{sec}");
                }
                assert!(!body.contains("Masterlink]"), "{sec}");
                assert!(!body.contains("__ZB_END_"), "{sec}");
            }
            assert!(took < Duration::from_secs(20), "{took:?}");
        });
    }

    #[test]
    fn a_dropped_device_session_reopens_under_the_same_id() {
        use std::io::{Read, Write};
        // Answers one command per connection, then hangs up (ssh timeout).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for n in 1..=2 {
                let Ok((mut s, _)) = l.accept() else { return };
                let _ = s.write_all(b"[admin@MK] > ");
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while let Ok(1) = s.read(&mut b) {
                    if b[0] == b'\r' && !line.is_empty() {
                        let cmd = String::from_utf8_lossy(&line).to_string();
                        let _ = s.write_all(
                            format!("{cmd}\r\nconexão {n}: {cmd}\r\n[admin@MK] > ").as_bytes(),
                        );
                        std::thread::sleep(Duration::from_millis(100));
                        break;
                    }
                    if b[0] != b'\r' && b[0] != b'\n' {
                        line.push(b[0]);
                    }
                }
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
            deadline: std::time::Instant::now() + Duration::from_secs(30),
            timeout: Duration::from_secs(30),
            host: &host,
        };
        let out = net_connect(
            &json!({"transport": "telnet", "host": "127.0.0.1", "port": port,
                    "vendor": "mikrotik", "remember": false}),
            &ctx,
            &exec,
        )
        .unwrap();
        let id = out.split_whitespace().next().unwrap().to_string();
        let first = terminal_batch(
            &json!({"id": id, "commands": ["/system identity print"]}),
            &ctx,
            &exec,
        )
        .unwrap();
        assert!(first.contains("conexão 1:"), "{first}");
        // The device hung up; the next call reopens it, same id.
        std::thread::sleep(Duration::from_millis(400));
        let again = terminal_batch(
            &json!({"id": id, "commands": ["/ip address print"]}),
            &ctx,
            &exec,
        )
        .unwrap();
        assert!(
            again.starts_with(&format!(
                "[terminal {id} reaberto: a sessão anterior tinha caído]"
            )),
            "{again}"
        );
        assert!(again.contains("conexão 2:"), "{again}");
        // Closed on purpose: never reopened, the error lists what is open.
        terminal::close_terminal(&id).unwrap();
        let err = terminal_batch(
            &json!({"id": id, "commands": ["/ip address print"]}),
            &ctx,
            &exec,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("chame net_connect de novo"), "{err}");
        assert!(err.contains("abertos:"), "{err}");
    }

    #[test]
    fn batch_stops_routeros_monitor_and_slow_command_then_moves_on() {
        with_fake_router("[admin@MK] > ", "mikrotik_router_os", |id, ctx, exec| {
            let started = std::time::Instant::now();
            let out = terminal_batch(
                &json!({"id": id, "commands": [
                    "/ip route check 179.108.156.6",
                    {"cmd": "/slow", "timeout_secs": 2},
                    "/system identity print"
                ]}),
                ctx,
                exec,
            )
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(15), "{out}");
            let check = out.split("### /slow").next().unwrap();
            assert_eq!(check.matches("status: ok").count(), 1, "{out}");
            assert!(!check.contains("Q quit"), "{out}");
            assert!(
                check.contains("[saída contínua interrompida pelo Zerith após"),
                "{out}"
            );
            assert!(out.contains("[tempo esgotado após 2s"), "{out}");
            assert!(out.contains("aguardando 1"), "{out}");
            assert!(out.contains("ok: /system identity print"), "{out}");
        });
    }

    #[test]
    fn continuous_commands_get_a_stop_parameter_or_are_refused() {
        let mk = Vendor::MikrotikRouterOs;
        let p = |v, c| prepare_network_command(v, c);
        assert_eq!(
            p(mk, "/ping 8.8.8.8").unwrap(),
            (
                "/ping 8.8.8.8 count=4".into(),
                Some("[ajustado: count=4 acrescentado]".into())
            )
        );
        assert_eq!(p(mk, "/ping 8.8.8.8 count=2").unwrap().1, None);
        assert_eq!(
            p(mk, "/tool torch ether1").unwrap().0,
            "/tool torch ether1 duration=5"
        );
        assert_eq!(
            p(mk, "/interface monitor-traffic ether1").unwrap().0,
            "/interface monitor-traffic ether1 once"
        );
        assert_eq!(
            p(mk, "/interface/ethernet/monitor sfp1").unwrap().0,
            "/interface/ethernet/monitor sfp1 once"
        );
        assert_eq!(
            p(mk, "/tool sniffer quick interface=ether1").unwrap().0,
            "/tool sniffer quick interface=ether1 duration=5"
        );
        let (c, n) = p(mk, "/ip route check 179.108.156.6").unwrap();
        assert_eq!(c, "/ip route check 179.108.156.6");
        assert!(n.unwrap().contains("contínuo"));
        assert!(p(mk, "/log print follow").is_err());
        assert!(p(mk, "/ip firewall connection print follow-only").is_err());
        assert_eq!(
            p(mk, "/ip route print").unwrap(),
            ("/ip route print without-paging".into(), None)
        );
        assert_eq!(
            p(mk, "/ip firewall filter print terse where chain=input")
                .unwrap()
                .0,
            "/ip firewall filter print without-paging terse where chain=input"
        );
        for kept in [
            "/ip route print count-only",
            "/ip route print without-paging",
            ":put [/ip route print as-value]",
            "/ip/route/print",
            "/system identity printer",
        ] {
            assert_eq!(p(mk, kept).unwrap().0, kept);
        }
        assert_eq!(
            p(Vendor::Linux, "ping 10.0.0.1").unwrap().0,
            "ping -c 4 10.0.0.1"
        );
        assert!(p(Vendor::Linux, "tail -f /var/log/syslog").is_err());
        assert_eq!(p(Vendor::CiscoIos, "ping 10.0.0.1").unwrap().1, None);
    }

    #[test]
    fn batch_adjusts_ping_and_refuses_print_follow() {
        with_fake_router("[admin@MK] > ", "mikrotik_router_os", |id, ctx, exec| {
            let out = terminal_batch(
                &json!({"id": id, "commands": ["/log print follow", "/ping 1.1.1.1"]}),
                ctx,
                exec,
            )
            .unwrap();
            assert!(
                out.contains("[não enviado pelo Zerith: `print follow`"),
                "{out}"
            );
            assert!(
                out.contains("### /ping 1.1.1.1 count=4\n[ajustado: count=4 acrescentado]"),
                "{out}"
            );
            assert!(out.contains("ok: /ping 1.1.1.1 count=4"), "{out}");
        });
    }

    #[test]
    fn batch_pages_cisco_more_and_turns_paging_off() {
        with_fake_router("R1#", "cisco_ios", |id, ctx, exec| {
            let out = terminal_batch(
                &json!({"id": id, "commands": ["show run", "show clock"]}),
                ctx,
                exec,
            )
            .unwrap();
            assert!(out.contains("linha 2b") && out.contains("end"), "{out}");
            assert!(out.contains("[paginação automática: 3 páginas]"), "{out}");
            assert!(
                out.contains("[paginação desativada pelo Zerith: terminal length 0]"),
                "{out}"
            );
            assert!(out.contains("ok: show clock"), "{out}");
        });
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

    #[test]
    fn multi_client_inventory_disambiguates_same_device_name_and_auto_onboards_with_vpn() {
        let dir = tempfile::tempdir().unwrap();
        let inv_path = dir.path().join("inventory.toml");

        let dev_a = DeviceEntry {
            name: "core-01".into(),
            client: Some("TelecomSul".into()),
            site: Some("DC-Curitiba".into()),
            vpn: Some("wg-telecomsul".into()),
            host: "10.0.0.1".into(),
            user: "admin".into(),
            vendor: Vendor::HuaweiVrp,
            role: DeviceRole::Core,
            ..Default::default()
        };
        let dev_b = DeviceEntry {
            name: "core-01".into(),
            client: Some("HospitalVida".into()),
            site: Some("Matriz-SP".into()),
            vpn: Some("wg-hospital".into()),
            host: "10.0.0.1".into(),
            user: "admin".into(),
            vendor: Vendor::MikrotikRouterOs,
            role: DeviceRole::Core,
            ..Default::default()
        };
        upsert_inventory_device(&inv_path, dev_a).unwrap();
        upsert_inventory_device(&inv_path, dev_b).unwrap();

        let loaded = load_inventory(dir.path(), Some(dir.path())).unwrap();
        assert_eq!(loaded.len(), 2);

        // Requesting `core-01` without client must refuse to guess and list both clients
        let err = find_inventory_device(&loaded, "core-01", None).unwrap_err();
        assert!(err.contains("TelecomSul/core-01"), "{err}");
        assert!(err.contains("HospitalVida/core-01"), "{err}");

        // Requesting with explicit client or qualified `Client/device` resolves unambiguously
        let hit_a = find_inventory_device(&loaded, "core-01", Some("TelecomSul")).unwrap();
        assert_eq!(hit_a.vendor, Vendor::HuaweiVrp);
        assert_eq!(hit_a.vpn.as_deref(), Some("wg-telecomsul"));

        let hit_b = find_inventory_device(&loaded, "HospitalVida/core-01", None).unwrap();
        assert_eq!(hit_b.vendor, Vendor::MikrotikRouterOs);
        assert_eq!(hit_b.vpn.as_deref(), Some("wg-hospital"));

        // WireGuard .conf parser & profile saver
        let wg_conf = r#"
[Interface]
PrivateKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
Address = 10.250.0.2/32

[Peer]
PublicKey = BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=
AllowedIPs = 10.0.0.0/24, 172.16.50.0/24
Endpoint = vpn.telecomsul.com.br:51820
"#;
        let prof =
            save_wireguard_profile(dir.path(), "wg-telecomsul", wg_conf, dir.path()).unwrap();
        assert_eq!(prof.name, "wg-telecomsul");
        assert_eq!(prof.address.as_deref(), Some("10.250.0.2/32"));
        assert_eq!(prof.allowed_ips, vec!["10.0.0.0/24", "172.16.50.0/24"]);
        assert_eq!(
            prof.endpoint.as_deref(),
            Some("vpn.telecomsul.com.br:51820")
        );

        assert_eq!(
            derive_secret_ref_name(Some("Telecom Sul"), "core-01"),
            "net_telecom_sul_core_01"
        );
        assert!(remove_inventory_device(&inv_path, Some("TelecomSul"), "core-01").unwrap());
        let after_rem = load_inventory(dir.path(), Some(dir.path())).unwrap();
        assert_eq!(after_rem.len(), 1);
        assert_eq!(after_rem[0].client.as_deref(), Some("HospitalVida"));
    }

    #[test]
    fn subnet_expansion_and_host_fingerprinting_for_proxmox_mikrotik_and_huawei() {
        let cidr = expand_subnet_or_range("10.0.0.0/30", 16);
        assert_eq!(cidr, vec!["10.0.0.1", "10.0.0.2"]);
        let range = expand_subnet_or_range("192.168.10.5-7", 16);
        assert_eq!(range, vec!["192.168.10.5", "192.168.10.6", "192.168.10.7"]);

        // Proxmox VE host (ports 22 + 8006)
        let pve_rows = vec![
            ProbeRow {
                host: "10.0.0.10".into(),
                resolved_ip: "10.0.0.10".into(),
                port: 22,
                service: "SSH",
                open: true,
                rtt_ms: Some(1.2),
                detail: "SSH-2.0-OpenSSH_9.2p1 Debian".into(),
            },
            ProbeRow {
                host: "10.0.0.10".into(),
                resolved_ip: "10.0.0.10".into(),
                port: 8006,
                service: "Proxmox-VE",
                open: true,
                rtt_ms: Some(1.5),
                detail: "conectado".into(),
            },
        ];
        let (vendor, role, transport, port, tags) = fingerprint_probed_host(&pve_rows).unwrap();
        assert_eq!(vendor, Vendor::Linux);
        assert_eq!(role, DeviceRole::Server);
        assert_eq!(transport, DeviceTransport::Ssh);
        assert_eq!(port, 22);
        assert!(tags.contains(&"proxmox".to_string()));

        // MikroTik RouterOS (Winbox 8291)
        let mk_rows = vec![ProbeRow {
            host: "10.0.0.1".into(),
            resolved_ip: "10.0.0.1".into(),
            port: 8291,
            service: "Winbox",
            open: true,
            rtt_ms: Some(0.9),
            detail: "conectado".into(),
        }];
        let (mk_vendor, mk_role, _, _, _) = fingerprint_probed_host(&mk_rows).unwrap();
        assert_eq!(mk_vendor, Vendor::MikrotikRouterOs);
        assert_eq!(mk_role, DeviceRole::Border);
    }

    #[test]
    fn scp_transfer_builder_and_rtt_jitter_stats_work() {
        let dev = DeviceEntry {
            name: "pve-01".into(),
            client: Some("TelecomSul".into()),
            host: "10.50.0.10".into(),
            port: 2222,
            user: "root".into(),
            jump_host: Some("bastion@203.0.113.5".into()),
            legacy_ssh: true,
            ..Default::default()
        };
        let local = Path::new("/tmp/dump.pcap");
        let up = build_scp_args(&dev, "upload", local, "/var/tmp/dump.pcap").unwrap();
        assert!(up.contains(&"-P".to_string()));
        assert!(up.contains(&"2222".to_string()));
        assert!(up.contains(&"-J".to_string()));
        assert!(up.contains(&"bastion@203.0.113.5".to_string()));
        assert_eq!(up.last().unwrap(), "root@10.50.0.10:/var/tmp/dump.pcap");

        let down = build_scp_args(&dev, "download", local, "/etc/pve/storage.cfg").unwrap();
        assert_eq!(down[down.len() - 2], "root@10.50.0.10:/etc/pve/storage.cfg");
        assert_eq!(down.last().unwrap(), "/tmp/dump.pcap");
        assert!(build_scp_args(&dev, "invalid", local, "/tmp/x").is_err());

        let stats = compute_rtt_jitter_stats(4, &[10.0, 14.0, 12.0]);
        assert_eq!(stats.sent, 4);
        assert_eq!(stats.received, 3);
        assert!((stats.loss_pct - 25.0).abs() < 0.01);
        assert!((stats.min_ms - 10.0).abs() < 0.01);
        assert!((stats.max_ms - 14.0).abs() < 0.01);
        assert!((stats.avg_ms - 12.0).abs() < 0.01);
        assert!((stats.jitter_ms - 3.0).abs() < 0.01);
    }

    #[test]
    fn net_diff_and_net_transfer_enforce_read_and_write_policy() {
        let scratch =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let ws = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let outside = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let outside_cfg = outside.path().join("outside.cfg");
        std::fs::write(&outside_cfg, "hostname OUTSIDE\n").unwrap();
        std::fs::write(ws.path().join("a.cfg"), "hostname A\n").unwrap();
        std::fs::write(ws.path().join("b.cfg"), "hostname B\n").unwrap();
        std::fs::write(ws.path().join(".env"), "SECRET=123\n").unwrap();

        let mut policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            ws.path(),
        );
        policy.rules =
            vec![
                zerith_security::Rule::parse(zerith_security::RuleKind::Deny, "read_file(*.env)")
                    .unwrap(),
            ];

        let ctx = CallCtx {
            workspace: ws.path(),
            policy: &policy,
            memory: None,
            scope: None,
        };
        let cancel = zerith_providers::Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: std::time::Instant::now() + Duration::from_secs(5),
            timeout: Duration::from_secs(5),
            host: &host,
        };

        // In-workspace files and inline configs work.
        let ok = net_diff(&json!({"before": "a.cfg", "after": "b.cfg"}), &ctx, &exec).unwrap();
        assert!(ok.contains("hostname B"), "{ok}");
        let inline = net_diff(
            &json!({"before": "hostname A\ninterface Gi0/1\n ip address 1.1.1.1 255.255.255.0", "after": "hostname B\ninterface Gi0/1\n ip address 1.1.1.1 255.255.255.0"}),
            &ctx,
            &exec,
        )
        .unwrap();
        assert!(inline.contains("hostname B"), "{inline}");

        // Outside-workspace and denied `.env` files are rejected before reading.
        assert!(matches!(
            net_diff(
                &json!({"before": outside_cfg.to_string_lossy(), "after": "b.cfg"}),
                &ctx,
                &exec
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            net_diff(&json!({"before": ".env", "after": "b.cfg"}), &ctx, &exec),
            Err(ToolError::Denied(_))
        ));

        // net_transfer enforces write_file on download and read_file on upload.
        assert!(matches!(
            net_transfer(
                &json!({"direction": "download", "host": "10.0.0.1", "local_path": "out.bin", "remote_path": "/tmp/a"}),
                &ctx,
                &exec
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            net_transfer(
                &json!({"direction": "upload", "host": "10.0.0.1", "local_path": outside_cfg.to_string_lossy(), "remote_path": "/tmp/a"}),
                &ctx,
                &exec
            ),
            Err(ToolError::Denied(_))
        ));
        assert!(matches!(
            net_transfer(
                &json!({"direction": "upload", "host": "10.0.0.1", "local_path": ".env", "remote_path": "/tmp/a"}),
                &ctx,
                &exec
            ),
            Err(ToolError::Denied(_))
        ));
    }
}
