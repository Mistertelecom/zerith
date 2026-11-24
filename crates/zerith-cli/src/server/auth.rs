//! Segurança do servidor: endereço de escuta, configuração, tokens por
//! dispositivo (só o hash sha256 vai para o disco) e limite de falhas de
//! autenticação por IP.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Boxed = Box<dyn std::error::Error>;

/// Prefixo dos tokens: facilita achar um token vazado em log/commit.
const TOKEN_PREFIX: &str = "zt_";

// ---------------------------------------------------------------- bind

/// Classe do endereço de escuta.
#[derive(Debug, PartialEq, Eq)]
pub enum BindKind {
    Loopback,
    /// 10/8, 172.16/12, 192.168/16, 100.64/10 (Tailscale/CGNAT), fd00::/8.
    Private,
    /// 0.0.0.0, `::` ou endereço roteável na internet.
    Public,
}

pub fn classify(ip: IpAddr) -> BindKind {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if v4.is_loopback() {
                BindKind::Loopback
            } else if o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
            {
                BindKind::Private
            } else {
                BindKind::Public
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                BindKind::Loopback
            } else if let Some(v4) = v6.to_ipv4_mapped() {
                classify(IpAddr::V4(v4))
            } else if v6.octets()[0] == 0xfd {
                BindKind::Private
            } else {
                BindKind::Public
            }
        }
    }
}

/// Valida o endereço: público ou "todas as interfaces" só com `allow_public`.
pub fn check_bind(addr: &str, allow_public: bool) -> Result<SocketAddr, Boxed> {
    let sa: SocketAddr = addr
        .parse()
        .map_err(|_| format!("endereço inválido '{addr}' (use ip:porta, ex.: 127.0.0.1:7878)"))?;
    if classify(sa.ip()) == BindKind::Public && !allow_public {
        return Err(format!(
            "recusado: {sa} é público ou todas as interfaces. O servidor não tem TLS próprio; \
             use um IP privado/Tailscale/WireGuard ou um proxy HTTPS (veja docs/servidor.md). \
             Para forçar, passe --allow-public."
        )
        .into());
    }
    Ok(sa)
}

pub const PUBLIC_WARNING: &str = "AVISO: o servidor está ouvindo num endereço PÚBLICO sem TLS. \
O token Bearer e os dados dos workflows trafegam em texto puro e podem ser capturados. \
Coloque um proxy HTTPS (Caddy) ou uma VPN (WireGuard/Tailscale) na frente, ou volte para 127.0.0.1.";

// ---------------------------------------------------------------- config

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Nome que aparece no desktop ("servidor X").
    pub name: String,
    pub bind: String,
    /// URL que o desktop usa para chegar aqui (vai no código de pareamento).
    pub url: String,
    #[serde(default)]
    pub allow_public: bool,
}

pub fn server_dir(root: &Path) -> PathBuf {
    root.join("server")
}

pub fn config_path(root: &Path) -> PathBuf {
    server_dir(root).join("config.toml")
}

pub fn load_config(root: &Path) -> Result<ServerConfig, Boxed> {
    let p = config_path(root);
    let text = std::fs::read_to_string(&p)
        .map_err(|_| format!("{} não existe; rode `zerith server init`", p.display()))?;
    Ok(toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?)
}

pub fn save_config(root: &Path, cfg: &ServerConfig) -> Result<(), Boxed> {
    write_private(&config_path(root), toml::to_string(cfg)?.as_bytes())?;
    Ok(())
}

/// Grava via arquivo temporário + rename; no Unix com modo 0600.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------- tokens

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenEntry {
    pub device: String,
    /// sha256 do token em hex. O token em si nunca é gravado.
    pub sha256: String,
    pub created_at_ms: i64,
}

pub fn tokens_path(root: &Path) -> PathBuf {
    server_dir(root).join("tokens.json")
}

/// Último uso fica num arquivo separado escrito só pelo servidor: assim o
/// `tokens.json` só é escrito pela CLI e um `revoke` nunca é desfeito por
/// uma gravação concorrente do servidor.
pub fn usage_path(root: &Path) -> PathBuf {
    server_dir(root).join("token_usage.json")
}

pub fn load_tokens(root: &Path) -> Vec<TokenEntry> {
    std::fs::read(tokens_path(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_tokens(root: &Path, list: &[TokenEntry]) -> Result<(), Boxed> {
    write_private(&tokens_path(root), &serde_json::to_vec_pretty(list)?)?;
    Ok(())
}

/// `device` → último uso em ms.
pub fn load_usage(root: &Path) -> HashMap<String, i64> {
    std::fs::read(usage_path(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 32 bytes do gerador do sistema operacional.
pub fn new_token() -> Result<String, Boxed> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("gerador aleatório do sistema: {e}"))?;
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{TOKEN_PREFIX}{hex}"))
}

pub fn valid_device(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Cria token para `device` e devolve o token em texto (só esta vez).
pub fn add_token(root: &Path, device: &str) -> Result<String, Boxed> {
    if !valid_device(device) {
        return Err(
            "nome do dispositivo: 1-64 caracteres entre letras, números, '-', '_' e '.'".into(),
        );
    }
    let mut list = load_tokens(root);
    if list.iter().any(|t| t.device == device) {
        return Err(format!(
            "já existe token para '{device}'; revogue antes (`zerith server token revoke {device}`)"
        )
        .into());
    }
    let token = new_token()?;
    list.push(TokenEntry {
        device: device.to_string(),
        sha256: hash_token(&token),
        created_at_ms: zerith_workflows::now_ms(),
    });
    save_tokens(root, &list)?;
    Ok(token)
}

pub fn revoke_token(root: &Path, device: &str) -> Result<(), Boxed> {
    let mut list = load_tokens(root);
    let before = list.len();
    list.retain(|t| t.device != device);
    if list.len() == before {
        return Err(format!("nenhum token para '{device}'").into());
    }
    save_tokens(root, &list)
}

/// Comparação em tempo constante (o tamanho é fixo: hashes hex de 64).
/// Mesma ideia de `ct_eq` em `zerith-desktop/src/workflows.rs`.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Dispositivo dono do token, comparando com TODOS os hashes sem sair cedo.
/// Relê `tokens.json` a cada chamada: revogação vale na hora, sem reiniciar.
pub fn authenticate(root: &Path, token: &str) -> Option<String> {
    let h = hash_token(token);
    let mut found = None;
    for t in load_tokens(root) {
        if ct_eq(&t.sha256, &h) && found.is_none() {
            found = Some(t.device);
        }
    }
    found
}

/// Grava o último uso no máximo uma vez por minuto por dispositivo.
pub fn touch_usage(root: &Path, device: &str, lock: &Mutex<()>) {
    let Ok(_g) = lock.lock() else { return };
    let mut usage = load_usage(root);
    let now = zerith_workflows::now_ms();
    if usage.get(device).is_some_and(|&t| now - t < 60_000) {
        return;
    }
    usage.insert(device.to_string(), now);
    let _ = write_private(
        &usage_path(root),
        &serde_json::to_vec(&usage).unwrap_or_default(),
    );
}

// ---------------------------------------------------------------- rate limit

pub const MAX_AUTH_FAILURES: usize = 10;
const WINDOW: Duration = Duration::from_secs(60);

/// Falhas de autenticação por IP numa janela deslizante de 1 minuto.
#[derive(Default)]
pub struct FailLimiter {
    by_ip: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl FailLimiter {
    /// `true` = IP bloqueado agora (já errou o máximo no último minuto).
    pub fn blocked(&self, ip: IpAddr) -> bool {
        let Ok(mut m) = self.by_ip.lock() else {
            return true;
        };
        let now = Instant::now();
        // Limpa IPs antigos para o mapa não crescer sem fim.
        m.retain(|_, v| {
            v.retain(|t| now.duration_since(*t) < WINDOW);
            !v.is_empty()
        });
        m.get(&ip).is_some_and(|v| v.len() >= MAX_AUTH_FAILURES)
    }

    pub fn fail(&self, ip: IpAddr) {
        if let Ok(mut m) = self.by_ip.lock() {
            m.entry(ip).or_default().push(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classificacao_de_enderecos() {
        for s in ["127.0.0.1", "::1"] {
            assert_eq!(classify(ip(s)), BindKind::Loopback, "{s}");
        }
        for s in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.1",
            "192.168.1.10",
            "100.64.0.1",
            "100.127.1.1",
            "fd7a:115c:a1e0::1",
        ] {
            assert_eq!(classify(ip(s)), BindKind::Private, "{s}");
        }
        for s in [
            "0.0.0.0",
            "::",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2001:db8::1",
        ] {
            assert_eq!(classify(ip(s)), BindKind::Public, "{s}");
        }
    }

    #[test]
    fn bind_publico_recusado_sem_flag() {
        assert!(check_bind("127.0.0.1:7878", false).is_ok());
        assert!(check_bind("100.101.102.103:7878", false).is_ok());
        let e = check_bind("0.0.0.0:7878", false).unwrap_err().to_string();
        assert!(e.contains("--allow-public"));
        assert!(check_bind("203.0.113.5:7878", false).is_err());
        assert!(check_bind("0.0.0.0:7878", true).is_ok());
        assert!(check_bind("localhost", false).is_err());
    }

    #[test]
    fn token_guardado_so_como_hash_e_revogavel() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tok = add_token(root, "notebook").unwrap();
        assert!(tok.starts_with("zt_") && tok.len() == 3 + 64);
        let file = std::fs::read_to_string(tokens_path(root)).unwrap();
        assert!(!file.contains(&tok) && !file.contains(&tok[3..]));
        assert!(file.contains(&hash_token(&tok)));
        assert_eq!(authenticate(root, &tok).as_deref(), Some("notebook"));
        assert_eq!(authenticate(root, "zt_errado"), None);
        assert!(add_token(root, "notebook").is_err(), "nome repetido");
        assert!(add_token(root, "../x").is_err());
        revoke_token(root, "notebook").unwrap();
        assert_eq!(authenticate(root, &tok), None);
        assert!(revoke_token(root, "notebook").is_err());
    }

    #[test]
    fn comparacao_constante() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "a"));
    }

    #[test]
    fn limite_de_falhas_por_ip() {
        let l = FailLimiter::default();
        let a = ip("10.0.0.1");
        for _ in 0..MAX_AUTH_FAILURES {
            assert!(!l.blocked(a));
            l.fail(a);
        }
        assert!(l.blocked(a));
        assert!(!l.blocked(ip("10.0.0.2")), "outro IP não é afetado");
    }
}
