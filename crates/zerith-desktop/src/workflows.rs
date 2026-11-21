use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use zerith_core::{Paths, Settings};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_workflows::{
    channels, execute_workflow, now_ms, starter_templates, test_single_node, unix_secs, AiExecutor,
    ChannelConfig, CronSchedule, LocalDateTime, NodeEvent, NodeKind, RunOptions, Store, Workflow,
    WorkflowEdge, WorkflowError, WorkflowNode, WorkflowRun,
};

use crate::state::AppState;

fn open_store(root: &Path) -> Result<Store, String> {
    let st = Store::open(&root.join("workflows.db")).map_err(|e| e.to_string())?;
    st.seed_defaults().map_err(|e| e.to_string())?;
    Ok(st)
}

struct DesktopAiExecutor {
    root: PathBuf,
}

impl AiExecutor for DesktopAiExecutor {
    fn complete(
        &self,
        model: Option<&str>,
        system: &str,
        prompt: &str,
    ) -> zerith_workflows::Result<String> {
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
        let mut last_err = None;
        let mut tried = false;

        for cand in gw.candidates.into_iter().take(3) {
            let model_name = if !cand.model.trim().is_empty() {
                cand.model
            } else {
                cand.config.default_model.clone().unwrap_or_default()
            };
            if model_name.trim().is_empty() {
                continue;
            }
            tried = true;
            let client = match Client::new(cand.config) {
                Ok(c) => c,
                Err(e) => {
                    last_err = Some(zerith_providers::explain::headline(&e));
                    continue;
                }
            };
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

        if !tried {
            return Err(WorkflowError::Ai(
                "Nenhum modelo de IA configurado. Selecione um modelo no nó ou configure uma conta em Ajustes → Contas.".into(),
            ));
        }

        Err(WorkflowError::Ai(last_err.unwrap_or_else(|| {
            "Falha desconhecida ao chamar modelo de IA".into()
        })))
    }
}

/// Runs a periodic background cron scheduler inside the desktop app and binds
/// runtime AI/event hooks for native `workflow_*` chat tools.
pub fn spawn_scheduler(app: AppHandle, root: PathBuf) {
    let hook_app = app.clone();
    zerith_tools::set_workflow_change_hook(Arc::new(move || {
        let _ = hook_app.emit("workflows:changed", ());
    }));
    zerith_tools::set_workflow_ai_executor(Arc::new(DesktopAiExecutor { root: root.clone() }));
    let last_fired: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    tauri::async_runtime::spawn(async move {
        // Give startup a short moment before checking due schedules.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let app_tick = app.clone();
            let root_tick = root.clone();
            let fired_ref = Arc::clone(&last_fired);
            let _ = tauri::async_runtime::spawn_blocking(move || {
                tick_due_workflows(&app_tick, &root_tick, &fired_ref);
            })
            .await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

fn tick_due_workflows(app: &AppHandle, root: &Path, last_fired: &Arc<Mutex<HashMap<String, i64>>>) {
    let Ok(store) = open_store(root) else {
        return;
    };
    let Ok(wfs) = store.list_workflows() else {
        return;
    };
    let now_secs = unix_secs();

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
                let dt = LocalDateTime::from_unix_secs(now_secs, tz);
                if sched.matches_dt(&dt) {
                    let key = dt.minute_key();
                    let already = last_fired
                        .lock()
                        .ok()
                        .is_some_and(|m| m.get(&wf.id) == Some(&key));
                    if !already {
                        if let Ok(mut m) = last_fired.lock() {
                            m.insert(wf.id.clone(), key);
                        }
                        matched_tz = Some(tz);
                        break;
                    }
                }
            }
        }

        if let Some(tz) = matched_tz {
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
                    on_event: &mut emit,
                };
                if let Ok(run) = execute_workflow(&wf, &mut opts) {
                    let _ = app_bg.emit("workflow:done", &run);
                }
            });
        }
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
            on_event: &mut emit,
        };
        let run = execute_workflow(&wf, &mut opts).map_err(|e| e.to_string())?;
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
  "nodes": [
    {{
      "id": "n1",
      "kind": "trigger_cron" | "trigger_manual" | "shell" | "http_request" | "ai_agent" | "condition" | "notify_channel" | "save_file",
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
- shell: {{"command": "...", "timeout_secs": 30}}
- http_request: {{"method": "GET", "url": "https://...", "headers": {{}}, "body": "", "allow_error_status": true}}
- ai_agent: {{"model": "", "system": "...", "prompt": "... {{{{prev}}}}"}}
- condition: {{"mode": "not_empty" | "contains" | "not_contains" | "equals", "pattern": "..."}}
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
            on_event: &mut noop,
        };
        test_single_node(&node, &sample, &opts).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
