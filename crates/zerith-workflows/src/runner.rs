use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::model::{
    NodeEvent, NodeKind, NodeRunLog, Workflow, WorkflowEdge, WorkflowNode, WorkflowRun,
};
use crate::schedule::LocalDateTime;
use crate::store::Store;
use crate::{channels, ssh};
use crate::{now_ms, unix_secs, Result, WorkflowError};

pub trait AiExecutor: Send + Sync {
    fn complete(&self, model: Option<&str>, system: &str, prompt: &str) -> Result<String>;

    /// Full bounded agent turn with skills, plugins, MCP tools and memory.
    /// Defaults to [`AiExecutor::complete`] for lightweight executors/tests.
    fn complete_agent_loop(
        &self,
        _bot_name: Option<&str>,
        model: Option<&str>,
        system: &str,
        prompt: &str,
        _max_turns: usize,
        _tool_mode: &str,
    ) -> Result<String> {
        self.complete(model, system, prompt)
    }

    /// Optional specialist bot lookup returning `(preferred_model, instructions)`.
    fn resolve_bot(&self, _bot_name: &str) -> Option<(Option<String>, String)> {
        None
    }
}

pub struct NoopAiExecutor;

impl AiExecutor for NoopAiExecutor {
    fn complete(&self, _model: Option<&str>, _system: &str, prompt: &str) -> Result<String> {
        Ok(format!("[AI mock]\n{prompt}"))
    }
}

pub struct RunOptions<'a> {
    pub store: &'a Store,
    pub data_root: &'a Path,
    pub ai: &'a dyn AiExecutor,
    pub trigger_source: &'a str,
    pub tz_offset_minutes: i32,
    pub initial_input: Option<&'a str>,
    pub on_event: &'a mut (dyn FnMut(NodeEvent) + Send),
}

static RUN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn execute_workflow(wf: &Workflow, opts: &mut RunOptions<'_>) -> Result<WorkflowRun> {
    let started_at_ms = now_ms();
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("run-{}-{}-{}", wf.id, started_at_ms, seq);
    opts.store
        .record_run_start(&run_id, &wf.id, opts.trigger_source)?;

    let nodes_map: HashMap<&str, &WorkflowNode> =
        wf.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    if nodes_map.is_empty() {
        let err_msg = "workflow não possui nós".to_string();
        opts.store
            .record_run_finish(&run_id, &wf.id, "error", &err_msg, &[])?;
        return Err(WorkflowError::Exec(err_msg));
    }

    let mut outgoing: HashMap<&str, Vec<&WorkflowEdge>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    for n in &wf.nodes {
        indegree.insert(n.id.as_str(), 0);
    }
    for e in &wf.edges {
        if nodes_map.contains_key(e.from.as_str()) && nodes_map.contains_key(e.to.as_str()) {
            outgoing.entry(e.from.as_str()).or_default().push(e);
            *indegree.entry(e.to.as_str()).or_insert(0) += 1;
        }
    }

    // Every indegree-0 node enters the queue (keeps the topological walk and
    // cycle check intact), but only triggers matching this run's source are
    // active. Workflows without trigger nodes keep the legacy "all roots" rule.
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut scheduled: HashSet<&str> = HashSet::new();
    for n in &wf.nodes {
        if indegree.get(n.id.as_str()).copied().unwrap_or(0) == 0 {
            queue.push_back(n.id.as_str());
            scheduled.insert(n.id.as_str());
        }
    }
    let has_triggers = wf.nodes.iter().any(|n| is_trigger(n.kind));

    let mut outputs: HashMap<String, String> = HashMap::new();
    let mut active_nodes: HashSet<String> = queue
        .iter()
        .filter(|id| {
            !has_triggers
                || nodes_map
                    .get(*id)
                    .is_some_and(|n| trigger_fires(n.kind, opts.trigger_source))
        })
        .map(|s| (*s).to_string())
        .collect();
    // Dedup keys are committed only when the whole run succeeds.
    let mut pending_seen: Vec<(String, Vec<String>)> = Vec::new();
    let mut last_incoming: HashMap<String, String> = HashMap::new();
    let mut logs: Vec<NodeRunLog> = Vec::new();
    let mut overall_status = "ok".to_string();
    let mut summary = String::new();

    let now_dt = LocalDateTime::from_unix_secs(unix_secs(), opts.tz_offset_minutes);

    while let Some(node_id) = queue.pop_front() {
        let Some(node) = nodes_map.get(node_id).copied() else {
            continue;
        };
        if !active_nodes.contains(node_id) {
            logs.push(NodeRunLog {
                node_id: node.id.clone(),
                label: node.label.clone(),
                kind: node.kind,
                status: "skipped".into(),
                output: "ramo não ativado".into(),
                duration_ms: 0,
            });
            (opts.on_event)(NodeEvent {
                workflow_id: wf.id.clone(),
                run_id: run_id.clone(),
                node_id: node.id.clone(),
                status: "skipped".into(),
                output: String::new(),
            });
            enqueue_children(
                node_id,
                &outgoing,
                &mut indegree,
                &mut queue,
                &mut scheduled,
            );
            continue;
        }

        (opts.on_event)(NodeEvent {
            workflow_id: wf.id.clone(),
            run_id: run_id.clone(),
            node_id: node.id.clone(),
            status: "running".into(),
            output: String::new(),
        });

        let prev = last_incoming.get(node_id).cloned().unwrap_or_default();
        let t0 = Instant::now();
        let res = run_node_with_controls(node, &prev, &outputs, &now_dt, opts);
        let duration_ms = t0.elapsed().as_millis() as u64;

        match res {
            Ok((out, branch)) => {
                if let Some(scope) = dedup_scope(node, &prev, &outputs, &now_dt) {
                    pending_seen.push((scope, out.lines().map(str::to_string).collect()));
                }
                outputs.insert(node.id.clone(), out.clone());
                if node.kind == NodeKind::Transform
                    && node.config.get("mode").and_then(|v| v.as_str()) == Some("dedup_lines")
                {
                    // Deduplication overrides upstream summary so already-seen items don't leak forward.
                    summary = out.clone();
                } else if !out.is_empty() {
                    summary = out.clone();
                }
                logs.push(NodeRunLog {
                    node_id: node.id.clone(),
                    label: node.label.clone(),
                    kind: node.kind,
                    status: "ok".into(),
                    output: out.clone(),
                    duration_ms,
                });
                (opts.on_event)(NodeEvent {
                    workflow_id: wf.id.clone(),
                    run_id: run_id.clone(),
                    node_id: node.id.clone(),
                    status: "ok".into(),
                    output: out.clone(),
                });

                if let Some(children) = outgoing.get(node_id) {
                    for edge in children {
                        let edge_matches = match (&branch, &edge.branch) {
                            (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
                            (Some(_), None) => true,
                            (None, Some(expected)) => expected.eq_ignore_ascii_case("true"),
                            (None, None) => true,
                        };
                        if edge_matches {
                            active_nodes.insert(edge.to.clone());
                            last_incoming.insert(edge.to.clone(), out.clone());
                        }
                    }
                }
                enqueue_children(
                    node_id,
                    &outgoing,
                    &mut indegree,
                    &mut queue,
                    &mut scheduled,
                );
            }
            Err(err) => {
                let err_str = err.to_string();
                overall_status = "error".into();
                summary = format!("Erro no nó '{}': {err_str}", node.label);
                logs.push(NodeRunLog {
                    node_id: node.id.clone(),
                    label: node.label.clone(),
                    kind: node.kind,
                    status: "error".into(),
                    output: err_str.clone(),
                    duration_ms,
                });
                (opts.on_event)(NodeEvent {
                    workflow_id: wf.id.clone(),
                    run_id: run_id.clone(),
                    node_id: node.id.clone(),
                    status: "error".into(),
                    output: err_str,
                });
                break;
            }
        }
    }

    if overall_status == "ok"
        && (scheduled.len() < nodes_map.len() || indegree.values().any(|&d| d > 0))
    {
        overall_status = "error".into();
        summary = "ciclo detectado no grafo do workflow".into();
    }

    if overall_status == "ok" {
        for (scope, items) in &pending_seen {
            opts.store.mark_items_seen(scope, items)?;
        }
    }

    opts.store
        .record_run_finish(&run_id, &wf.id, &overall_status, &summary, &logs)?;

    Ok(WorkflowRun {
        id: run_id,
        workflow_id: wf.id.clone(),
        trigger_source: opts.trigger_source.to_string(),
        status: overall_status,
        started_at_ms,
        finished_at_ms: Some(now_ms()),
        summary,
        node_logs: logs,
    })
}

fn is_trigger(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::TriggerCron | NodeKind::TriggerManual | NodeKind::TriggerWebhook
    )
}

/// Which trigger nodes a run started by `source` activates. Manual-like
/// sources (manual, ai_tool, test) fire every trigger.
fn trigger_fires(kind: NodeKind, source: &str) -> bool {
    match source {
        "cron" => kind == NodeKind::TriggerCron,
        "webhook" => kind == NodeKind::TriggerWebhook,
        _ => is_trigger(kind),
    }
}

/// Scope of a non-pinned `dedup_lines` transform, or `None` for other nodes.
fn dedup_scope(
    node: &WorkflowNode,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
) -> Option<String> {
    let cfg = &node.config;
    if node.kind != NodeKind::Transform
        || cfg_str(cfg, "mode") != Some("dedup_lines")
        || cfg.get("pin_enabled").and_then(|v| v.as_bool()) == Some(true)
    {
        return None;
    }
    Some(
        cfg_str(cfg, "pattern")
            .map(|p| interpolate(p, prev, outputs, now_dt))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("node:{}", node.id)),
    )
}

fn enqueue_children<'a>(
    node_id: &str,
    outgoing: &HashMap<&'a str, Vec<&'a WorkflowEdge>>,
    indegree: &mut HashMap<&'a str, usize>,
    queue: &mut VecDeque<&'a str>,
    scheduled: &mut HashSet<&'a str>,
) {
    let Some(edges) = outgoing.get(node_id) else {
        return;
    };
    for e in edges {
        let target = e.to.as_str();
        if let Some(deg) = indegree.get_mut(target) {
            *deg = deg.saturating_sub(1);
            if *deg == 0 && !scheduled.contains(target) {
                scheduled.insert(target);
                queue.push_back(target);
            }
        }
    }
}

fn run_node_with_controls(
    node: &WorkflowNode,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    opts: &RunOptions<'_>,
) -> Result<(String, Option<String>)> {
    let cfg = &node.config;
    // Pin / Mock output bypasses real external execution for safe downstream testing.
    if cfg.get("pin_enabled").and_then(|v| v.as_bool()) == Some(true) {
        let pinned = cfg
            .get("pinned_output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Ok((pinned, None));
    }

    let retry_count = cfg
        .get("retry_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .clamp(0, 5);
    let retry_delay_secs = cfg
        .get("retry_delay_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .clamp(1, 60);

    let mut attempt = 0u64;
    loop {
        match run_node_masked(node, prev, outputs, now_dt, opts) {
            Ok(ok) => return Ok(ok),
            Err(err) if attempt < retry_count => {
                attempt += 1;
                std::thread::sleep(Duration::from_secs(retry_delay_secs));
                let _ = err;
            }
            Err(err) => return Err(err),
        }
    }
}

struct HttpCall<'a> {
    cfg: &'a Value,
    method: &'a str,
    url: &'a str,
    body: &'a str,
    allow_err: bool,
    prev: &'a str,
    outputs: &'a HashMap<String, String>,
    now_dt: &'a LocalDateTime,
}

fn execute_http(call: HttpCall<'_>) -> Result<String> {
    let mut req = ureq::request(call.method, call.url).timeout(Duration::from_secs(30));
    if let Some(obj) = call.cfg.get("headers").and_then(|h| h.as_object()) {
        for (k, v) in obj {
            if let Some(vs) = v.as_str() {
                req = req.set(k, &interpolate(vs, call.prev, call.outputs, call.now_dt));
            }
        }
    }
    let resp = if matches!(call.method, "POST" | "PUT" | "PATCH") {
        if call
            .cfg
            .get("headers")
            .and_then(|h| h.get("Content-Type"))
            .is_none()
        {
            req = req.set("Content-Type", "application/json");
        }
        req.send_string(call.body)
    } else {
        req.call()
    };
    match resp {
        Ok(r) => {
            let status = r.status();
            let text = r.into_string().unwrap_or_default();
            let clipped = clip_text(&text, 12_000);
            Ok(format!("HTTP {status}\n{clipped}"))
        }
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let clipped = clip_text(&text, 4000);
            let formatted = format!("HTTP {code}\n{clipped}");
            if call.allow_err {
                Ok(formatted)
            } else {
                Err(WorkflowError::Exec(formatted))
            }
        }
        Err(e) => {
            if call.allow_err {
                Ok(format!("HTTP ERROR: {e}"))
            } else {
                Err(WorkflowError::Exec(e.to_string()))
            }
        }
    }
}

fn parse_loop_items(
    raw_items: Option<&str>,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
) -> Vec<String> {
    let source = match raw_items {
        Some(raw) if !raw.is_empty() => interpolate(raw, prev, outputs, now_dt),
        _ => prev.to_string(),
    };
    let trimmed = source.trim();
    if trimmed.starts_with('[') {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(trimmed) {
            return arr
                .into_iter()
                .take(50)
                .map(|v| match v {
                    Value::String(s) => s,
                    other => other.to_string(),
                })
                .filter(|s| !s.trim().is_empty())
                .collect();
        }
    }
    trimmed
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(50)
        .map(str::to_string)
        .collect()
}

fn run_node(
    node: &WorkflowNode,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    opts: &RunOptions<'_>,
) -> Result<(String, Option<String>)> {
    let cfg = &node.config;
    match node.kind {
        NodeKind::TriggerCron | NodeKind::TriggerManual => Ok((
            format!(
                "Disparado ({}) em {} {}",
                opts.trigger_source,
                now_dt.date_string(),
                now_dt.time_string()
            ),
            None,
        )),
        NodeKind::TriggerWebhook => {
            if let Some(payload) = opts.initial_input.map(str::trim).filter(|s| !s.is_empty()) {
                Ok((payload.to_string(), None))
            } else {
                Ok((
                    format!(
                        "Webhook disparado ({}) em {} {}",
                        opts.trigger_source,
                        now_dt.date_string(),
                        now_dt.time_string()
                    ),
                    None,
                ))
            }
        }
        NodeKind::Shell => {
            let raw_cmd = cfg_str(cfg, "command").unwrap_or("");
            if raw_cmd.is_empty() {
                return Err(WorkflowError::Exec("comando shell vazio".into()));
            }
            let (cmd, env) = interpolate_for_shell(raw_cmd, prev, outputs, now_dt, None);
            let timeout = cfg
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .clamp(1, 600);
            let workdir = cfg_str(cfg, "cwd").map(PathBuf::from);
            let out =
                run_shell_command(&cmd, workdir.as_deref(), Duration::from_secs(timeout), &env)?;
            Ok((out, None))
        }
        NodeKind::SshCommand => {
            let t = ssh::target(cfg, opts.data_root)?;
            let raw_cmd = cfg_str(cfg, "command")
                .ok_or_else(|| WorkflowError::Exec("comando SSH vazio".into()))?;
            // The remote shell parses this string: dynamic values are POSIX-quoted.
            let cmd = interpolate_inner(raw_cmd, prev, outputs, now_dt, None, &mut posix_quote);
            let timeout = cfg
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .clamp(1, 300);
            let args = ssh::args(&t, &cmd);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let timeout = Duration::from_secs(timeout);
            let out = match t.password_secret.as_deref() {
                Some(name) => {
                    let pw = secret_value(name)?;
                    let askpass = ssh::AskPass::create()?;
                    run_direct_command("ssh", &args, &askpass.env(&pw), timeout)?
                }
                None => run_direct_command("ssh", &args, &[], timeout)?,
            };
            Ok((out, None))
        }
        NodeKind::HttpRequest => {
            let url_raw = cfg_str(cfg, "url").unwrap_or("");
            if url_raw.is_empty() {
                return Err(WorkflowError::Exec("URL HTTP vazia".into()));
            }
            let url = interpolate(url_raw, prev, outputs, now_dt);
            let method = cfg_str(cfg, "method").unwrap_or("GET").to_ascii_uppercase();
            let body_raw = cfg_str(cfg, "body").unwrap_or("");
            let body = interpolate(body_raw, prev, outputs, now_dt);
            let allow_err = cfg
                .get("allow_error_status")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let out = execute_http(HttpCall {
                cfg,
                method: &method,
                url: &url,
                body: &body,
                allow_err,
                prev,
                outputs,
                now_dt,
            })?;
            Ok((out, None))
        }
        NodeKind::AiAgent => {
            let explicit_model = cfg_str(cfg, "model");
            let bot_name = cfg_str(cfg, "bot_name");
            let bot_meta = bot_name.and_then(|name| opts.ai.resolve_bot(name));
            let resolved_model = explicit_model
                .map(str::to_string)
                .or_else(|| bot_meta.as_ref().and_then(|(m, _)| m.clone()));
            let node_sys = cfg_str(cfg, "system").unwrap_or("");
            let combined_sys = match (bot_meta.as_ref().map(|(_, i)| i.trim()), node_sys.trim()) {
                (Some(bot_inst), "") if !bot_inst.is_empty() => bot_inst.to_string(),
                (Some(bot_inst), custom) if !bot_inst.is_empty() => {
                    format!("{bot_inst}\n\nInstruções adicionais do nó:\n{custom}")
                }
                (_, "") => "You are a helpful automation agent.".to_string(),
                (_, custom) => custom.to_string(),
            };
            let prompt_raw = cfg_str(cfg, "prompt").unwrap_or("{{prev}}");
            let system = interpolate(&combined_sys, prev, outputs, now_dt);
            let prompt = interpolate(prompt_raw, prev, outputs, now_dt);
            let use_tools = cfg
                .get("use_tools")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let out = if use_tools {
                let max_turns = cfg
                    .get("max_turns")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(6)
                    .clamp(1, 20) as usize;
                let tool_mode = cfg_str(cfg, "tool_mode").unwrap_or("safe");
                opts.ai.complete_agent_loop(
                    bot_name,
                    resolved_model.as_deref(),
                    &system,
                    &prompt,
                    max_turns,
                    tool_mode,
                )?
            } else {
                opts.ai
                    .complete(resolved_model.as_deref(), &system, &prompt)?
            };
            Ok((out, None))
        }
        NodeKind::Condition => {
            let mode = cfg_str(cfg, "mode").unwrap_or("not_empty");
            let pattern = interpolate(cfg_str(cfg, "pattern").unwrap_or(""), prev, outputs, now_dt);
            let passed = match mode {
                "contains" => {
                    !pattern.is_empty() && prev.to_lowercase().contains(&pattern.to_lowercase())
                }
                "not_contains" => {
                    pattern.is_empty() || !prev.to_lowercase().contains(&pattern.to_lowercase())
                }
                "equals" => prev.trim() == pattern.trim(),
                _ => !prev.trim().is_empty(),
            };
            let branch = if passed { "true" } else { "false" }.to_string();
            Ok((prev.to_string(), Some(branch)))
        }
        NodeKind::Transform => {
            let mode = cfg_str(cfg, "mode").unwrap_or("template");
            let out = match mode {
                "json_path" => {
                    let path = cfg_str(cfg, "pattern").unwrap_or("");
                    extract_json_path(prev, path)?
                }
                "filter_lines" => {
                    let pat =
                        interpolate(cfg_str(cfg, "pattern").unwrap_or(""), prev, outputs, now_dt)
                            .to_lowercase();
                    prev.lines()
                        .filter(|l| pat.is_empty() || l.to_lowercase().contains(&pat))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                "dedup_lines" => {
                    let scope = dedup_scope(node, prev, outputs, now_dt).unwrap_or_default();
                    let lines: Vec<String> = prev
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect();
                    opts.store.unseen_items(&scope, &lines)?.join("\n")
                }
                _ => {
                    let tpl = cfg_str(cfg, "template").unwrap_or("{{prev}}");
                    interpolate(tpl, prev, outputs, now_dt)
                }
            };
            Ok((out, None))
        }
        NodeKind::Delay => {
            let secs = cfg
                .get("seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 300);
            std::thread::sleep(Duration::from_secs(secs));
            Ok((prev.to_string(), None))
        }
        NodeKind::Loop => {
            let items = parse_loop_items(cfg_str(cfg, "items"), prev, outputs, now_dt);
            if items.is_empty() {
                return Ok((String::new(), None));
            }
            let mode = cfg_str(cfg, "mode").unwrap_or("template");
            let sep = cfg
                .get("separator")
                .and_then(|v| v.as_str())
                .unwrap_or("\n");
            let mut results = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let idx_str = idx.to_string();
                let res = match mode {
                    "shell" => {
                        let cmd_tpl = cfg_str(cfg, "command").unwrap_or("echo {{item}}");
                        let (cmd, env) = interpolate_for_shell(
                            cmd_tpl,
                            prev,
                            outputs,
                            now_dt,
                            Some((item, &idx_str)),
                        );
                        let out = run_shell_command(&cmd, None, Duration::from_secs(30), &env)?;
                        format!("{item}: {out}")
                    }
                    "http" => {
                        let url_tpl = cfg_str(cfg, "url").unwrap_or("");
                        if url_tpl.is_empty() {
                            return Err(WorkflowError::Exec("URL HTTP vazia no nó Loop".into()));
                        }
                        let url = interpolate_inner(
                            url_tpl,
                            prev,
                            outputs,
                            now_dt,
                            Some((item, &idx_str)),
                            &mut raw_value,
                        );
                        let method = cfg_str(cfg, "method").unwrap_or("GET").to_ascii_uppercase();
                        let body = interpolate_inner(
                            cfg_str(cfg, "body").unwrap_or(""),
                            prev,
                            outputs,
                            now_dt,
                            Some((item, &idx_str)),
                            &mut raw_value,
                        );
                        let out = execute_http(HttpCall {
                            cfg,
                            method: &method,
                            url: &url,
                            body: &body,
                            allow_err: true,
                            prev,
                            outputs,
                            now_dt,
                        })?;
                        format!("{item}: {out}")
                    }
                    _ => {
                        let tpl = cfg_str(cfg, "template").unwrap_or("{{item}}");
                        interpolate_inner(
                            tpl,
                            prev,
                            outputs,
                            now_dt,
                            Some((item, &idx_str)),
                            &mut raw_value,
                        )
                    }
                };
                results.push(res);
            }
            Ok((results.join(sep), None))
        }
        NodeKind::NotifyChannel => {
            let channel_id = cfg_str(cfg, "channel_id").ok_or_else(|| {
                WorkflowError::Channel(
                    "selecione um canal de notificação configurado neste nó".into(),
                )
            })?;
            let ch = opts.store.get_channel(channel_id)?.ok_or_else(|| {
                WorkflowError::Channel(format!("canal '{channel_id}' não encontrado"))
            })?;
            let target = cfg_str(cfg, "target").map(|t| interpolate(t, prev, outputs, now_dt));
            let msg_tpl = cfg_str(cfg, "message").unwrap_or("{{prev}}");
            let msg = interpolate(msg_tpl, prev, outputs, now_dt);
            let attachment = notify_attachment(cfg, prev, outputs, now_dt, opts)?;
            let res = channels::dispatch_with(&ch, target.as_deref(), &msg, attachment.as_ref())?;
            Ok((res, None))
        }
        NodeKind::SaveFile => {
            let rel_raw = cfg_str(cfg, "path").unwrap_or("reports/workflow-{{date}}.md");
            // Upstream values cannot inject separators or `..` into the path.
            let rel = interpolate_inner(rel_raw, prev, outputs, now_dt, None, &mut path_segment);
            let content = cfg_str(cfg, "content")
                .map(|c| interpolate(c, prev, outputs, now_dt))
                .unwrap_or_else(|| prev.to_string());
            let target = {
                let p = PathBuf::from(&rel);
                if p.is_absolute() {
                    p
                } else {
                    opts.data_root.join(p)
                }
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let append = cfg.get("append").and_then(|v| v.as_bool()).unwrap_or(false);
            if append {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&target)?;
                writeln!(f, "{content}")?;
            } else {
                std::fs::write(&target, &content)?;
            }
            Ok((format!("{SAVED_PREFIX}{}", target.display()), None))
        }
    }
}

// ---- Segredos `{{secret:nome}}` --------------------------------------------

thread_local! {
    /// Valores de segredo lidos durante o nó atual (para mascarar a saída) e o
    /// primeiro `{{secret:x}}` que faltou no cofre.
    static SECRETS_SEEN: RefCell<(Vec<String>, Option<String>)> =
        const { RefCell::new((Vec::new(), None)) };
}

#[cfg(test)]
thread_local! {
    /// Cofre de teste: os testes nunca tocam o cofre real.
    static TEST_VAULT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

const SECRET_MASK: &str = "***";

/// Lê `name` no cofre configurado (`zerith_core::secrets`; no servidor, o
/// backend de arquivo) e guarda o valor para mascarar a saída do nó.
pub(crate) fn secret_value(name: &str) -> Result<String> {
    #[cfg(test)]
    let got = TEST_VAULT.with(|v| {
        v.borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("segredo '{name}' não encontrado no cofre"))
    });
    #[cfg(not(test))]
    let got = zerith_core::secrets::get_secret(name).map_err(|e| e.to_string());
    let v = got.map_err(|e| WorkflowError::Exec(format!("{e}{}", secret_hint(name))))?;
    if !v.is_empty() {
        SECRETS_SEEN.with(|s| s.borrow_mut().0.push(v.clone()));
    }
    Ok(v)
}

fn secret_hint(name: &str) -> String {
    format!(" (grave no servidor: zerith secret set {name}, ou pela tela do desktop)")
}

/// `{{secret:nome}}` na interpolação: o valor, ou `None` (o token fica literal
/// e o nó falha em [`run_node_masked`]).
fn scoped_secret(name: &str) -> Option<String> {
    let name = name.trim();
    match secret_value(name) {
        Ok(v) => Some(v),
        Err(_) => {
            SECRETS_SEEN.with(|s| {
                s.borrow_mut().1.get_or_insert_with(|| name.to_string());
            });
            None
        }
    }
}

/// Troca cada valor de segredo por `***` (o mais longo primeiro).
fn mask_secrets(text: &str, values: &[String]) -> String {
    let mut vals: Vec<&String> = values.iter().filter(|v| !v.is_empty()).collect();
    vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let mut out = text.to_string();
    for v in vals {
        out = out.replace(v.as_str(), SECRET_MASK);
    }
    out
}

/// [`run_node`] com os segredos lidos no nó mascarados na saída e no erro —
/// é isso que vai para log, histórico, eventos e resumo.
fn run_node_masked(
    node: &WorkflowNode,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    opts: &RunOptions<'_>,
) -> Result<(String, Option<String>)> {
    SECRETS_SEEN.with(|s| *s.borrow_mut() = (Vec::new(), None));
    let res = run_node(node, prev, outputs, now_dt, opts);
    let (seen, missing) = SECRETS_SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if let Some(name) = missing {
        return Err(WorkflowError::Exec(format!(
            "segredo '{name}' não encontrado no cofre{}",
            secret_hint(&name)
        )));
    }
    match res {
        Ok((out, branch)) => Ok((mask_secrets(&out, &seen), branch)),
        Err(e) if seen.is_empty() => Err(e),
        Err(WorkflowError::Exec(m)) => Err(WorkflowError::Exec(mask_secrets(&m, &seen))),
        Err(WorkflowError::Channel(m)) => Err(WorkflowError::Channel(mask_secrets(&m, &seen))),
        Err(WorkflowError::Ai(m)) => Err(WorkflowError::Ai(mask_secrets(&m, &seen))),
        Err(e) => Err(WorkflowError::Exec(mask_secrets(&e.to_string(), &seen))),
    }
}

/// Início da saída do nó SaveFile; o resto é o caminho gravado (lido pelo
/// anexo do NotifyChannel).
const SAVED_PREFIX: &str = "Arquivo salvo em ";

/// Anexo do NotifyChannel: `attach_node` (id de um nó SaveFile já executado)
/// ou `attachment` (caminho, relativo à pasta de dados, ou URL http(s)).
/// Valores interpolados no caminho não podem trocar de pasta.
fn notify_attachment(
    cfg: &Value,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    opts: &RunOptions<'_>,
) -> Result<Option<channels::Attachment>> {
    if let Some(id) = cfg_str(cfg, "attach_node") {
        let out = outputs.get(id).ok_or_else(|| {
            WorkflowError::Exec(format!("anexo: o nó '{id}' ainda não rodou nesta execução"))
        })?;
        let path = out.strip_prefix(SAVED_PREFIX).ok_or_else(|| {
            WorkflowError::Exec(format!("anexo: o nó '{id}' não é um nó Salvar arquivo"))
        })?;
        return channels::Attachment::from_path(Path::new(path.trim())).map(Some);
    }
    let Some(raw) = cfg_str(cfg, "attachment") else {
        return Ok(None);
    };
    if raw.starts_with("http://") || raw.starts_with("https://") {
        let url = interpolate_inner(raw, prev, outputs, now_dt, None, &mut |v| {
            v.chars().filter(|c| !c.is_whitespace()).collect()
        });
        return Ok(Some(channels::Attachment::from_url(&url)));
    }
    let rel = interpolate_inner(raw, prev, outputs, now_dt, None, &mut path_segment);
    let p = PathBuf::from(&rel);
    let path = if p.is_absolute() {
        p
    } else {
        opts.data_root.join(p)
    };
    channels::Attachment::from_path(&path).map(Some)
}

/// Tests a single workflow node with optional sample upstream input (`prev`)
/// without recording a full workflow run.
pub fn test_single_node(
    node: &WorkflowNode,
    sample_input: &str,
    opts: &RunOptions<'_>,
) -> Result<String> {
    let now_dt = LocalDateTime::from_unix_secs(unix_secs(), opts.tz_offset_minutes);
    let outputs = HashMap::new();
    let (out, branch) = run_node_masked(node, sample_input, &outputs, &now_dt, opts)?;
    if let Some(scope) = dedup_scope(node, sample_input, &outputs, &now_dt) {
        let items: Vec<String> = out.lines().map(str::to_string).collect();
        opts.store.mark_items_seen(&scope, &items)?;
    }
    if let Some(b) = branch {
        Ok(format!("[branch={b}]\n{out}"))
    } else {
        Ok(out)
    }
}

fn extract_json_path(raw: &str, path: &str) -> Result<String> {
    // Allow direct piping from HttpRequest nodes ("HTTP 200\n{...}").
    let trimmed = raw.trim();
    let json_text = if let Some(rest) = trimmed.strip_prefix("HTTP ") {
        rest.split_once('\n')
            .map(|(_, b)| b.trim())
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    let mut cur: Value = serde_json::from_str(json_text).map_err(|e| {
        WorkflowError::Exec(format!("entrada não é JSON válido para json_path: {e}"))
    })?;
    for seg in path.split('.').map(str::trim).filter(|s| !s.is_empty()) {
        if let Ok(idx) = seg.parse::<usize>() {
            cur = cur.get(idx).cloned().ok_or_else(|| {
                WorkflowError::Exec(format!("índice '{idx}' não encontrado em JSON"))
            })?;
        } else {
            cur = cur.get(seg).cloned().ok_or_else(|| {
                WorkflowError::Exec(format!("campo '{seg}' não encontrado em JSON"))
            })?;
        }
    }
    Ok(match cur {
        Value::String(s) => s,
        other => serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string()),
    })
}

fn run_direct_command(
    bin: &str,
    args: &[&str],
    env: &[(&str, String)],
    timeout: Duration,
) -> Result<String> {
    let mut c = Command::new(bin);
    c.args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    wait_command_with_timeout(c, timeout)
}

const MAX_PIPE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct PipeState {
    bytes: Vec<u8>,
    done: bool,
}

struct PipeReader {
    pair: Arc<(Mutex<PipeState>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

struct MarkPipeDone(Arc<(Mutex<PipeState>, Condvar)>);

impl Drop for MarkPipeDone {
    fn drop(&mut self) {
        if let Ok(mut st) = self.0 .0.lock() {
            st.done = true;
        }
        self.0 .1.notify_all();
    }
}

fn spawn_pipe_reader<R: Read + Send + 'static>(pipe: Option<R>) -> PipeReader {
    let Some(mut reader) = pipe else {
        let pair = Arc::new((
            Mutex::new(PipeState {
                bytes: Vec::new(),
                done: true,
            }),
            Condvar::new(),
        ));
        return PipeReader { pair, handle: None };
    };
    let pair = Arc::new((Mutex::new(PipeState::default()), Condvar::new()));
    let worker_pair = Arc::clone(&pair);
    let handle = std::thread::spawn(move || {
        let _done = MarkPipeDone(Arc::clone(&worker_pair));
        let mut buf = [0u8; 8192];
        let mut kept = 0usize;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if kept < MAX_PIPE_BYTES {
                        let take = n.min(MAX_PIPE_BYTES - kept);
                        if let Ok(mut st) = worker_pair.0.lock() {
                            st.bytes.extend_from_slice(&buf[..take]);
                        }
                        kept += take;
                    }
                }
            }
        }
    });
    PipeReader {
        pair,
        handle: Some(handle),
    }
}

impl PipeReader {
    fn collect_until(self, deadline: Instant) -> Vec<u8> {
        let bytes = if let Ok(guard) = self.pair.0.lock() {
            let mut guard = guard;
            while !guard.done {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, res) = self
                    .pair
                    .1
                    .wait_timeout(guard, remaining)
                    .unwrap_or_else(|e| e.into_inner());
                guard = next;
                if res.timed_out() {
                    break;
                }
            }
            let done = guard.done;
            let bytes = std::mem::take(&mut guard.bytes);
            drop(guard);
            if done {
                if let Some(h) = self.handle {
                    let _ = h.join();
                }
            }
            bytes
        } else {
            Vec::new()
        };
        bytes
    }
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    let err = String::from_utf8_lossy(stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    text
}

enum CommandWait {
    Exited(std::process::ExitStatus, Vec<u8>, Vec<u8>),
    TimedOut(Vec<u8>, Vec<u8>),
}

fn wait_command_raw(mut c: Command, timeout: Duration) -> Result<CommandWait> {
    let mut child = c.spawn()?;
    let stdout_reader = spawn_pipe_reader(child.stdout.take());
    let stderr_reader = spawn_pipe_reader(child.stderr.take());
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let deadline = Instant::now() + Duration::from_millis(750);
            let out = stdout_reader.collect_until(deadline);
            let err = stderr_reader.collect_until(deadline);
            return Ok(CommandWait::Exited(status, out, err));
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let deadline = Instant::now() + Duration::from_millis(250);
            let out = stdout_reader.collect_until(deadline);
            let err = stderr_reader.collect_until(deadline);
            return Ok(CommandWait::TimedOut(out, err));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_command_with_timeout(c: Command, timeout: Duration) -> Result<String> {
    match wait_command_raw(c, timeout)? {
        CommandWait::Exited(status, out, err) => {
            let text = combine_output(&out, &err);
            if status.success() {
                return Ok(clip_text(text.trim(), 16_000));
            }
            Err(WorkflowError::Exec(format!(
                "código {:?}: {}",
                status.code(),
                clip_text(text.trim(), 2000)
            )))
        }
        CommandWait::TimedOut(out, err) => {
            let msg = format!("comando expirou após {}s", timeout.as_secs());
            let text = combine_output(&out, &err);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Err(WorkflowError::Exec(msg))
            } else {
                Err(WorkflowError::Exec(format!(
                    "{msg}: {}",
                    clip_text(trimmed, 2000)
                )))
            }
        }
    }
}

fn interpolate_inner(
    tpl: &str,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    loop_vars: Option<(&str, &str)>,
    esc: &mut dyn FnMut(&str) -> String,
) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        if rest[start..].starts_with("{{{") {
            out.push('{');
            rest = &rest[start + 1..];
            continue;
        }
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        if let Some(nested) = after_open[..end].find("{{") {
            out.push_str(&rest[start..start + 2 + nested]);
            rest = &after_open[nested..];
            continue;
        }
        let token = &after_open[..end];
        match token {
            "prev" => out.push_str(&esc(prev)),
            "date" => out.push_str(&now_dt.date_string()),
            "time" => out.push_str(&now_dt.time_string()),
            "item" => {
                if let Some((item, _)) = loop_vars {
                    out.push_str(&esc(item));
                } else {
                    out.push_str(&rest[start..start + 2 + end + 2]);
                }
            }
            "index" => {
                if let Some((_, idx)) = loop_vars {
                    out.push_str(idx);
                } else {
                    out.push_str(&rest[start..start + 2 + end + 2]);
                }
            }
            _ => {
                if let Some(id) = token
                    .strip_prefix("node.")
                    .filter(|id| outputs.contains_key(*id))
                {
                    out.push_str(&esc(&outputs[id]));
                } else if let Some(v) = token.strip_prefix("secret:").and_then(scoped_secret) {
                    out.push_str(&esc(&v));
                } else {
                    out.push_str(&rest[start..start + 2 + end + 2]);
                }
            }
        }
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

pub fn interpolate(
    tpl: &str,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
) -> String {
    interpolate_inner(tpl, prev, outputs, now_dt, None, &mut raw_value)
}

fn raw_value(v: &str) -> String {
    v.to_string()
}

/// POSIX single-quoting: safe inside `sh -c` / remote SSH command lines.
fn posix_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', r"'\''"))
}

/// Dynamic value used inside a file path: no separators, drive colons, `..`
/// or control characters, so it cannot leave the directory the template chose.
fn path_segment(v: &str) -> String {
    let cleaned: String = v
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').replace("..", "_");
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Interpolates a local shell command without splicing upstream data into it:
/// each dynamic value goes into an env var (`ZERITH_V<n>`) and the command gets
/// a reference (`"$ZERITH_V0"` on sh, `!ZERITH_V0!` with delayed expansion on
/// cmd, which never re-parses the expanded value).
fn interpolate_for_shell(
    tpl: &str,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
    loop_vars: Option<(&str, &str)>,
) -> (String, Vec<(String, String)>) {
    let mut env: Vec<(String, String)> = Vec::new();
    let cmd = interpolate_inner(tpl, prev, outputs, now_dt, loop_vars, &mut |v| {
        let name = format!("ZERITH_V{}", env.len());
        env.push((name.clone(), v.to_string()));
        if cfg!(windows) {
            format!("!{name}!")
        } else {
            format!("\"${name}\"")
        }
    });
    (cmd, env)
}

fn cfg_str<'a>(cfg: &'a Value, key: &str) -> Option<&'a str> {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn clip_text(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut clipped: String = s.chars().take(max).collect();
        clipped.push_str("\n...[truncado]");
        clipped
    }
}

fn run_shell_command(
    cmd: &str,
    cwd: Option<&Path>,
    timeout: Duration,
    env: &[(String, String)],
) -> Result<String> {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Delayed expansion only when values travel by env var, so a
            // literal `!` in a plain command keeps its old meaning.
            let v = if env.is_empty() { "" } else { "/V:ON " };
            c.raw_arg(format!("{v}/S /C \"{cmd}\""));
        }
        #[cfg(not(windows))]
        {
            c.arg("/C").arg(cmd);
        }
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    if let Some(dir) = cwd {
        if dir.is_dir() {
            c.current_dir(dir);
        }
    }
    c.envs(env.iter().map(|(k, v)| (k, v)));
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    wait_command_with_timeout(c, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{WorkflowEdge, WorkflowNode};
    use serde_json::json;

    struct EchoAi;
    impl AiExecutor for EchoAi {
        fn complete(&self, model: Option<&str>, system: &str, prompt: &str) -> Result<String> {
            let m = model.unwrap_or("default");
            Ok(format!("AI_REPORT[{m}|{system}]: {prompt}"))
        }

        fn resolve_bot(&self, bot_name: &str) -> Option<(Option<String>, String)> {
            if bot_name == "noc-engineer" {
                Some((Some("gpt-4o-mini".into()), "NOC specialist".into()))
            } else {
                None
            }
        }
    }

    #[test]
    fn dag_execution_interpolates_prev_and_branches_condition() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("workflows.db");
        let store = Store::open(&db_path).unwrap();
        store.seed_defaults().unwrap();
        assert_eq!(store.list_workflows().unwrap().len(), 5);

        let wf = Workflow {
            alert: Default::default(),
            run_on: None,
            id: "test-wf".into(),
            name: "Test Pipeline".into(),
            description: "".into(),
            icon: "workflow".into(),
            enabled: true,
            nodes: vec![
                WorkflowNode {
                    id: "t".into(),
                    kind: NodeKind::TriggerManual,
                    label: "Start".into(),
                    x: 0.0,
                    y: 0.0,
                    config: json!({}),
                },
                WorkflowNode {
                    id: "s".into(),
                    kind: NodeKind::Shell,
                    label: "Echo".into(),
                    x: 100.0,
                    y: 0.0,
                    config: json!({ "command": "echo OFFLINE_SWITCH_01" }),
                },
                WorkflowNode {
                    id: "a".into(),
                    kind: NodeKind::AiAgent,
                    label: "AI".into(),
                    x: 200.0,
                    y: 0.0,
                    config: json!({ "prompt": "Check: {{prev}}" }),
                },
                WorkflowNode {
                    id: "c".into(),
                    kind: NodeKind::Condition,
                    label: "Has Offline?".into(),
                    x: 300.0,
                    y: 0.0,
                    config: json!({ "mode": "contains", "pattern": "OFFLINE_SWITCH_01" }),
                },
                WorkflowNode {
                    id: "f_true".into(),
                    kind: NodeKind::SaveFile,
                    label: "Write True".into(),
                    x: 400.0,
                    y: 0.0,
                    config: json!({ "path": "out_true.txt" }),
                },
                WorkflowNode {
                    id: "f_false".into(),
                    kind: NodeKind::SaveFile,
                    label: "Write False".into(),
                    x: 400.0,
                    y: 120.0,
                    config: json!({ "path": "out_false.txt" }),
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "e1".into(),
                    from: "t".into(),
                    to: "s".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e2".into(),
                    from: "s".into(),
                    to: "a".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e3".into(),
                    from: "a".into(),
                    to: "c".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e4".into(),
                    from: "c".into(),
                    to: "f_true".into(),
                    branch: Some("true".into()),
                },
                WorkflowEdge {
                    id: "e5".into(),
                    from: "c".into(),
                    to: "f_false".into(),
                    branch: Some("false".into()),
                },
            ],
            created_at_ms: 1,
            updated_at_ms: 1,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        };

        store.save_workflow(&wf).unwrap();
        let ai = EchoAi;
        let mut events = Vec::new();
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "test",
            tz_offset_minutes: -180,
            initial_input: None,
            on_event: &mut |ev| events.push(ev),
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok");
        assert!(dir.path().join("out_true.txt").is_file());
        assert!(!dir.path().join("out_false.txt").is_file());
        let saved = std::fs::read_to_string(dir.path().join("out_true.txt")).unwrap();
        assert!(
            saved.contains(
                "AI_REPORT[default|You are a helpful automation agent.]: Check: OFFLINE_SWITCH_01"
            ),
            "{saved}"
        );
        assert!(events
            .iter()
            .any(|e| e.node_id == "f_false" && e.status == "skipped"));
        assert_eq!(store.list_runs("test-wf", 10).unwrap().len(), 1);
    }

    #[test]
    fn transform_json_path_and_filter_lines_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let ai = EchoAi;
        let opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "test",
            tz_offset_minutes: -180,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let n_json = WorkflowNode {
            id: "j".into(),
            kind: NodeKind::Transform,
            label: "Extract offline".into(),
            x: 0.0,
            y: 0.0,
            config: json!({ "mode": "json_path", "pattern": "data.offline.0" }),
        };
        let out_j = test_single_node(
            &n_json,
            "HTTP 200\n{\"data\":{\"offline\":[\"OLT-01\"]}}",
            &opts,
        )
        .unwrap();
        assert_eq!(out_j, "OLT-01");

        let n_filter = WorkflowNode {
            id: "f".into(),
            kind: NodeKind::Transform,
            label: "Filter DOWN".into(),
            x: 0.0,
            y: 0.0,
            config: json!({ "mode": "filter_lines", "pattern": "DOWN" }),
        };
        let out_f = test_single_node(&n_filter, "SW-01 UP\nOLT-02 DOWN\nSW-03 UP", &opts).unwrap();
        assert_eq!(out_f, "OLT-02 DOWN");
    }

    #[test]
    fn webhook_pin_loop_and_bot_binding_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let ai = EchoAi;
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "webhook",
            tz_offset_minutes: -180,
            initial_input: Some("[\"10.0.0.1\",\"10.0.0.2\"]"),
            on_event: &mut |_| {},
        };
        let wf = Workflow {
            alert: Default::default(),
            run_on: None,
            id: "hook-wf".into(),
            name: "Hook + Loop + Mock + Bot".into(),
            description: "".into(),
            icon: "workflow".into(),
            enabled: true,
            bot_name: Some("noc-engineer".into()),
            nodes: vec![
                WorkflowNode {
                    id: "wh".into(),
                    kind: NodeKind::TriggerWebhook,
                    label: "Webhook".into(),
                    x: 0.0,
                    y: 0.0,
                    config: json!({ "slug": "zabbix" }),
                },
                WorkflowNode {
                    id: "lp".into(),
                    kind: NodeKind::Loop,
                    label: "Format IPs".into(),
                    x: 100.0,
                    y: 0.0,
                    config: json!({
                        "mode": "template",
                        "template": "#{{index}} host={{item}}",
                        "separator": ", "
                    }),
                },
                WorkflowNode {
                    id: "mk".into(),
                    kind: NodeKind::Shell,
                    label: "Mocked Probe".into(),
                    x: 200.0,
                    y: 0.0,
                    config: json!({
                        "command": "exit 1",
                        "pin_enabled": true,
                        "pinned_output": "PINNED_OK"
                    }),
                },
                WorkflowNode {
                    id: "ai".into(),
                    kind: NodeKind::AiAgent,
                    label: "Bot Summary".into(),
                    x: 300.0,
                    y: 0.0,
                    config: json!({
                        "bot_name": "noc-engineer",
                        "prompt": "{{node.lp}} | {{prev}}"
                    }),
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "e1".into(),
                    from: "wh".into(),
                    to: "lp".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e2".into(),
                    from: "lp".into(),
                    to: "mk".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e3".into(),
                    from: "mk".into(),
                    to: "ai".into(),
                    branch: None,
                },
            ],
            created_at_ms: 1,
            updated_at_ms: 1,
            last_run_at_ms: None,
            last_status: None,
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok");
        assert_eq!(
            run.summary,
            "AI_REPORT[gpt-4o-mini|NOC specialist]: #0 host=10.0.0.1, #1 host=10.0.0.2 | PINNED_OK"
        );
    }

    #[test]
    fn dedup_lines_filters_previously_seen_items() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let ai = EchoAi;
        let opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "test",
            tz_offset_minutes: -180,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let n = WorkflowNode {
            id: "dedup1".into(),
            kind: NodeKind::Transform,
            label: "Dedup Videos".into(),
            x: 0.0,
            y: 0.0,
            config: json!({ "mode": "dedup_lines", "pattern": "yt-videos" }),
        };
        let first =
            test_single_node(&n, "https://yt/1\nhttps://yt/2\nhttps://yt/1", &opts).unwrap();
        assert_eq!(first, "https://yt/1\nhttps://yt/2");
        let second = test_single_node(&n, "https://yt/2\nhttps://yt/3", &opts).unwrap();
        assert_eq!(second, "https://yt/3");
        let third =
            test_single_node(&n, "https://yt/1\nhttps://yt/2\nhttps://yt/3", &opts).unwrap();
        assert_eq!(third, "");

        let ai_loop = WorkflowNode {
            id: "ai_loop".into(),
            kind: NodeKind::AiAgent,
            label: "Loop Agent".into(),
            x: 0.0,
            y: 0.0,
            config: json!({
                "use_tools": true,
                "max_turns": 8,
                "tool_mode": "bypass",
                "bot_name": "noc-engineer",
                "prompt": "Find videos"
            }),
        };
        let ai_out = test_single_node(&ai_loop, "", &opts).unwrap();
        assert!(ai_out.contains("Find videos"), "{ai_out}");
    }

    fn node(id: &str, kind: NodeKind, config: Value) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            kind,
            label: id.into(),
            x: 0.0,
            y: 0.0,
            config,
        }
    }

    fn edge(from: &str, to: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: format!("{from}-{to}"),
            from: from.into(),
            to: to.into(),
            branch: None,
        }
    }

    fn wf_of(nodes: Vec<WorkflowNode>, edges: Vec<WorkflowEdge>) -> Workflow {
        Workflow {
            alert: Default::default(),
            run_on: None,
            id: "wf".into(),
            name: "wf".into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            bot_name: None,
            nodes,
            edges,
            created_at_ms: 1,
            updated_at_ms: 1,
            last_run_at_ms: None,
            last_status: None,
        }
    }

    #[test]
    fn segredo_resolvido_na_execucao_e_mascarado_no_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        TEST_VAULT.with(|v| v.borrow_mut().insert("api_x".into(), "s3nh4-Sup3r".into()));
        let echo = if cfg!(windows) {
            "echo {{secret:api_x}}"
        } else {
            "printf '%s\\n' {{secret:api_x}}"
        };
        let wf = wf_of(
            vec![
                node(
                    "t",
                    NodeKind::Transform,
                    json!({"template": "tok={{secret:api_x}}"}),
                ),
                node("s", NodeKind::Shell, json!({"command": echo})),
                node(
                    "d",
                    NodeKind::Transform,
                    json!({"template": "antes: {{node.t}}"}),
                ),
            ],
            vec![edge("t", "s"), edge("s", "d")],
        );
        let events = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&events);
        let mut on_event = move |e: NodeEvent| sink.lock().unwrap().push_str(&e.output);
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &NoopAiExecutor,
            trigger_source: "manual",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut on_event,
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.node_logs);
        assert_eq!(run.node_logs[0].output, "tok=***");
        assert_eq!(run.node_logs[1].output.trim(), "***");
        // O nó seguinte recebe o texto já mascarado.
        assert_eq!(run.node_logs[2].output, "antes: tok=***");
        let saved = serde_json::to_string(&store.list_runs("wf", 1).unwrap()).unwrap();
        assert!(!saved.contains("s3nh4-Sup3r") && saved.contains("***"));
        assert!(!run.summary.contains("s3nh4") && !events.lock().unwrap().contains("s3nh4"));

        // Segredo que falta: o nó falha com o nome, sem rodar com o token cru.
        let wf = wf_of(
            vec![node(
                "t",
                NodeKind::Transform,
                json!({"template": "{{secret:nada}}"}),
            )],
            vec![],
        );
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "error");
        assert!(run.summary.contains("segredo 'nada'"), "{}", run.summary);
        assert!(!run.summary.contains("s3nh4"));
    }

    #[test]
    fn mascara_troca_o_valor_mais_longo_primeiro() {
        let vals = vec!["ab".to_string(), "abcd".to_string(), String::new()];
        assert_eq!(mask_secrets("x abcd y ab", &vals), "x *** y ***");
        assert_eq!(mask_secrets("nada", &[]), "nada");
    }

    #[test]
    fn only_triggers_matching_source_start_branches() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let wf = wf_of(
            vec![
                node("cron", NodeKind::TriggerCron, json!({})),
                node("hook", NodeKind::TriggerWebhook, json!({})),
                node("fc", NodeKind::SaveFile, json!({"path": "cron.txt"})),
                node("fh", NodeKind::SaveFile, json!({"path": "hook.txt"})),
                node("orphan", NodeKind::SaveFile, json!({"path": "orphan.txt"})),
            ],
            vec![edge("cron", "fc"), edge("hook", "fh")],
        );
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &EchoAi,
            trigger_source: "webhook",
            tz_offset_minutes: 0,
            initial_input: Some("payload"),
            on_event: &mut |_| {},
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok", "{}", run.summary);
        assert!(dir.path().join("hook.txt").is_file());
        assert!(!dir.path().join("cron.txt").exists());
        assert!(!dir.path().join("orphan.txt").exists());
    }

    #[test]
    fn interpolated_values_cannot_inject_shell_or_escape_save_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let payload = if cfg!(windows) {
            "a & echo INJECTED"
        } else {
            "a; echo INJECTED $(echo X) '"
        };
        let wf = wf_of(
            vec![
                node("hook", NodeKind::TriggerWebhook, json!({})),
                node("sh", NodeKind::Shell, json!({"command": "echo {{prev}}"})),
                node(
                    "f",
                    NodeKind::SaveFile,
                    json!({"path": "out/{{node.hook}}.txt", "content": "x"}),
                ),
            ],
            vec![edge("hook", "sh"), edge("sh", "f")],
        );
        let run_with = |input: &str| {
            let mut opts = RunOptions {
                store: &store,
                data_root: dir.path(),
                ai: &EchoAi,
                trigger_source: "webhook",
                tz_offset_minutes: 0,
                initial_input: Some(input),
                on_event: &mut |_| {},
            };
            execute_workflow(&wf, &mut opts).unwrap()
        };
        let run = run_with(payload);
        assert_eq!(run.status, "ok", "{}", run.summary);
        assert_eq!(run.node_logs[1].output, payload);

        let run = run_with("../../evil");
        assert_eq!(run.status, "ok", "{}", run.summary);
        assert!(dir.path().join("out").join("___evil.txt").is_file());
        assert_eq!(posix_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn dedup_keys_commit_only_when_run_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let mk = |fail: bool| {
            wf_of(
                vec![
                    node("t", NodeKind::TriggerManual, json!({})),
                    node(
                        "src",
                        NodeKind::Transform,
                        json!({"mode": "template", "template": "a\nb"}),
                    ),
                    node(
                        "d",
                        NodeKind::Transform,
                        json!({"mode": "dedup_lines", "pattern": "s"}),
                    ),
                    node(
                        "after",
                        NodeKind::Shell,
                        json!({"command": if fail { "exit 3" } else { "echo ok" }}),
                    ),
                ],
                vec![edge("t", "src"), edge("src", "d"), edge("d", "after")],
            )
        };
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &EchoAi,
            trigger_source: "manual",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let failed = execute_workflow(&mk(true), &mut opts).unwrap();
        assert_eq!(failed.status, "error");
        let lines = vec!["a".to_string(), "b".to_string()];
        assert_eq!(store.unseen_items("s", &lines).unwrap(), lines);
        let ok = execute_workflow(&mk(false), &mut opts).unwrap();
        assert_eq!(ok.status, "ok", "{}", ok.summary);
        assert!(store.unseen_items("s", &lines).unwrap().is_empty());
    }

    #[test]
    fn large_output_over_100kib_does_not_deadlock_pipe() {
        let cmd = if cfg!(windows) {
            "echo BEGIN_LARGE && (for /L %i in (1,1,1600) do @echo 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef) && echo END_LARGE"
        } else {
            "echo BEGIN_LARGE && i=0 && while [ $i -lt 1600 ]; do echo 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef; i=$((i+1)); done && echo END_LARGE"
        };

        let mut raw_cmd = if cfg!(windows) {
            #[cfg_attr(not(windows), allow(unused_mut))]
            let mut c = Command::new("cmd");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.raw_arg(format!("/S /C \"{cmd}\""));
            }
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd);
            c
        };
        raw_cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let wait = wait_command_raw(raw_cmd, Duration::from_secs(10)).unwrap();
        match wait {
            CommandWait::Exited(status, out, err) => {
                assert!(
                    status.success(),
                    "status={status:?} err={}",
                    String::from_utf8_lossy(&err)
                );
                assert!(
                    out.len() > 100 * 1024,
                    "expected >100 KiB output, got {}",
                    out.len()
                );
                let text = String::from_utf8_lossy(&out);
                assert!(text.contains("BEGIN_LARGE"));
                assert!(text.contains("END_LARGE"));
            }
            CommandWait::TimedOut(_, _) => panic!("command timed out while draining >100 KiB pipe"),
        }

        let shell_out = run_shell_command(cmd, None, Duration::from_secs(10), &[]).unwrap();
        assert!(shell_out.starts_with("BEGIN_LARGE"));
        assert!(shell_out.contains("...[truncado]"));
    }

    #[test]
    fn cycle_detection_marks_run_error_and_preserves_skipped_branches() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cycles.db")).unwrap();
        let ai = EchoAi;

        // Trigger -> A -> B -> A cycle
        let wf_cycle = Workflow {
            alert: Default::default(),
            run_on: None,
            id: "cycle-wf".into(),
            name: "Cycle".into(),
            description: "".into(),
            icon: "workflow".into(),
            enabled: true,
            bot_name: None,
            nodes: vec![
                WorkflowNode {
                    id: "t".into(),
                    kind: NodeKind::TriggerManual,
                    label: "Trigger".into(),
                    x: 0.0,
                    y: 0.0,
                    config: json!({}),
                },
                WorkflowNode {
                    id: "a".into(),
                    kind: NodeKind::Transform,
                    label: "A".into(),
                    x: 100.0,
                    y: 0.0,
                    config: json!({ "mode": "template", "template": "{{prev}}" }),
                },
                WorkflowNode {
                    id: "b".into(),
                    kind: NodeKind::Transform,
                    label: "B".into(),
                    x: 200.0,
                    y: 0.0,
                    config: json!({ "mode": "template", "template": "{{prev}}" }),
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "e1".into(),
                    from: "t".into(),
                    to: "a".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e2".into(),
                    from: "a".into(),
                    to: "b".into(),
                    branch: None,
                },
                WorkflowEdge {
                    id: "e3".into(),
                    from: "b".into(),
                    to: "a".into(),
                    branch: None,
                },
            ],
            created_at_ms: 1,
            updated_at_ms: 1,
            last_run_at_ms: None,
            last_status: None,
        };

        let mut opts1 = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "test",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let run1 = execute_workflow(&wf_cycle, &mut opts1).unwrap();
        assert_eq!(run1.status, "error");
        assert_eq!(run1.summary, "ciclo detectado no grafo do workflow");

        // Rapid consecutive runs still get distinct monotonic run_ids.
        let run2 = execute_workflow(&wf_cycle, &mut opts1).unwrap();
        assert_ne!(run1.id, run2.id);
    }

    #[test]
    fn interpolate_single_pass_preserves_literal_tokens_in_upstream_output() {
        let dt = LocalDateTime::from_unix_secs(1_700_000_000, 0);
        let mut outputs = HashMap::new();
        outputs.insert(
            "n1".to_string(),
            "up {{date}} {{time}} {{prev}}".to_string(),
        );

        let prev = "literal {{date}} {{time}} {{node.n1}} {{prev}}";
        let rendered = interpolate(
            "D={{date}} | P={{prev}} | N={{node.n1}} | U={{unknown}}",
            prev,
            &outputs,
            &dt,
        );
        assert_eq!(
            rendered,
            format!(
                "D={} | P=literal {{{{date}}}} {{{{time}}}} {{{{node.n1}}}} {{{{prev}}}} | N=up {{{{date}}}} {{{{time}}}} {{{{prev}}}} | U={{{{unknown}}}}",
                dt.date_string()
            )
        );

        // Loop node does not re-expand {{date}} or {{index}} inside {{item}} or {{prev}}.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("loop.db")).unwrap();
        let ai = EchoAi;
        let opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &ai,
            trigger_source: "test",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let lp = WorkflowNode {
            id: "lp".into(),
            kind: NodeKind::Loop,
            label: "Loop".into(),
            x: 0.0,
            y: 0.0,
            config: json!({
                "mode": "template",
                "template": "#{{index}}={{item}}|{{prev}}"
            }),
        };
        let out = test_single_node(&lp, "{{date}} {{index}} {{item}}", &opts).unwrap();
        assert_eq!(
            out,
            "#0={{date}} {{index}} {{item}}|{{date}} {{index}} {{item}}"
        );
    }

    #[test]
    fn notify_anexa_arquivo_do_save_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        let ch = crate::model::ChannelConfig {
            id: "local".into(),
            name: "local".into(),
            kind: crate::model::ChannelKind::DesktopNotify,
            enabled: true,
            config: json!({}),
            updated_at_ms: 0,
        };
        store.save_channel(&ch).unwrap();
        let wf = wf_of(
            vec![
                node("m", NodeKind::TriggerManual, json!({})),
                node(
                    "f",
                    NodeKind::SaveFile,
                    json!({"path": "r/rel.md", "content": "x"}),
                ),
                node(
                    "n",
                    NodeKind::NotifyChannel,
                    json!({"channel_id": "local", "message": "ok", "attach_node": "f"}),
                ),
                node(
                    "p",
                    NodeKind::NotifyChannel,
                    json!({"channel_id": "local", "attachment": "r/{{prev}}"}),
                ),
            ],
            vec![edge("m", "f"), edge("f", "n"), edge("n", "p")],
        );
        let mut opts = RunOptions {
            store: &store,
            data_root: dir.path(),
            ai: &EchoAi,
            trigger_source: "manual",
            tz_offset_minutes: 0,
            initial_input: None,
            on_event: &mut |_| {},
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert!(
            run.node_logs[2].output.contains("anexo ignorado"),
            "{:?}",
            run.node_logs
        );
        // `{{prev}}` vira um nome sem barras (não sai de r/) e inexistente.
        assert_eq!(run.status, "error");
        assert!(run.summary.contains("nó 'p'"), "{}", run.summary);
    }
}
