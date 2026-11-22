use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use zerith_core::{Paths, Settings};
use zerith_memory::{MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_runtime::{run_agent_turn, AgentOptions, TurnContext};
use zerith_security::{Mode, ModeHandle, Policy};
use zerith_sessions::{NewMessage, Role, SessionSource, Store as SessionStore};
use zerith_tools::Registry;
use zerith_workflows::{
    channels, execute_workflow, now_ms, starter_templates, test_single_node, unix_secs, AiExecutor,
    ChannelConfig, CronSchedule, LocalDateTime, NodeEvent, NodeKind, RunOptions, Store, Workflow,
    WorkflowEdge, WorkflowError, WorkflowNode, WorkflowRun,
};

use crate::state::AppState;

static WEBHOOK_PORT: AtomicU16 = AtomicU16::new(47891);

fn open_store(root: &Path) -> Result<Store, String> {
    let st = Store::open(&root.join("workflows.db")).map_err(|e| e.to_string())?;
    st.seed_defaults().map_err(|e| e.to_string())?;
    Ok(st)
}

struct DesktopAiExecutor {
    root: PathBuf,
}

impl DesktopAiExecutor {
    fn resolve_gateway_candidates(
        &self,
        model: Option<&str>,
    ) -> (Paths, PathBuf, Settings, Vec<(Client, String)>) {
        let paths = Paths::with_root(&self.root);
        let dir = crate::turn::last_workspace(&paths.db());
        let cfg = Settings::load(&paths, Some(&dir)).settings;
        let fallback = crate::wire::configured_providers_from(&cfg, &self.root)
            .into_iter()
            .next()
            .unwrap_or_else(ProviderConfig::from_env);

        let wanted = model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                cfg.routing
                    .main_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                cfg.model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                fallback
                    .default_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("");

        let (gw, _) = zerith_runtime::resolve_gateway(&cfg, wanted, &fallback);
        let mut clients = Vec::new();
        for cand in gw.candidates.into_iter().take(3) {
            let model_name = if !cand.model.trim().is_empty() {
                cand.model
            } else {
                cand.config.default_model.clone().unwrap_or_default()
            };
            if model_name.trim().is_empty() {
                continue;
            }
            if let Ok(client) = Client::new(cand.config) {
                clients.push((client, model_name));
            }
        }
        (paths, dir, cfg, clients)
    }
}

impl AiExecutor for DesktopAiExecutor {
    fn complete(
        &self,
        model: Option<&str>,
        system: &str,
        prompt: &str,
    ) -> zerith_workflows::Result<String> {
        let (_, _, _, clients) = self.resolve_gateway_candidates(model);
        if clients.is_empty() {
            return Err(WorkflowError::Ai(
                "Nenhum modelo de IA configurado. Selecione um modelo no nó ou configure uma conta em Ajustes → Contas.".into(),
            ));
        }

        let mut last_err = None;
        for (client, model_name) in clients {
            let req = ChatRequest::new(
                &model_name,
                vec![ChatMessage::system(system), ChatMessage::user(prompt)],
            )
            .max_tokens(4096)
            .temperature(0.2);

            match client.complete(&req, &Cancel::default()) {
                Ok(comp) => return Ok(comp.text),
                Err(e) => {
                    last_err = Some(zerith_providers::explain::headline(&e));
                }
            }
        }

        Err(WorkflowError::Ai(last_err.unwrap_or_else(|| {
            "Falha desconhecida ao chamar modelo de IA".into()
        })))
    }

    fn complete_agent_loop(
        &self,
        bot_name: Option<&str>,
        model: Option<&str>,
        system: &str,
        prompt: &str,
        max_turns: usize,
        tool_mode: &str,
    ) -> zerith_workflows::Result<String> {
        let (paths, workspace, mut settings, clients) = self.resolve_gateway_candidates(model);
        let Some((client, model_name)) = clients.into_iter().next() else {
            return Err(WorkflowError::Ai(
                "Nenhum modelo de IA configurado para o loop do agente.".into(),
            ));
        };

        let db_path = paths.db();
        let store = SessionStore::open(&db_path).map_err(|e| WorkflowError::Ai(e.to_string()))?;
        let contrib = zerith_plugins::Library::open(&self.root.join("plugins"))
            .and_then(|l| l.contributions())
            .ok();
        if let Some(c) = &contrib {
            c.apply_hooks(&mut settings);
        }
        let sec_mode = if tool_mode.eq_ignore_ascii_case("bypass") {
            settings.permissions.mode = "danger-full-access".to_string();
            Mode::Bypass
        } else {
            Mode::DontAsk
        };

        let policy = Policy::from_settings(&settings, &workspace)
            .map(|p| p.with_handle(ModeHandle::new(sec_mode)))
            .ok();

        let mut tools = Registry::new();
        let mut mcp = zerith_mcp::McpRegistry::load(&paths, Some(&workspace));
        if let Some(c) = &contrib {
            mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
        }
        let _ = mcp.register_into(&mut tools);
        let prompt = if mcp.is_empty() {
            prompt.to_string()
        } else {
            mcp.expand_mentions(prompt).0
        };

        let skills = zerith_skills::Library::open(self.root.join("skills"))
            .ok()
            .map(|l| match &contrib {
                Some(c) => l.with_extra(c.skill_dirs()),
                None => l,
            });
        let extra = contrib.as_ref().map(|c| c.agents.as_slice()).unwrap_or(&[]);
        let mut agent = bot_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|n| zerith_core::agents::find_with_extra(&paths, Some(&workspace), extra, n));
        if let Some(a) = agent.as_mut() {
            merge_agent_system(a, system);
        }

        let memory = SqliteMemory::open(&db_path).ok();
        let scope = zerith_memory::Scope::workspace(&workspace);
        let sess_label = match bot_name.map(str::trim).filter(|s| !s.is_empty()) {
            Some(b) => format!("@{b} · Loop Workflow"),
            None => "Workflow · Loop Agente".to_string(),
        };
        let session = store
            .create_session(&sess_label, SessionSource::Subagent)
            .map_err(|e| WorkflowError::Ai(e.to_string()))?;
        let _ = store.set_workspace(&session.id, &workspace.display().to_string());

        let full_prompt = if agent.is_none() && !system.trim().is_empty() {
            format!("[Instruções do bloco]\n{}\n\n{}", system.trim(), prompt)
        } else {
            prompt
        };

        let report = run_agent_turn(
            &store,
            &client,
            TurnContext {
                memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
                scope: Some(&scope),
                skills: skills.as_ref(),
                agent: agent.as_ref(),
                settings: Some(&settings),
                project_dir: Some(&workspace),
                no_project_instructions: false,
            },
            AgentOptions {
                tools: policy.is_some().then_some(&tools),
                policy: policy.as_ref(),
                max_steps: max_turns.clamp(1, 20),
                history: 4,
                spill_dir: Some(self.root.join("spill")),
                ..AgentOptions::default()
            },
            &session.id,
            &model_name,
            &full_prompt,
            &Cancel::default(),
            |_| {},
        )
        .map_err(|e| WorkflowError::Ai(e.to_string()))?;

        Ok(report.text)
    }

    fn resolve_bot(&self, bot_name: &str) -> Option<(Option<String>, String)> {
        let wanted = bot_name.trim();
        if wanted.is_empty() {
            return None;
        }
        crate::agents::all(&self.root)
            .into_iter()
            .find(|a| !a.disabled && a.name.eq_ignore_ascii_case(wanted))
            .map(|a| (a.model, a.instructions))
    }
}

fn merge_agent_system(agent: &mut zerith_core::agents::AgentDef, system: &str) {
    let sys = system.trim();
    if sys.is_empty() {
        return;
    }
    let base = agent.instructions.trim();
    if sys == base || (sys == "You are a helpful automation agent." && !base.is_empty()) {
        return;
    }
    if let Some((prefix, custom)) = sys.split_once("\n\nInstruções adicionais do nó:\n") {
        let custom = custom.trim();
        if base.is_empty() {
            agent.instructions = custom.to_string();
        } else if prefix.trim() != base {
            agent.instructions = sys.to_string();
        } else if !custom.is_empty() && !base.contains(custom) {
            agent.instructions = format!("{base}\n\nInstruções adicionais do nó:\n{custom}");
        }
    } else if base.is_empty() {
        agent.instructions = sys.to_string();
    } else if !base.contains(sys) {
        agent.instructions = format!("{base}\n\nInstruções adicionais do nó:\n{sys}");
    }
}

fn notify_bot_chat(app: &AppHandle, root: &Path, wf: &Workflow, run: &WorkflowRun) {
    let Some(bot_raw) = wf
        .bot_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    // Quiet heartbeat: scheduled cron runs with zero new output stay silent.
    if run.trigger_source == "cron" && run.status == "ok" && run.summary.trim().is_empty() {
        return;
    }
    let db_path = Paths::with_root(root).db();
    let Ok(st) = SessionStore::open(&db_path) else {
        return;
    };
    let prefix = format!("@{bot_raw}");
    let sid = st
        .list_sessions(200)
        .ok()
        .and_then(|list| {
            list.into_iter()
                .find(|s| !s.archived && s.title.starts_with(&prefix))
                .map(|s| s.id)
        })
        .or_else(|| {
            let title = format!("@{bot_raw} · Automação");
            let created = st.create_session(&title, SessionSource::Native).ok()?;
            let ws = crate::turn::last_workspace(&db_path);
            let _ = st.set_workspace(&created.id, &ws.display().to_string());
            Some(created.id)
        });
    let Some(session_id) = sid else {
        return;
    };

    let status_badge = if run.status == "ok" {
        "✅ OK"
    } else {
        "❌ Erro"
    };
    let total_ms: u64 = run.node_logs.iter().map(|n| n.duration_ms).sum();
    let mut clipped_summary = run.summary.trim().to_string();
    if clipped_summary.chars().count() > 2400 {
        clipped_summary = clipped_summary.chars().take(2400).collect();
        clipped_summary.push_str("\n...[resumo truncado]");
    }
    let body = format!(
        "### ⚡ Workflow: **{}** — {}\n- **Gatilho:** `{}` · **Duração:** `{}ms` · **Blocos:** {}\n\n{}",
        wf.name,
        status_badge,
        run.trigger_source,
        total_ms,
        run.node_logs.len(),
        if clipped_summary.is_empty() {
            "_Execução concluída sem texto de saída._"
        } else {
            &clipped_summary
        }
    );

    if st
        .append(&session_id, &NewMessage::new(Role::Assistant, body))
        .is_ok()
    {
        let _ = app.emit(
            "bot:workflow_report",
            json!({
                "botName": bot_raw,
                "sessionId": session_id,
                "workflowId": wf.id,
                "workflowName": wf.name,
                "status": run.status,
                "summary": run.summary,
            }),
        );
    }
}

/// Runs a periodic background cron scheduler inside the desktop app and binds
/// runtime AI/event hooks for native `workflow_*` chat tools plus local webhooks.
pub fn spawn_scheduler(app: AppHandle, root: PathBuf) {
    let hook_app = app.clone();
    zerith_tools::set_workflow_change_hook(Arc::new(move || {
        let _ = hook_app.emit("workflows:changed", ());
    }));
    zerith_tools::set_workflow_ai_executor(Arc::new(DesktopAiExecutor { root: root.clone() }));
    let last_fired: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let last_checked_utc_min: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));

    let wh_app = app.clone();
    let wh_root = root.clone();
    tauri::async_runtime::spawn(async move {
        run_webhook_server(wh_app, wh_root).await;
    });

    tauri::async_runtime::spawn(async move {
        // Give startup a short moment before checking due schedules.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let app_tick = app.clone();
            let root_tick = root.clone();
            let fired_ref = Arc::clone(&last_fired);
            let checked_ref = Arc::clone(&last_checked_utc_min);
            let _ = tauri::async_runtime::spawn_blocking(move || {
                tick_due_workflows(&app_tick, &root_tick, &fired_ref, &checked_ref);
            })
            .await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

const MAX_WEBHOOK_HDR_BYTES: usize = 16_384;
const MAX_WEBHOOK_BODY_BYTES: usize = 262_144;
const MAX_CONCURRENT_WEBHOOKS: usize = 8;

struct RunSlotGuard(Arc<AtomicUsize>);

impl Drop for RunSlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_acquire_run_slot(counter: &Arc<AtomicUsize>) -> Option<RunSlotGuard> {
    loop {
        let cur = counter.load(Ordering::Acquire);
        if cur >= MAX_CONCURRENT_WEBHOOKS {
            return None;
        }
        if counter
            .compare_exchange(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(RunSlotGuard(Arc::clone(counter)));
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_webhook_match(wfs: Vec<Workflow>, slug_lookup: &str) -> Option<(Workflow, String)> {
    for w in wfs {
        if !w.enabled {
            continue;
        }
        let matched_node = w
            .nodes
            .iter()
            .find(|n| {
                n.kind == NodeKind::TriggerWebhook
                    && n.config
                        .get("slug")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .is_some_and(|s| !s.is_empty() && s == slug_lookup)
            })
            .or_else(|| {
                if w.id == slug_lookup {
                    w.nodes.iter().find(|n| n.kind == NodeKind::TriggerWebhook)
                } else {
                    None
                }
            });
        if let Some(node) = matched_node {
            let secret = node
                .config
                .get("secret")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            return Some((w, secret));
        }
    }
    None
}

async fn run_webhook_server(app: AppHandle, root: PathBuf) {
    let listener = match TcpListener::bind("127.0.0.1:47891").await {
        Ok(l) => l,
        Err(_) => match TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(_) => return,
        },
    };
    if let Ok(addr) = listener.local_addr() {
        WEBHOOK_PORT.store(addr.port(), Ordering::Relaxed);
    }
    let active_runs = Arc::new(AtomicUsize::new(0));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let app_c = app.clone();
        let root_c = root.clone();
        let active_c = Arc::clone(&active_runs);
        tauri::async_runtime::spawn(async move {
            handle_webhook_conn(stream, app_c, root_c, active_c).await;
        });
    }
}

async fn write_http_response(stream: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
    // Briefly drain remaining request bytes (bounded in time and size) so
    // Windows Winsock does not emit TCP RST over the response on early exits
    // such as 413 or 429.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(25);
    let mut drain = [0u8; 4096];
    let mut drained = 0usize;
    while drained < 65_536 {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rem.is_zero() {
            break;
        }
        match tokio::time::timeout(rem, stream.read(&mut drain)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => drained += n,
        }
    }
}

async fn handle_webhook_conn(
    stream: TcpStream,
    app: AppHandle,
    root: PathBuf,
    active_runs: Arc<AtomicUsize>,
) {
    process_webhook_conn(
        stream,
        root,
        active_runs,
        move |wf, body_text, root, guard| {
            std::thread::spawn(move || {
                let _slot_guard = guard;
                let Ok(st) = open_store(&root) else {
                    return;
                };
                let ai = DesktopAiExecutor { root: root.clone() };
                let mut emit = |ev: NodeEvent| {
                    let _ = app.emit("workflow:event", &ev);
                };
                let mut opts = RunOptions {
                    store: &st,
                    data_root: &root,
                    ai: &ai,
                    trigger_source: "webhook",
                    tz_offset_minutes: -180,
                    initial_input: if body_text.is_empty() {
                        None
                    } else {
                        Some(body_text.as_str())
                    },
                    on_event: &mut emit,
                };
                if let Ok(run) = execute_workflow(&wf, &mut opts) {
                    notify_bot_chat(&app, &root, &wf, &run);
                    let _ = app.emit("workflow:done", &run);
                }
            });
        },
    )
    .await;
}

async fn process_webhook_conn<F>(
    mut stream: TcpStream,
    root: PathBuf,
    active_runs: Arc<AtomicUsize>,
    spawn_run: F,
) where
    F: FnOnce(Workflow, String, PathBuf, RunSlotGuard) + Send + 'static,
{
    let Some(slot_guard) = try_acquire_run_slot(&active_runs) else {
        write_http_response(
            &mut stream,
            429,
            "Too Many Requests",
            r#"{"ok":false,"error":"too many concurrent webhook runs"}"#,
        )
        .await;
        return;
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut raw = Vec::with_capacity(4096);
    let hdr_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            if pos > MAX_WEBHOOK_HDR_BYTES {
                write_http_response(
                    &mut stream,
                    413,
                    "Payload Too Large",
                    r#"{"ok":false,"error":"headers too large"}"#,
                )
                .await;
                return;
            }
            break pos;
        }
        if raw.len() >= MAX_WEBHOOK_HDR_BYTES {
            write_http_response(
                &mut stream,
                413,
                "Payload Too Large",
                r#"{"ok":false,"error":"headers too large"}"#,
            )
            .await;
            return;
        }
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rem.is_zero() {
            return;
        }
        let mut chunk = [0u8; 2048];
        match tokio::time::timeout(rem, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                if !raw.is_empty() {
                    write_http_response(
                        &mut stream,
                        400,
                        "Bad Request",
                        r#"{"ok":false,"error":"malformed http"}"#,
                    )
                    .await;
                }
                return;
            }
            Ok(Ok(n)) => raw.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) | Err(_) => return,
        }
    };

    let head_str = String::from_utf8_lossy(&raw[..hdr_end]);
    let mut lines = head_str.lines();
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    if !matches!(method, "POST" | "PUT") {
        write_http_response(
            &mut stream,
            405,
            "Method Not Allowed",
            r#"{"ok":false,"error":"use POST or PUT"}"#,
        )
        .await;
        return;
    }

    let (path_only, query_str) = target.split_once('?').unwrap_or((target, ""));
    let Some(slug_raw) = path_only.strip_prefix("/webhook/") else {
        write_http_response(
            &mut stream,
            404,
            "Not Found",
            r#"{"ok":false,"error":"expected /webhook/<slug>"}"#,
        )
        .await;
        return;
    };
    let slug = slug_raw.trim_matches('/').to_string();
    if slug.is_empty() {
        write_http_response(
            &mut stream,
            404,
            "Not Found",
            r#"{"ok":false,"error":"empty webhook slug"}"#,
        )
        .await;
        return;
    }

    let mut content_len: usize = 0;
    let mut bearer_tok = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let kl = k.trim().to_ascii_lowercase();
            let vt = v.trim();
            if kl == "content-length" {
                content_len = vt.parse::<usize>().unwrap_or(0).min(MAX_WEBHOOK_BODY_BYTES);
            } else if kl == "authorization" {
                if let Some(tok) = vt
                    .strip_prefix("Bearer ")
                    .or_else(|| vt.strip_prefix("bearer "))
                {
                    bearer_tok = tok.trim().to_string();
                }
            }
        }
    }

    let mut body_bytes = raw[hdr_end + 4..].to_vec();
    while body_bytes.len() < content_len {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rem.is_zero() {
            break;
        }
        let mut chunk = [0u8; 8192];
        match tokio::time::timeout(rem, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
            Ok(Ok(m)) => body_bytes.extend_from_slice(&chunk[..m]),
        }
    }
    body_bytes.truncate(content_len.min(MAX_WEBHOOK_BODY_BYTES));
    let body_text = String::from_utf8_lossy(&body_bytes).to_string();

    let query_token = query_str
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| percent_decode(v))
        .unwrap_or_default();

    let root_lookup = root.clone();
    let slug_lookup = slug.clone();
    let found = tokio::task::spawn_blocking(move || {
        let st = open_store(&root_lookup).ok()?;
        let wfs = st.list_workflows().ok()?;
        find_webhook_match(wfs, &slug_lookup)
    })
    .await
    .ok()
    .flatten();

    let Some((wf, required_secret)) = found else {
        write_http_response(
            &mut stream,
            404,
            "Not Found",
            r#"{"ok":false,"error":"no enabled webhook workflow matches slug"}"#,
        )
        .await;
        return;
    };

    if !required_secret.is_empty()
        && bearer_tok != required_secret
        && query_token != required_secret
    {
        write_http_response(
            &mut stream,
            401,
            "Unauthorized",
            r#"{"ok":false,"error":"invalid webhook token"}"#,
        )
        .await;
        return;
    }

    let wf_id = wf.id.clone();
    spawn_run(wf, body_text, root, slot_guard);

    let ok_json = format!(
        r#"{{"ok":true,"workflow_id":"{}"}}"#,
        wf_id.replace('"', "")
    );
    write_http_response(&mut stream, 202, "Accepted", &ok_json).await;
}

fn collect_due_workflows(
    wfs: &[Workflow],
    now_secs: i64,
    last_fired: &Mutex<HashMap<String, i64>>,
    last_checked_utc_min: &Mutex<Option<i64>>,
) -> Vec<(Workflow, i32)> {
    let now_utc_min = now_secs.div_euclid(60);
    let start_min = {
        let mut checked = last_checked_utc_min
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let start = match *checked {
            Some(prev) if prev < now_utc_min => (prev + 1).max(now_utc_min.saturating_sub(5)),
            _ => now_utc_min,
        };
        *checked = Some(now_utc_min);
        start
    };

    let mut due = Vec::new();
    for utc_min in start_min..=now_utc_min {
        let check_secs = utc_min * 60;
        for wf in wfs {
            if !wf.enabled {
                continue;
            }
            let mut matched_tz: Option<i32> = None;
            for node in &wf.nodes {
                if node.kind != NodeKind::TriggerCron {
                    continue;
                }
                let expr = node
                    .config
                    .get("cron")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0 8 * * *");
                let tz = node
                    .config
                    .get("tz_offset_minutes")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32)
                    .unwrap_or(-180);
                if let Ok(sched) = CronSchedule::parse(expr) {
                    let dt = LocalDateTime::from_unix_secs(check_secs, tz);
                    if sched.matches_dt(&dt) {
                        let mut m = last_fired.lock().unwrap_or_else(|e| e.into_inner());
                        if m.get(&wf.id) == Some(&utc_min) {
                            break;
                        }
                        m.insert(wf.id.clone(), utc_min);
                        matched_tz = Some(tz);
                        break;
                    }
                }
            }

            if let Some(tz) = matched_tz {
                due.push((wf.clone(), tz));
            }
        }
    }
    due
}

fn tick_due_workflows(
    app: &AppHandle,
    root: &Path,
    last_fired: &Arc<Mutex<HashMap<String, i64>>>,
    last_checked_utc_min: &Arc<Mutex<Option<i64>>>,
) {
    let Ok(store) = open_store(root) else {
        return;
    };
    let Ok(wfs) = store.list_workflows() else {
        return;
    };
    let now_secs = unix_secs();

    for (wf, tz) in collect_due_workflows(&wfs, now_secs, last_fired, last_checked_utc_min) {
        let app_bg = app.clone();
        let root_bg = root.to_path_buf();
        std::thread::spawn(move || {
            let Ok(st) = open_store(&root_bg) else {
                return;
            };
            let ai = DesktopAiExecutor {
                root: root_bg.clone(),
            };
            let mut emit = |ev: NodeEvent| {
                let _ = app_bg.emit("workflow:event", &ev);
            };
            let mut opts = RunOptions {
                store: &st,
                data_root: &root_bg,
                ai: &ai,
                trigger_source: "cron",
                tz_offset_minutes: tz,
                initial_input: None,
                on_event: &mut emit,
            };
            if let Ok(run) = execute_workflow(&wf, &mut opts) {
                notify_bot_chat(&app_bg, &root_bg, &wf, &run);
                let _ = app_bg.emit("workflow:done", &run);
            }
        });
    }
}

#[tauri::command]
pub async fn workflow_list(state: State<'_, AppState>) -> Result<Vec<Workflow>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .list_workflows()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_save(
    state: State<'_, AppState>,
    workflow: Workflow,
) -> Result<Workflow, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .save_workflow(&workflow)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .delete_workflow(&id)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_toggle(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .set_workflow_enabled(&id, enabled)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_run_now(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    tz_offset_minutes: Option<i32>,
) -> Result<WorkflowRun, String> {
    let root = state.root().to_path_buf();
    let tz = tz_offset_minutes.unwrap_or(-180);
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&root)?;
        let wf = store
            .get_workflow(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("workflow '{id}' não encontrado"))?;
        let ai = DesktopAiExecutor { root: root.clone() };
        let mut emit = |ev: NodeEvent| {
            let _ = app.emit("workflow:event", &ev);
        };
        let mut opts = RunOptions {
            store: &store,
            data_root: &root,
            ai: &ai,
            trigger_source: "manual",
            tz_offset_minutes: tz,
            initial_input: None,
            on_event: &mut emit,
        };
        let run = execute_workflow(&wf, &mut opts).map_err(|e| e.to_string())?;
        notify_bot_chat(&app, &root, &wf, &run);
        let _ = app.emit("workflow:done", &run);
        Ok(run)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_runs(
    state: State<'_, AppState>,
    id: String,
    limit: Option<usize>,
) -> Result<Vec<WorkflowRun>, String> {
    let root = state.root().to_path_buf();
    let lim = limit.unwrap_or(25).clamp(1, 100);
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .list_runs(&id, lim)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_generate_from_prompt(
    state: State<'_, AppState>,
    prompt: String,
    tz_offset_minutes: Option<i32>,
) -> Result<Workflow, String> {
    let root = state.root().to_path_buf();
    let tz = tz_offset_minutes.unwrap_or(-180);
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&root)?;
        let channels = store.list_channels().unwrap_or_default();
        let channels_hint = if channels.is_empty() {
            "None configured yet".to_string()
        } else {
            channels
                .iter()
                .map(|c| {
                    let kind = serde_json::to_value(c.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    format!("id='{}' name='{}' kind='{}'", c.id, c.name, kind)
                })
                .collect::<Vec<_>>()
                .join("; ")
        };

        let sys = format!(
            r#"You design Zerith visual workflows as pure JSON. Respond ONLY with a raw JSON object (no markdown fences, no commentary).
Schema:
{{
  "name": "Short descriptive title",
  "description": "One-sentence summary",
  "icon": "workflow" | "activity" | "globe" | "bot" | "bell" | "terminal" | "file-text",
  "bot_name": null | "noc-engineer" | "finance-analyst" | "workflow-architect" | "code-reviewer",
  "nodes": [
    {{
      "id": "n1",
      "kind": "trigger_cron" | "trigger_manual" | "trigger_webhook" | "shell" | "http_request" | "ai_agent" | "condition" | "transform" | "delay" | "loop" | "notify_channel" | "save_file",
      "label": "Short node title",
      "x": 40,
      "y": 140,
      "config": {{ ... }}
    }}
  ],
  "edges": [
    {{ "id": "e1", "from": "n1", "to": "n2", "branch": null | "true" | "false" }}
  ]
}}

Config shapes per kind:
- trigger_cron: {{"cron": "0 8 * * *", "tz_offset_minutes": {tz}}}
- trigger_manual: {{}}
- trigger_webhook: {{"slug": "short-slug", "secret": ""}}
- shell: {{"command": "...", "timeout_secs": 30}}
- http_request: {{"method": "GET", "url": "https://...", "headers": {{}}, "body": "", "allow_error_status": true}}
- ai_agent: {{"bot_name": "", "model": "", "system": "...", "prompt": "... {{{{prev}}}}"}}
- condition: {{"mode": "not_empty" | "contains" | "not_contains" | "equals", "pattern": "..."}}
- transform: {{"mode": "template" | "json_path" | "filter_lines", "pattern": "", "template": "{{{{prev}}}}"}}
- delay: {{"seconds": 5}}
- loop: {{"mode": "template" | "shell" | "http", "items": "", "template": "{{{{item}}}}", "separator": "\n"}}
- notify_channel: {{"channel_id": "", "target": "", "message": "{{{{prev}}}}"}}
- save_file: {{"path": "reports/out-{{{{date}}}}.md", "append": false}}

Layout rules:
- Place nodes left-to-right starting at x=40, y=140, stepping +270 horizontally (e.g. 40, 310, 580, 850, 1120).
- Use template vars {{{{prev}}}}, {{{{date}}}}, {{{{time}}}}, or {{{{node.<id>}}}} where downstream nodes consume upstream outputs.
- Configured channels available to bind in notify_channel.channel_id: [{channels_hint}]. Pick the closest matching channel id if one fits, otherwise leave channel_id empty string."#
        );

        let ai = DesktopAiExecutor { root: root.clone() };
        let raw = ai.complete(None, &sys, &prompt).map_err(|e| e.to_string())?;
        let cleaned = strip_json_fence(&raw);
        let parsed: Value = serde_json::from_str(cleaned)
            .map_err(|e| format!("IA retornou JSON inválido: {e}"))?;

        let now = now_ms();
        let mut nodes: Vec<WorkflowNode> = parsed
            .get("nodes")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| "JSON gerado pela IA não contém lista válida de nós".to_string())?;

        for node in &mut nodes {
            if node.kind == NodeKind::TriggerCron {
                if let Some(obj) = node.config.as_object_mut() {
                    obj.entry("tz_offset_minutes")
                        .or_insert_with(|| json!(tz));
                }
            }
        }

        let edges: Vec<WorkflowEdge> = parsed
            .get("edges")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let wf = Workflow {
            id: format!("wf-{now}"),
            name: parsed
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Generated Workflow")
                .to_string(),
            description: parsed
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            icon: parsed
                .get("icon")
                .and_then(|v| v.as_str())
                .unwrap_or("workflow")
                .to_string(),
            enabled: false,
            nodes,
            edges,
            created_at_ms: now,
            updated_at_ms: now,
            last_run_at_ms: None,
            last_status: None,
            bot_name: parsed
                .get("bot_name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        };

        store.save_workflow(&wf).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn strip_json_fence(raw: &str) -> &str {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("```json") {
        rest.trim().strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = t.strip_prefix("```") {
        rest.trim().strip_suffix("```").unwrap_or(rest).trim()
    } else if let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) {
        if a <= b {
            &t[a..=b]
        } else {
            t
        }
    } else {
        t
    }
}

#[tauri::command]
pub async fn channel_list(state: State<'_, AppState>) -> Result<Vec<ChannelConfig>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .list_channels()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn channel_save(
    state: State<'_, AppState>,
    channel: ChannelConfig,
) -> Result<ChannelConfig, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .save_channel(&channel)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn channel_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        open_store(&root)?
            .delete_channel(&id)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn channel_test(
    state: State<'_, AppState>,
    id: String,
    target: Option<String>,
    message: Option<String>,
) -> Result<String, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&root)?;
        let ch = store
            .get_channel(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("canal '{id}' não encontrado"))?;
        let text = message
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Zerith Workflows: mensagem de teste enviada com sucesso!");
        channels::dispatch(&ch, target.as_deref(), text).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn workflow_webhook_base_url() -> Result<String, String> {
    let port = WEBHOOK_PORT.load(Ordering::Relaxed);
    Ok(format!("http://127.0.0.1:{port}/webhook/"))
}

#[tauri::command]
pub async fn workflow_templates() -> Result<Vec<Workflow>, String> {
    Ok(starter_templates())
}

#[tauri::command]
pub async fn workflow_test_node(
    state: State<'_, AppState>,
    node: WorkflowNode,
    sample_input: Option<String>,
    tz_offset_minutes: Option<i32>,
) -> Result<String, String> {
    let root = state.root().to_path_buf();
    let sample = sample_input.unwrap_or_default();
    let tz = tz_offset_minutes.unwrap_or(-180);
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store(&root)?;
        let ai = DesktopAiExecutor { root: root.clone() };
        let mut noop = |_| {};
        let opts = RunOptions {
            store: &store,
            data_root: &root,
            ai: &ai,
            trigger_source: "node_test",
            tz_offset_minutes: tz,
            initial_input: None,
            on_event: &mut noop,
        };
        test_single_node(&node, &sample, &opts).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod webhook_tests {
    use super::*;

    fn webhook_node(id: &str, slug: &str, secret: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            kind: NodeKind::TriggerWebhook,
            label: id.into(),
            x: 0.0,
            y: 0.0,
            config: json!({"slug": slug, "secret": secret}),
        }
    }

    #[test]
    fn percent_decode_handles_plus_and_hex_sequences() {
        assert_eq!(percent_decode("abc+123%20%21%2F%ZZ"), "abc 123 !/%ZZ");
    }

    #[test]
    fn slug_lookup_uses_matched_webhook_nodes_secret() {
        let wf = Workflow {
            id: "wf-multi".into(),
            name: "multi".into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            nodes: vec![
                webhook_node("n1", "first", ""),
                webhook_node("n2", "second", "top-secret"),
            ],
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        };
        let (matched, secret) = find_webhook_match(vec![wf], "second").unwrap();
        assert_eq!(matched.id, "wf-multi");
        assert_eq!(secret, "top-secret");
    }

    #[test]
    fn concurrency_slot_caps_at_8_and_releases_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut guards = Vec::new();
        for _ in 0..MAX_CONCURRENT_WEBHOOKS {
            guards.push(try_acquire_run_slot(&counter).expect("slot available"));
        }
        assert!(try_acquire_run_slot(&counter).is_none());
        guards.pop();
        assert!(try_acquire_run_slot(&counter).is_some());
        drop(guards);
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }

    #[test]
    fn webhook_http_hardening_rejects_get_cors_oversized_headers_and_saturates() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let scratch =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch/wh-test");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let store = open_store(&scratch).unwrap();
        store
            .save_workflow(&Workflow {
                id: "wf-hook".into(),
                name: "hook".into(),
                description: String::new(),
                icon: "workflow".into(),
                enabled: true,
                nodes: vec![webhook_node("n1", "deploy", "my secret+1")],
                edges: vec![],
                created_at_ms: 0,
                updated_at_ms: 0,
                last_run_at_ms: None,
                last_status: None,
                bot_name: None,
            })
            .unwrap();
        drop(store);

        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let send_raw = |listener: TcpListener, req: Vec<u8>, counter: Arc<AtomicUsize>, root: PathBuf| async move {
                let captured = Arc::new(Mutex::new(None::<String>));
                let cap_c = Arc::clone(&captured);
                let srv = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    process_webhook_conn(stream, root, counter, move |_wf, body, _r, _g| {
                        *cap_c.lock().unwrap() = Some(body);
                    })
                    .await;
                    listener
                });
                let mut client = TcpStream::connect(addr).await.unwrap();
                client.write_all(&req).await.unwrap();
                let _ = client.shutdown().await;
                let mut resp = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match client.read(&mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => resp.extend_from_slice(&tmp[..n]),
                        Err(_) if !resp.is_empty() => break,
                        Err(e) => panic!("read failed before any response (req_len={}): {e}", req.len()),
                    }
                }
                let listener = srv.await.unwrap();
                let body_cap = captured.lock().unwrap().clone();
                (
                    listener,
                    String::from_utf8_lossy(&resp).into_owned(),
                    body_cap,
                )
            };

            let counter = Arc::new(AtomicUsize::new(0));
            // 1. GET is rejected with 405 and no CORS headers.
            let (listener, resp, body) = send_raw(
                listener,
                b"GET /webhook/deploy?token=my+secret%2B1 HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                Arc::clone(&counter),
                scratch.clone(),
            )
            .await;
            assert!(resp.starts_with("HTTP/1.1 405"), "{resp}");
            assert!(!resp.contains("Access-Control-Allow-"), "{resp}");
            assert!(body.is_none());

            // 2. Saturated counter returns 429.
            counter.store(MAX_CONCURRENT_WEBHOOKS, Ordering::SeqCst);
            let (listener, resp429, _) = send_raw(
                listener,
                b"POST /webhook/deploy HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
                Arc::clone(&counter),
                scratch.clone(),
            )
            .await;
            assert!(resp429.starts_with("HTTP/1.1 429"), "{resp429}");
            counter.store(0, Ordering::SeqCst);

            // 3. POST with percent-encoded query token + trailing extra bytes truncates to content-length.
            let (listener, resp202, cap_body) = send_raw(
                listener,
                b"POST /webhook/deploy?token=my+secret%2B1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nhelloEXTRA".to_vec(),
                Arc::clone(&counter),
                scratch.clone(),
            )
            .await;
            assert!(resp202.starts_with("HTTP/1.1 202"), "{resp202}");
            assert!(!resp202.contains("Access-Control-Allow-"), "{resp202}");
            assert_eq!(cap_body.as_deref(), Some("hello"));
            assert_eq!(counter.load(Ordering::Acquire), 0);

            // 4. Oversized headers (>16 KiB) return 413.
            let big_hdr = format!(
                "POST /webhook/deploy HTTP/1.1\r\nHost: localhost\r\nX-Pad: {}\r\n\r\n",
                "a".repeat(17_000)
            );
            let (_listener, resp413, _) = send_raw(
                listener,
                big_hdr.into_bytes(),
                Arc::clone(&counter),
                scratch.clone(),
            )
            .await;
            assert!(resp413.starts_with("HTTP/1.1 413"), "{resp413}");
        });
    }

    fn cron_wf(id: &str, nodes: Vec<WorkflowNode>) -> Workflow {
        Workflow {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            nodes,
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        }
    }

    fn cron_node(id: &str, cron: &str, tz_offset_minutes: i32) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            kind: NodeKind::TriggerCron,
            label: id.into(),
            x: 0.0,
            y: 0.0,
            config: json!({
                "cron": cron,
                "tz_offset_minutes": tz_offset_minutes
            }),
        }
    }

    #[test]
    fn scheduler_multi_timezone_cron_does_not_double_fire_in_same_minute() {
        let wf = cron_wf(
            "wf-tz",
            vec![
                cron_node("c1", "* * * * *", -180),
                cron_node("c2", "* * * * *", 0),
            ],
        );
        let wfs = vec![wf];
        let last_fired = Mutex::new(HashMap::new());
        let last_checked = Mutex::new(None);

        // 1_700_000_000 is 2023-11-14 22:13:20 UTC; +30s stays in the same UTC minute.
        let t1 = 1_700_000_000i64;
        let first = collect_due_workflows(&wfs, t1, &last_fired, &last_checked);
        assert_eq!(first.len(), 1);

        let second = collect_due_workflows(&wfs, t1 + 30, &last_fired, &last_checked);
        assert!(
            second.is_empty(),
            "expected no duplicate fire in same UTC minute"
        );

        // Next UTC minute fires again once.
        let third = collect_due_workflows(&wfs, t1 + 60, &last_fired, &last_checked);
        assert_eq!(third.len(), 1);
    }

    #[test]
    fn scheduler_catch_up_covers_skipped_minute_and_caps_at_5_minutes() {
        // Align to an exact UTC minute boundary where minute == 4.
        let base_min = 28_333_320i64; // divisible by 60? check modulo
        let aligned_utc_min = (1_700_000_000i64 / 60 / 60) * 60 + 4; // :04 of that hour
        let _ = base_min;
        let wf_five = cron_wf("wf-min5", vec![cron_node("c1", "5 * * * *", 0)]);
        let last_fired = Mutex::new(HashMap::new());
        let last_checked = Mutex::new(None);

        // Tick at :04 -> does not match "5 * * * *".
        let r1 = collect_due_workflows(
            std::slice::from_ref(&wf_five),
            aligned_utc_min * 60 + 10,
            &last_fired,
            &last_checked,
        );
        assert!(r1.is_empty());

        // Tick drifts past :05 and lands at :06 -> catches up :05 without duplicating.
        let r2 = collect_due_workflows(
            std::slice::from_ref(&wf_five),
            (aligned_utc_min + 2) * 60 + 10,
            &last_fired,
            &last_checked,
        );
        assert_eq!(r2.len(), 1);

        // Another tick within :06 does not re-fire :05 or :06.
        let r3 = collect_due_workflows(
            &[wf_five],
            (aligned_utc_min + 2) * 60 + 40,
            &last_fired,
            &last_checked,
        );
        assert!(r3.is_empty());

        // Large drift (10 minutes) is capped at 5 minutes back (+ current minute = 6 runs max).
        let wf_every = cron_wf("wf-every", vec![cron_node("c1", "* * * * *", 0)]);
        let fired2 = Mutex::new(HashMap::new());
        let checked2 = Mutex::new(Some(1000));
        let r4 = collect_due_workflows(&[wf_every], 1010 * 60 + 5, &fired2, &checked2);
        assert_eq!(r4.len(), 6);
    }

    #[test]
    fn merge_agent_system_preserves_custom_node_instructions() {
        let mut agent = zerith_core::agents::AgentDef {
            name: "noc".into(),
            description: String::new(),
            model: None,
            instructions: "NOC specialist".into(),
            path: PathBuf::from("noc.md"),
            layer: "user",
            tools: None,
            disallowed_tools: vec![],
            permission_mode: None,
            max_turns: None,
            skills: vec![],
            isolation: zerith_core::agents::Isolation::None,
            background: false,
            created_by_ai: false,
            disabled: false,
            icon: None,
            quick_prompts: vec![],
        };

        // Identical or fallback default leaves agent instructions untouched.
        merge_agent_system(&mut agent, "NOC specialist");
        assert_eq!(agent.instructions, "NOC specialist");
        merge_agent_system(&mut agent, "You are a helpful automation agent.");
        assert_eq!(agent.instructions, "NOC specialist");

        // Custom node instructions combined in runner.rs are preserved on agent.instructions.
        merge_agent_system(
            &mut agent,
            "NOC specialist\n\nInstruções adicionais do nó:\nOnly report offline OLTs.",
        );
        assert_eq!(
            agent.instructions,
            "NOC specialist\n\nInstruções adicionais do nó:\nOnly report offline OLTs."
        );
    }
}
