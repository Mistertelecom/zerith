//! Servidores cadastrados (Ajustes > Servidores): cada um é um `zerith server`
//! pareado pelo código `zerith-pair:` (veja `docs/servidor.md`). A lista fica
//! em `<raiz>/servers.json` (id, nome, url, criado em, último contato) e o
//! token no cofre configurado (`server-token-<id>`), nunca em arquivo.
//!
//! Toda chamada de rede roda fora da thread de UI (`spawn_blocking`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;
use zerith_core::secrets::{Configured, SecretStore};
use zerith_workflows::remote::{Health, PairingCode, RemoteClient};
use zerith_workflows::{now_ms, random_token, DaemonStatus};

use crate::state::AppState;

/// Ciclo do daemon mais velho que isso deixa o servidor amarelo.
pub const DAEMON_FRESH_MS: i64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub created_at_ms: i64,
    /// Última resposta boa da API (ms); `None` = nunca respondeu desde o cadastro.
    #[serde(default)]
    pub last_contact_ms: Option<i64>,
}

/// Verde: daemon com ciclo recente. Amarelo: API responde, daemon parado ou
/// atrasado. Vermelho: API não responde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    Green,
    Yellow,
    Red,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub id: String,
    pub color: Color,
    pub version: Option<String>,
    pub health: Option<Health>,
    /// Sinal de vida do daemon, já decodificado de `health.daemon`.
    pub daemon: Option<DaemonStatus>,
    pub error: Option<String>,
    pub checked_at_ms: i64,
    pub last_contact_ms: Option<i64>,
}

pub fn token_entry(id: &str) -> String {
    format!("server-token-{id}")
}

fn file(root: &Path) -> PathBuf {
    root.join("servers.json")
}

/// Serializa leitura+gravação de `servers.json` (monitor e comandos).
static FILE_LOCK: Mutex<()> = Mutex::new(());

pub fn load(root: &Path) -> Vec<ServerEntry> {
    std::fs::read(file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(root: &Path, list: &[ServerEntry]) -> Result<(), String> {
    let path = file(root);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

fn update(root: &Path, f: impl FnOnce(&mut Vec<ServerEntry>)) -> Result<Vec<ServerEntry>, String> {
    let _g = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = load(root);
    f(&mut list);
    save(root, &list)?;
    Ok(list)
}

pub fn find(root: &Path, id: &str) -> Result<ServerEntry, String> {
    load(root)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("servidor '{id}' não está cadastrado"))
}

pub fn client(root: &Path, vault: &dyn SecretStore, id: &str) -> Result<RemoteClient, String> {
    let s = find(root, id)?;
    let token = vault
        .get(&token_entry(id))
        .map_err(|e| format!("token de '{}' fora do cofre ({e}); pareie de novo", s.name))?;
    Ok(RemoteClient::new(&s.url, &token))
}

/// Cor do status a partir do `GET /v1/health` (ou da falta dele).
pub fn status_color(health: Option<&Health>, now_ms: i64) -> Color {
    let Some(h) = health else {
        return Color::Red;
    };
    let cycle = h
        .daemon
        .as_ref()
        .and_then(|d| d.get("last_cycle_ms"))
        .and_then(|v| v.as_i64());
    match cycle {
        Some(ms) if now_ms - ms < DAEMON_FRESH_MS => Color::Green,
        _ => Color::Yellow,
    }
}

pub fn status_of(id: &str, health: Result<Health, String>, last: Option<i64>) -> ServerStatus {
    let now = now_ms();
    let (health, error) = match health {
        Ok(h) => (Some(h), None),
        Err(e) => (None, Some(e)),
    };
    ServerStatus {
        id: id.to_string(),
        color: status_color(health.as_ref(), now),
        version: health.as_ref().map(|h| h.version.clone()),
        daemon: health
            .as_ref()
            .and_then(|h| h.daemon.clone())
            .and_then(|d| serde_json::from_value(d).ok()),
        last_contact_ms: if health.is_some() { Some(now) } else { last },
        health,
        error,
        checked_at_ms: now,
    }
}

/// Decodifica o código, confirma que é um Zerith (`ping`) e que o token vale
/// (`health`) antes de gravar. Mesmo endereço já cadastrado: troca o token e
/// o nome, mantém o id (parear de novo depois de revogar).
pub fn add(root: &Path, vault: &dyn SecretStore, pair_code: &str) -> Result<ServerEntry, String> {
    let pc = PairingCode::decode(pair_code)?;
    let c = RemoteClient::from_pairing(&pc);
    let unreachable = |e: zerith_workflows::remote::RemoteError| {
        format!(
            "não alcancei {} ({e}). Confira se o servidor está ligado e se este PC está na \
             mesma VPN/rede.",
            pc.url
        )
    };
    c.ping().map_err(unreachable)?;
    c.health().map_err(unreachable)?;
    let url = pc.url.trim_end_matches('/').to_string();
    let existing = load(root).into_iter().find(|s| s.url == url);
    let entry = ServerEntry {
        id: existing
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| format!("srv-{}", &random_token()[..10])),
        name: if pc.name.trim().is_empty() {
            url.clone()
        } else {
            pc.name.trim().to_string()
        },
        url,
        created_at_ms: existing.as_ref().map_or_else(now_ms, |s| s.created_at_ms),
        last_contact_ms: Some(now_ms()),
    };
    vault
        .set(&token_entry(&entry.id), &pc.token)
        .map_err(|e| e.to_string())?;
    let e = entry.clone();
    update(root, move |l| {
        l.retain(|s| s.id != e.id);
        l.push(e);
    })?;
    Ok(entry)
}

pub fn remove(root: &Path, vault: &dyn SecretStore, id: &str) -> Result<(), String> {
    update(root, |l| l.retain(|s| s.id != id))?;
    match vault.delete(&token_entry(id)) {
        Ok(()) | Err(zerith_core::secrets::SecretError::NotFound(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    Ok(())
}

/// `ping` (sem token) e `health` (com token).
pub fn test(root: &Path, vault: &dyn SecretStore, id: &str) -> ServerStatus {
    let last = find(root, id).ok().and_then(|s| s.last_contact_ms);
    let health = client(root, vault, id).and_then(|c| {
        c.ping().map_err(|e| e.to_string())?;
        c.health().map_err(|e| e.to_string())
    });
    remember(root, status_of(id, health, last))
}

pub fn health(root: &Path, vault: &dyn SecretStore, id: &str) -> ServerStatus {
    let last = find(root, id).ok().and_then(|s| s.last_contact_ms);
    let health = client(root, vault, id).and_then(|c| c.health().map_err(|e| e.to_string()));
    remember(root, status_of(id, health, last))
}

/// Último status conhecido de cada servidor (monitor e comandos).
pub static STATUS: Mutex<BTreeMap<String, ServerStatus>> = Mutex::new(BTreeMap::new());

/// Guarda o status, grava o último contato e devolve o anterior.
pub fn record(root: &Path, st: &ServerStatus) -> Option<ServerStatus> {
    if st.health.is_some() {
        let (id, at) = (st.id.clone(), st.last_contact_ms);
        let _ = update(root, |l| {
            if let Some(s) = l.iter_mut().find(|s| s.id == id) {
                s.last_contact_ms = at;
            }
        });
    }
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(st.id.clone(), st.clone())
}

fn remember(root: &Path, st: ServerStatus) -> ServerStatus {
    record(root, &st);
    st
}

// ------------------------------------------------------------ comandos

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn servers_list(state: State<'_, AppState>) -> Result<Vec<ServerEntry>, String> {
    let root = state.root().to_path_buf();
    blocking(move || Ok(load(&root))).await
}

/// Último status conhecido (sem rede): para a lista abrir já colorida.
#[tauri::command]
pub async fn servers_status() -> Result<Vec<ServerStatus>, String> {
    Ok(STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect())
}

#[tauri::command]
pub async fn server_add(
    state: State<'_, AppState>,
    pair_code: String,
) -> Result<ServerEntry, String> {
    let root = state.root().to_path_buf();
    blocking(move || add(&root, &Configured, &pair_code)).await
}

#[tauri::command]
pub async fn server_remove(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let root = state.root().to_path_buf();
    blocking(move || remove(&root, &Configured, &id)).await
}

#[tauri::command]
pub async fn server_test(state: State<'_, AppState>, id: String) -> Result<ServerStatus, String> {
    let root = state.root().to_path_buf();
    blocking(move || Ok(test(&root, &Configured, &id))).await
}

#[tauri::command]
pub async fn server_health(state: State<'_, AppState>, id: String) -> Result<ServerStatus, String> {
    let root = state.root().to_path_buf();
    blocking(move || Ok(health(&root, &Configured, &id))).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use zerith_core::secrets::SecretError;

    /// Cofre em memória: nada vai para o Gerenciador de Credenciais.
    #[derive(Default, Clone)]
    pub(crate) struct MapVault(pub Arc<Mutex<HashMap<String, String>>>);

    impl SecretStore for MapVault {
        fn get(&self, name: &str) -> Result<String, SecretError> {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| SecretError::NotFound(name.into()))
        }
        fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
    }

    /// Servidor HTTP falso mínimo (o desktop não depende da CLI): `ping` livre,
    /// o resto só com `Bearer <token>`. `routes` responde (método, caminho,
    /// corpo) com (status, JSON).
    pub(crate) fn fake_server(
        token: &'static str,
        routes: impl Fn(&str, &str, &str) -> (u16, serde_json::Value) + Send + Sync + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = Arc::clone(&routes);
                std::thread::spawn(move || {
                    let mut r = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let (mut auth, mut len) = (String::new(), 0usize);
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        let (k, v) = h.split_once(':').unwrap_or((h, ""));
                        match k.to_ascii_lowercase().as_str() {
                            "authorization" => auth = v.trim().to_string(),
                            "content-length" => len = v.trim().parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    let mut body = vec![0; len];
                    std::io::Read::read_exact(&mut r, &mut body).unwrap();
                    let body = String::from_utf8_lossy(&body).to_string();
                    let (code, json) = if path == "/v1/ping" {
                        (
                            200,
                            serde_json::json!({"name": "zerith", "version": "9.9.9"}),
                        )
                    } else if auth != format!("Bearer {token}") {
                        (401, serde_json::json!({"error": "token"}))
                    } else {
                        routes(&method, &path, &body)
                    };
                    let b = json.to_string();
                    let mut s = stream;
                    let _ = write!(
                        s,
                        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                        b.len()
                    );
                });
            }
        });
        format!("http://{addr}")
    }

    pub(crate) fn health_json(last_cycle_ms: Option<i64>) -> serde_json::Value {
        serde_json::json!({
            "name": "zerith", "version": "9.9.9", "hostname": "vps", "uptime_secs": 5,
            "daemon": last_cycle_ms.map(|ms| serde_json::json!({
                "version": "9.9.9", "started_at_ms": 1, "last_cycle_ms": ms,
                "hostname": "vps", "active_workflows": 1,
                "consecutive_failures": {"wf-ruim": 2}
            })),
            "workflows_total": 1, "workflows_enabled": 1, "failing_workflows": 0, "runs_active": 0
        })
    }

    #[test]
    fn pareamento_ida_e_volta_no_cadastro() {
        let dir = tempfile::tempdir().unwrap();
        let vault = MapVault::default();
        let url = fake_server("zt_bom", |_, p, _| match p {
            "/v1/health" => (200, health_json(Some(now_ms()))),
            _ => (404, serde_json::json!({"error": "?"})),
        });
        let code = PairingCode {
            v: 1,
            name: "vps".into(),
            url: url.clone(),
            token: "zt_bom".into(),
        }
        .encode();

        let s = add(dir.path(), &vault, &code).unwrap();
        assert_eq!((s.name.as_str(), s.url.as_str()), ("vps", url.as_str()));
        assert_eq!(load(dir.path()), vec![s.clone()]);
        assert_eq!(vault.get(&token_entry(&s.id)).unwrap(), "zt_bom");
        let raw = std::fs::read_to_string(dir.path().join("servers.json")).unwrap();
        assert!(!raw.contains("zt_bom"), "token nunca vai para o arquivo");

        let st = test(dir.path(), &vault, &s.id);
        assert_eq!(
            (st.color, st.version.as_deref()),
            (Color::Green, Some("9.9.9"))
        );
        assert_eq!(st.daemon.unwrap().consecutive_failures["wf-ruim"], 2);

        // Parear de novo o mesmo endereço mantém o id.
        assert_eq!(add(dir.path(), &vault, &code).unwrap().id, s.id);
        assert_eq!(load(dir.path()).len(), 1);

        remove(dir.path(), &vault, &s.id).unwrap();
        assert!(load(dir.path()).is_empty());
        assert!(
            vault.get(&token_entry(&s.id)).is_err(),
            "token apagado do cofre"
        );
    }

    #[test]
    fn cadastro_recusa_token_errado_e_endereco_morto_sem_gravar() {
        let dir = tempfile::tempdir().unwrap();
        let vault = MapVault::default();
        let url = fake_server("zt_bom", |_, _, _| (200, health_json(None)));
        let pc = |url: &str, token: &str| {
            PairingCode {
                v: 1,
                name: "x".into(),
                url: url.into(),
                token: token.into(),
            }
            .encode()
        };
        let err = add(dir.path(), &vault, &pc(&url, "zt_mau")).unwrap_err();
        assert!(err.contains("token recusado"), "{err}");
        // Porta fechada: liga e solta um listener para ter uma porta livre.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let err = add(dir.path(), &vault, &pc(&dead, "zt_bom")).unwrap_err();
        assert!(err.contains("não alcancei"), "{err}");
        assert!(add(dir.path(), &vault, "lixo").is_err());
        assert!(load(dir.path()).is_empty());
        assert!(vault.0.lock().unwrap().is_empty());
    }

    #[test]
    fn cor_do_status() {
        let now = 10_000_000;
        let h =
            |cycle: Option<i64>| -> Health { serde_json::from_value(health_json(cycle)).unwrap() };
        assert_eq!(status_color(None, now), Color::Red);
        assert_eq!(status_color(Some(&h(Some(now - 5_000))), now), Color::Green);
        assert_eq!(
            status_color(Some(&h(Some(now - DAEMON_FRESH_MS))), now),
            Color::Yellow,
            "ciclo de 2 min atrás já é atraso"
        );
        assert_eq!(
            status_color(Some(&h(None)), now),
            Color::Yellow,
            "sem daemon"
        );
    }
}
