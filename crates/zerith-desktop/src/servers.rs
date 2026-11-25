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
use zerith_workflows::remote::{Health, PairingCode, RemoteClient, RemoteError};
use zerith_workflows::{now_ms, random_token, DaemonStatus, Store, Workflow, WorkflowRun};

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
    let unreachable = |e: RemoteError| {
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

// ------------------------------------------------------------ workflows remotos

/// Resultado de enviar um workflow ao servidor.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "workflow", rename_all = "lowercase")]
pub enum Pushed {
    /// Gravado lá e aqui (versão local = versão do servidor).
    Saved(Workflow),
    /// 409: o servidor tem outra versão (vem a dele, para recarregar ou
    /// sobrescrever com a base nova).
    Conflict(Workflow),
}

/// Grava o workflow aqui (cópia de edição, com `run_on`) e manda ao servidor
/// sem o campo. Base da concorrência otimista: `base`, senão o `updated_at_ms`
/// local, que depois de cada envio passa a ser o do servidor. Workflow que
/// nunca foi ao servidor vai com base `None` (criar).
pub fn push(
    root: &Path,
    vault: &dyn SecretStore,
    store: &Store,
    wf: &Workflow,
    base: Option<i64>,
) -> Result<Pushed, String> {
    let server = wf
        .run_on
        .clone()
        .ok_or("workflow sem servidor em 'Executar em'")?;
    let c = client(root, vault, &server)?;
    let sent_before = store
        .get_workflow(&wf.id)
        .map_err(|e| e.to_string())?
        .is_some_and(|w| w.run_on.as_deref() == Some(server.as_str()) && w.updated_at_ms > 0);
    let base = base.or(sent_before.then_some(wf.updated_at_ms));
    let remote = Workflow {
        run_on: None,
        ..wf.clone()
    };
    match c.put(&remote, base) {
        Ok(saved) => {
            let local = Workflow {
                run_on: Some(server),
                ..saved
            };
            store
                .save_workflow(&local)
                .map(Pushed::Saved)
                .map_err(|e| e.to_string())
        }
        Err(RemoteError::Conflict(cur)) => Ok(Pushed::Conflict(*cur)),
        Err(e) => {
            // O servidor não aceitou: guarda a edição aqui para não perdê-la.
            store.save_workflow(wf).map_err(|e| e.to_string())?;
            Err(format!("salvo neste PC, mas não no servidor: {e}"))
        }
    }
}

/// Troca a cópia local pela do servidor ("Recarregar do servidor").
pub fn pull(
    root: &Path,
    vault: &dyn SecretStore,
    store: &Store,
    server: &str,
    id: &str,
) -> Result<Workflow, String> {
    let wf = client(root, vault, server)?
        .get(id)
        .map_err(|e| e.to_string())?;
    store
        .save_workflow(&Workflow {
            run_on: Some(server.to_string()),
            ..wf
        })
        .map_err(|e| e.to_string())
}

fn with_store<T: Send + 'static>(
    root: PathBuf,
    job: impl FnOnce(&Path, &Store) -> Result<T, String> + Send + 'static,
) -> impl FnOnce() -> Result<T, String> + Send + 'static {
    move || {
        let store = crate::workflows::open_store(&root)?;
        job(&root, &store)
    }
}

#[tauri::command]
pub async fn server_workflow_push(
    state: State<'_, AppState>,
    workflow: Workflow,
    base_updated_at_ms: Option<i64>,
) -> Result<Pushed, String> {
    let root = state.root().to_path_buf();
    blocking(with_store(root, move |r, s| {
        push(r, &Configured, s, &workflow, base_updated_at_ms)
    }))
    .await
}

#[tauri::command]
pub async fn server_workflow_pull(
    state: State<'_, AppState>,
    server_id: String,
    id: String,
) -> Result<Workflow, String> {
    let root = state.root().to_path_buf();
    blocking(with_store(root, move |r, s| {
        pull(r, &Configured, s, &server_id, &id)
    }))
    .await
}

/// Apaga o workflow (e as execuções) no servidor; 404 conta como feito.
#[tauri::command]
pub async fn server_workflow_delete(
    state: State<'_, AppState>,
    server_id: String,
    id: String,
) -> Result<(), String> {
    let root = state.root().to_path_buf();
    blocking(
        move || match client(&root, &Configured, &server_id)?.delete(&id) {
            Ok(()) | Err(RemoteError::Status(404, _)) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    )
    .await
}

fn remote_call<T: Send + 'static>(
    root: PathBuf,
    server_id: String,
    job: impl FnOnce(&RemoteClient) -> Result<T, RemoteError> + Send + 'static,
) -> impl FnOnce() -> Result<T, String> + Send + 'static {
    move || job(&client(&root, &Configured, &server_id)?).map_err(|e| e.to_string())
}

/// Lista do servidor (status, última execução, ativo) para o painel de saúde.
#[tauri::command]
pub async fn server_workflows(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<Vec<Workflow>, String> {
    let root = state.root().to_path_buf();
    blocking(remote_call(root, server_id, |c| c.list())).await
}

#[tauri::command]
pub async fn server_workflow_runs(
    state: State<'_, AppState>,
    server_id: String,
    id: String,
    limit: Option<usize>,
) -> Result<Vec<WorkflowRun>, String> {
    let root = state.root().to_path_buf();
    let lim = limit.unwrap_or(25).clamp(1, 200);
    blocking(remote_call(root, server_id, move |c| c.runs(&id, lim))).await
}

/// Dispara no servidor; o resultado chega por `server:event` (`run_finished`).
#[tauri::command]
pub async fn server_workflow_run(
    state: State<'_, AppState>,
    server_id: String,
    id: String,
) -> Result<String, String> {
    let root = state.root().to_path_buf();
    blocking(remote_call(root, server_id, move |c| c.run(&id, None))).await
}

/// Pausa/ativa no servidor e espelha na cópia local (mesma versão de lá).
#[tauri::command]
pub async fn server_workflow_enabled(
    state: State<'_, AppState>,
    server_id: String,
    id: String,
    enabled: bool,
) -> Result<Workflow, String> {
    let root = state.root().to_path_buf();
    blocking(with_store(root, move |r, s| {
        let wf = client(r, &Configured, &server_id)?
            .set_enabled(&id, enabled)
            .map_err(|e| e.to_string())?;
        s.save_workflow(&Workflow {
            run_on: Some(server_id),
            ..wf
        })
        .map_err(|e| e.to_string())
    }))
    .await
}

// ------------------------------------------------------------ monitor

/// Evento SSE do servidor reemitido ao frontend como `server:event`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerEvent {
    pub server_id: String,
    pub event: String,
    pub data: serde_json::Value,
}

/// Lê um fluxo `text/event-stream` e chama `on` a cada evento completo com
/// (id, evento, data). Termina no fim do fluxo ou em erro de leitura.
pub fn parse_sse(
    r: impl std::io::BufRead,
    mut on: impl FnMut(Option<&str>, &str, &str),
) -> std::io::Result<()> {
    let (mut id, mut event, mut data) = (None::<String>, String::new(), String::new());
    for line in r.lines() {
        let line = line?;
        if line.is_empty() {
            if !data.is_empty() {
                let ev = if event.is_empty() { "message" } else { &event };
                on(id.as_deref(), ev, data.trim_end_matches('\n'));
            }
            (id, event, data) = (None, String::new(), String::new());
            continue;
        }
        let (k, v) = line.split_once(':').unwrap_or((&line, ""));
        let v = v.strip_prefix(' ').unwrap_or(v);
        match k {
            "id" => id = Some(v.to_string()),
            "event" => event = v.to_string(),
            "data" => {
                data.push_str(v);
                data.push('\n');
            }
            _ => {}
        }
    }
    Ok(())
}

/// O que merece notificação numa mudança de estado: o servidor caiu (era
/// conhecido e não-vermelho, ficou vermelho) e workflows que passaram a
/// falhar (estavam fora de `before`, entraram em `after`).
pub fn alerts(
    prev: Option<Color>,
    now: Color,
    before: &std::collections::BTreeSet<String>,
    after: &std::collections::BTreeSet<String>,
) -> (bool, Vec<String>) {
    let down = now == Color::Red && prev.is_some_and(|p| p != Color::Red);
    (down, after.difference(before).cloned().collect())
}

/// Workflows remotos falhando, por servidor (para notificar uma vez).
static FAILING: Mutex<BTreeMap<String, std::collections::BTreeSet<String>>> =
    Mutex::new(BTreeMap::new());
static MONITOR_COLOR: Mutex<BTreeMap<String, Color>> = Mutex::new(BTreeMap::new());
/// Servidores com uma thread de SSE viva.
static WATCHED: Mutex<std::collections::BTreeSet<String>> =
    Mutex::new(std::collections::BTreeSet::new());

fn wf_name(root: &Path, id: &str) -> String {
    crate::workflows::open_store(root)
        .ok()
        .and_then(|s| s.get_workflow(id).ok().flatten())
        .map_or_else(|| id.to_string(), |w| w.name)
}

/// Atualiza a lista de falhas do servidor e notifica as novas.
fn set_failing(
    app: &tauri::AppHandle,
    root: &Path,
    server: &str,
    f: impl FnOnce(&mut std::collections::BTreeSet<String>),
) {
    let (before, after) = {
        let mut map = FAILING.lock().unwrap_or_else(|e| e.into_inner());
        let set = map.entry(server.to_string()).or_default();
        let before = set.clone();
        f(set);
        (before, set.clone())
    };
    let (_, new) = alerts(None, Color::Green, &before, &after);
    let srv = find(root, server).map_or_else(|_| server.to_string(), |s| s.name);
    for id in new {
        crate::notify::ping(
            app,
            "Workflow falhou no servidor",
            &format!("{} em {srv}", wf_name(root, &id)),
        );
    }
}

fn on_status(app: &tauri::AppHandle, root: &Path, st: ServerStatus) {
    use tauri::Emitter;
    record(root, &st);
    // Cor vista pelo monitor (não a do último "Testar"): o aviso de queda sai
    // uma vez mesmo que a tela tenha conferido antes.
    let prev = MONITOR_COLOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(st.id.clone(), st.color);
    let (down, _) = alerts(prev, st.color, &Default::default(), &Default::default());
    if down {
        let name = find(root, &st.id).map_or_else(|_| st.id.clone(), |s| s.name);
        crate::notify::ping(
            app,
            "Servidor sem resposta",
            &format!("{name}: {}", st.error.as_deref().unwrap_or("")),
        );
    }
    if let Some(d) = &st.daemon {
        let now: std::collections::BTreeSet<String> =
            d.consecutive_failures.keys().cloned().collect();
        set_failing(app, root, &st.id, |s| *s = now);
    }
    let _ = app.emit("server:status", &st);
}

/// Acompanha um servidor: SSE `GET /v1/events` reemitido como
/// `server:event`; o `daemon_status` (a cada 30 s) vira `server:status`. Caiu
/// a conexão: confere o health e reconecta com espera dobrando até 30 s (se o
/// SSE não passar, por exemplo um proxy que bufferiza, isso vira polling do
/// health a cada 30 s). Sai quando o servidor é removido.
fn watch(app: tauri::AppHandle, root: PathBuf, id: String) {
    use std::io::BufReader;
    use std::time::Duration;
    use tauri::Emitter;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        // daemon_status chega a cada 30 s: 75 s sem nada = conexão morta.
        .timeout_read(Duration::from_secs(75))
        .redirects(0)
        .build();
    let mut wait = Duration::from_secs(1);
    let mut last_id: Option<String> = None;
    while let Ok(s) = find(&root, &id) {
        let token = Configured.get(&token_entry(&id)).unwrap_or_default();
        let mut req = agent
            .get(&format!("{}/v1/events", s.url))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "text/event-stream");
        if let Some(l) = &last_id {
            req = req.set("Last-Event-ID", l);
        }
        if let Ok(resp) = req.call() {
            wait = Duration::from_secs(1);
            let _ = parse_sse(BufReader::new(resp.into_reader()), |eid, ev, data| {
                if let Some(e) = eid {
                    last_id = Some(e.to_string());
                }
                let data: serde_json::Value = serde_json::from_str(data).unwrap_or_default();
                if ev == "daemon_status" {
                    let h = serde_json::from_value(data.clone()).map_err(|e| e.to_string());
                    on_status(&app, &root, status_of(&id, h, None));
                    return;
                }
                if ev == "run_finished" {
                    if let Some(wf) = data["workflow_id"].as_str() {
                        let failed = data["status"] == "error";
                        set_failing(&app, &root, &id, |s| {
                            if failed {
                                s.insert(wf.to_string());
                            } else {
                                s.remove(wf);
                            }
                        });
                    }
                }
                let _ = app.emit(
                    "server:event",
                    ServerEvent {
                        server_id: id.clone(),
                        event: ev.to_string(),
                        data,
                    },
                );
            });
        }
        if find(&root, &id).is_err() {
            break;
        }
        on_status(&app, &root, health(&root, &Configured, &id));
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_secs(30));
    }
    WATCHED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
}

/// Sobe uma thread de [`watch`] para cada servidor cadastrado (confere a
/// lista a cada 10 s, então um servidor novo entra sozinho).
pub fn spawn_monitor(app: tauri::AppHandle, root: PathBuf) {
    std::thread::spawn(move || loop {
        for s in load(&root) {
            let fresh = WATCHED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(s.id.clone());
            if fresh {
                let (app, root) = (app.clone(), root.clone());
                std::thread::spawn(move || watch(app, root, s.id));
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
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

    /// Cadastra um servidor falso com PUT de verdade (concorrência otimista
    /// como a do `zerith server`) e devolve (raiz, cofre, id).
    fn paired_with_put() -> (tempfile::TempDir, MapVault, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = MapVault::default();
        let held: Arc<Mutex<Option<serde_json::Value>>> = Arc::default();
        let url = fake_server("zt_bom", move |m, p, body| {
            let mut cur = held.lock().unwrap();
            match (m, p) {
                ("GET", "/v1/health") => (200, health_json(Some(now_ms()))),
                ("PUT", _) => {
                    let b: serde_json::Value = serde_json::from_str(body).unwrap();
                    assert!(
                        b["workflow"].get("run_on").is_none(),
                        "run_on não vai ao servidor"
                    );
                    if let Some(c) = cur.as_ref() {
                        if b["base_updated_at_ms"] != c["updated_at_ms"] {
                            return (409, serde_json::json!({"error": "x", "current": c}));
                        }
                    }
                    let mut wf = b["workflow"].clone();
                    let next = cur
                        .as_ref()
                        .map_or(1000, |c| c["updated_at_ms"].as_i64().unwrap() + 1);
                    wf["updated_at_ms"] = next.into();
                    *cur = Some(wf.clone());
                    (200, wf)
                }
                _ => (404, serde_json::json!({"error": "?"})),
            }
        });
        let code = PairingCode {
            v: 1,
            name: "vps".into(),
            url,
            token: "zt_bom".into(),
        }
        .encode();
        let id = add(dir.path(), &vault, &code).unwrap().id;
        (dir, vault, id)
    }

    #[test]
    fn enviar_workflow_cria_atualiza_e_avisa_conflito() {
        let (dir, vault, srv) = paired_with_put();
        let store = Store::open(&dir.path().join("workflows.db")).unwrap();
        let mut wf: Workflow = serde_json::from_value(serde_json::json!({
            "id": "w", "name": "w", "created_at_ms": 1, "updated_at_ms": 5
        }))
        .unwrap();
        wf.run_on = Some(srv.clone());

        let Pushed::Saved(v1) = push(dir.path(), &vault, &store, &wf, None).unwrap() else {
            panic!("primeiro envio cria")
        };
        assert_eq!(
            (v1.updated_at_ms, v1.run_on.as_deref()),
            (1000, Some(srv.as_str()))
        );
        assert_eq!(
            store.get_workflow("w").unwrap().unwrap().updated_at_ms,
            1000
        );

        // Edição em cima da versão local: base = 1000.
        let Pushed::Saved(v2) = push(dir.path(), &vault, &store, &v1, None).unwrap() else {
            panic!("segunda edição grava")
        };
        assert_eq!(v2.updated_at_ms, 1001);

        // Outro dispositivo já salvou: a base velha vira conflito com a versão dele.
        let Pushed::Conflict(cur) = push(dir.path(), &vault, &store, &v1, Some(1000)).unwrap()
        else {
            panic!("base velha = 409")
        };
        assert_eq!(cur.updated_at_ms, 1001);
        // "Sobrescrever": manda de novo com a base do servidor.
        assert!(matches!(
            push(dir.path(), &vault, &store, &v1, Some(cur.updated_at_ms)).unwrap(),
            Pushed::Saved(_)
        ));
    }

    #[test]
    fn sse_vira_eventos_com_id() {
        let raw = "event: daemon_status\ndata: {\"a\":1}\n\n: comentário\nid: 7\nevent: run_finished\ndata: {\"status\":\"error\"}\n\nid: 8\ndata: x\n";
        let mut got = Vec::new();
        parse_sse(std::io::Cursor::new(raw), |id, ev, d| {
            got.push((id.map(String::from), ev.to_string(), d.to_string()))
        })
        .unwrap();
        assert_eq!(
            got,
            vec![
                (None, "daemon_status".into(), "{\"a\":1}".into()),
                (
                    Some("7".into()),
                    "run_finished".into(),
                    "{\"status\":\"error\"}".into()
                ),
            ],
            "evento sem linha em branco final não sai pela metade"
        );
    }

    #[test]
    fn notifica_uma_vez_por_mudanca_de_estado() {
        use std::collections::BTreeSet;
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let none = BTreeSet::new();
        assert!(alerts(Some(Color::Green), Color::Red, &none, &none).0);
        assert!(alerts(Some(Color::Yellow), Color::Red, &none, &none).0);
        assert!(
            !alerts(Some(Color::Red), Color::Red, &none, &none).0,
            "já estava caído"
        );
        assert!(
            !alerts(None, Color::Red, &none, &none).0,
            "primeira leitura não avisa"
        );
        let (_, new) = alerts(None, Color::Green, &set(&["a"]), &set(&["a", "b"]));
        assert_eq!(new, vec!["b".to_string()], "só o que passou a falhar");
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
