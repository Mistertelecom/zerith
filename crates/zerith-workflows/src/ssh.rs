//! Nó `ssh_command`: destino (campos do nó + equipamento do inventário),
//! linha de comando do `ssh` e senha por `SSH_ASKPASS`.
//!
//! Senha: as ferramentas de rede (`zerith_tools::network::net_connect`) abrem
//! um terminal (ConPTY/`script`) e digitam a senha no prompt; o motor não pode
//! depender de `zerith-tools` (que depende dele) e roda sem terminal, então
//! usa o mecanismo padrão do OpenSSH para isso: um askpass que só ecoa uma
//! variável de ambiente do processo filho. Nada de `sshpass` nem crate nova;
//! a senha não passa pela linha de comando nem por arquivo.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::{Result, WorkflowError};

/// Destino já resolvido.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SshTarget {
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_file: Option<String>,
    /// Nome do segredo com a senha; só vale sem `identity_file`.
    pub password_secret: Option<String>,
    pub legacy: bool,
}

/// Campos do `<raiz>/inventory.toml` que o nó usa (o formato completo é o
/// `DeviceEntry` de `zerith_tools::network`; aqui só leitura).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct InventoryDevice {
    name: String,
    client: Option<String>,
    host: String,
    port: Option<u16>,
    user: String,
    secret_ref: Option<String>,
    legacy_ssh: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct InventoryFile {
    devices: Vec<InventoryDevice>,
}

fn cfg_str<'a>(cfg: &'a Value, key: &str) -> Option<&'a str> {
    cfg.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Equipamento `cliente/nome` (ou só `nome`) de `<raiz>/inventory.toml`.
fn inventory_device(data_root: &Path, key: &str) -> Result<InventoryDevice> {
    let path = data_root.join("inventory.toml");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        WorkflowError::Exec(format!("device '{key}': sem {} ({e})", path.display()))
    })?;
    let inv: InventoryFile = toml::from_str(&raw)
        .map_err(|e| WorkflowError::Exec(format!("inventory.toml inválido: {e}")))?;
    let (client, name) = match key.rsplit_once('/') {
        Some((c, n)) => (Some(c.trim()), n.trim()),
        None => (None, key.trim()),
    };
    inv.devices
        .into_iter()
        .find(|d| {
            d.name.trim().eq_ignore_ascii_case(name)
                && client.is_none_or(|c| {
                    d.client
                        .as_deref()
                        .is_some_and(|dc| dc.trim().eq_ignore_ascii_case(c))
                })
        })
        .ok_or_else(|| WorkflowError::Exec(format!("device '{key}' não está no inventário")))
}

/// Valor que entra na linha do `ssh`: sem `-` no início, espaço ou controle.
fn ssh_token(what: &str, v: &str) -> Result<String> {
    let v = v.trim();
    if v.is_empty() || v.starts_with('-') || v.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(WorkflowError::Exec(format!("{what} SSH inválido")));
    }
    Ok(v.to_string())
}

/// Junta `device` (inventário) com os campos do nó; o que o nó define vence.
pub(crate) fn target(cfg: &Value, data_root: &Path) -> Result<SshTarget> {
    let dev = cfg_str(cfg, "device")
        .map(|key| inventory_device(data_root, key))
        .transpose()?;
    let host = cfg_str(cfg, "host")
        .map(str::to_string)
        .or_else(|| dev.as_ref().map(|d| d.host.clone()))
        .filter(|h| !h.trim().is_empty())
        .ok_or_else(|| WorkflowError::Exec("host SSH obrigatório".into()))?;
    let user = cfg_str(cfg, "user")
        .map(str::to_string)
        .or_else(|| dev.as_ref().map(|d| d.user.clone()))
        .filter(|u| !u.trim().is_empty());
    let port = cfg
        .get("port")
        .and_then(Value::as_u64)
        .or_else(|| dev.as_ref().and_then(|d| d.port).map(u64::from))
        .unwrap_or(22)
        .clamp(1, 65535) as u16;
    let identity_file = cfg_str(cfg, "identity_file").map(str::to_string);
    let password_secret = match identity_file {
        Some(_) => None,
        None => cfg_str(cfg, "password_secret")
            .map(str::to_string)
            .or_else(|| dev.as_ref().and_then(|d| d.secret_ref.clone()))
            .filter(|s| !s.trim().is_empty()),
    };
    let legacy = cfg
        .get("legacy_ssh")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| dev.as_ref().is_some_and(|d| d.legacy_ssh));
    Ok(SshTarget {
        host: ssh_token("host", &host)?,
        user: user.map(|u| ssh_token("usuário", &u)).transpose()?,
        port,
        identity_file,
        password_secret,
        legacy,
    })
}

/// Argumentos do `ssh` (sem o binário). Com senha: só password/keyboard-
/// interactive, uma tentativa; sem senha: `BatchMode`. `legacy`: os mesmos
/// algoritmos antigos de `zerith_tools::network::build_ssh_command`.
pub(crate) fn args(t: &SshTarget, remote_cmd: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-p".into(),
        t.port.to_string(),
    ];
    let opts: &[&str] = if t.password_secret.is_some() {
        &[
            "PreferredAuthentications=password,keyboard-interactive",
            "PubkeyAuthentication=no",
            "NumberOfPasswordPrompts=1",
        ]
    } else {
        &["BatchMode=yes"]
    };
    for o in opts {
        a.extend(["-o".into(), (*o).into()]);
    }
    if let Some(k) = &t.identity_file {
        a.extend(["-i".into(), k.clone()]);
    }
    if t.legacy {
        for o in [
            "KexAlgorithms=+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1",
            "HostKeyAlgorithms=+ssh-rsa",
            "PubkeyAcceptedAlgorithms=+ssh-rsa",
        ] {
            a.extend(["-o".into(), o.into()]);
        }
    }
    a.push(match &t.user {
        Some(u) => format!("{u}@{}", t.host),
        None => t.host.clone(),
    });
    a.push(remote_cmd.to_string());
    a
}

/// Variável que leva a senha ao askpass (só no ambiente do `ssh` filho).
pub(crate) const ASKPASS_ENV: &str = "ZERITH_SSH_PASSWORD";

/// Script de askpass temporário que só imprime [`ASKPASS_ENV`]. Apagado no
/// `Drop`.
/// ponytail: `SSH_ASKPASS_REQUIRE=force` é do OpenSSH >= 8.4 (Debian 11+,
/// Ubuntu 22.04+); mais antigo cai no `DISPLAY` sem terminal. No Windows vai um
/// `.cmd` — validado só no Linux, onde o daemon roda.
pub(crate) struct AskPass(pub PathBuf);

impl AskPass {
    pub(crate) fn create() -> Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let (ext, body) = if cfg!(windows) {
            (
                "cmd",
                format!("@echo off\r\nsetlocal EnableDelayedExpansion\r\necho(!{ASKPASS_ENV}!\r\n"),
            )
        } else {
            (
                "sh",
                format!("#!/bin/sh\nprintf '%s\\n' \"${ASKPASS_ENV}\"\n"),
            )
        };
        let path =
            std::env::temp_dir().join(format!("zerith-askpass-{}-{n}.{ext}", std::process::id()));
        std::fs::write(&path, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(path))
    }

    /// Ambiente do `ssh` filho: askpass forçado e a senha.
    pub(crate) fn env<'a>(&'a self, password: &'a str) -> Vec<(&'a str, String)> {
        let mut env = vec![
            ("SSH_ASKPASS", self.0.to_string_lossy().into_owned()),
            ("SSH_ASKPASS_REQUIRE", "force".to_string()),
            (ASKPASS_ENV, password.to_string()),
        ];
        if std::env::var_os("DISPLAY").is_none() {
            env.push(("DISPLAY", ":0".to_string()));
        }
        env
    }
}

impl Drop for AskPass {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn linha_do_ssh_com_legacy_porta_e_senha() {
        let dir = tempfile::tempdir().unwrap();
        let t = target(
            &json!({"host": "198.18.0.1", "user": "admin", "port": 2222,
                    "legacy_ssh": true, "password_secret": "dude-senha"}),
            dir.path(),
        )
        .unwrap();
        let a = args(&t, "/system identity print");
        let line = a.join(" ");
        assert!(line.contains("-p 2222"), "{line}");
        assert!(line.contains("HostKeyAlgorithms=+ssh-rsa"));
        assert!(line.contains("PubkeyAcceptedAlgorithms=+ssh-rsa"));
        assert!(line.contains("KexAlgorithms=+diffie-hellman-group14-sha1"));
        assert!(line.contains("PreferredAuthentications=password"));
        assert!(!line.contains("BatchMode"), "senha não usa BatchMode");
        assert_eq!(a[a.len() - 2], "admin@198.18.0.1");
        assert_eq!(a[a.len() - 1], "/system identity print");
        assert!(!line.contains("dude-senha"), "só o nome, nunca a senha");

        // Chave vence a senha; sem legacy não há algoritmos antigos.
        let t = target(
            &json!({"host": "h", "identity_file": "/k", "password_secret": "x"}),
            dir.path(),
        )
        .unwrap();
        let line = args(&t, "uptime").join(" ");
        assert!(line.contains("BatchMode=yes") && line.contains("-i /k"));
        assert!(line.contains("-p 22") && !line.contains("ssh-rsa"));
        assert_eq!(t.password_secret, None);

        assert!(target(&json!({"host": "-oProxyCommand=x"}), dir.path()).is_err());
        assert!(target(&json!({"host": "h", "user": "a b"}), dir.path()).is_err());
    }

    #[test]
    fn device_do_inventario_e_campos_do_no_vencem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("inventory.toml"),
            r#"
[[devices]]
name = "dude"
client = "Exemplo"
host = "198.18.10.1"
port = 2200
user = "noc"
secret_ref = "net_exemplo_dude"
legacy_ssh = true

[[devices]]
name = "dude"
client = "Outro"
host = "198.18.20.1"
user = "x"
"#,
        )
        .unwrap();
        let t = target(&json!({"device": "exemplo/DUDE"}), dir.path()).unwrap();
        assert_eq!(
            t,
            SshTarget {
                host: "198.18.10.1".into(),
                user: Some("noc".into()),
                port: 2200,
                identity_file: None,
                password_secret: Some("net_exemplo_dude".into()),
                legacy: true,
            }
        );
        let t = target(
            &json!({"device": "Outro/dude", "port": 22, "legacy_ssh": false}),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.legacy),
            ("198.18.20.1", 22, false)
        );
        assert!(target(&json!({"device": "nada/x"}), dir.path()).is_err());
    }
}
