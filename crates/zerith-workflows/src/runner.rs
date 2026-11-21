use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::channels;
use crate::model::{
    NodeEvent, NodeKind, NodeRunLog, Workflow, WorkflowEdge, WorkflowNode, WorkflowRun,
};
use crate::schedule::LocalDateTime;
use crate::store::Store;
use crate::{now_ms, unix_secs, Result, WorkflowError};

pub trait AiExecutor: Send + Sync {
    fn complete(&self, model: Option<&str>, system: &str, prompt: &str) -> Result<String>;
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
    pub on_event: &'a mut (dyn FnMut(NodeEvent) + Send),
}

pub fn execute_workflow(wf: &Workflow, opts: &mut RunOptions<'_>) -> Result<WorkflowRun> {
    let run_id = format!("run-{}-{}", wf.id, now_ms());
    let started_at_ms = now_ms();
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

    // Roots: triggers first, then any node with indegree 0.
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut scheduled: HashSet<&str> = HashSet::new();
    for n in &wf.nodes {
        if indegree.get(n.id.as_str()).copied().unwrap_or(0) == 0 {
            queue.push_back(n.id.as_str());
            scheduled.insert(n.id.as_str());
        }
    }
    if queue.is_empty() {
        if let Some(first) = wf.nodes.first() {
            queue.push_back(first.id.as_str());
            scheduled.insert(first.id.as_str());
        }
    }

    let mut outputs: HashMap<String, String> = HashMap::new();
    let mut active_nodes: HashSet<String> = queue.iter().map(|s| (*s).to_string()).collect();
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
        let res = run_node(node, &prev, &outputs, &now_dt, opts);
        let duration_ms = t0.elapsed().as_millis() as u64;

        match res {
            Ok((out, branch)) => {
                outputs.insert(node.id.clone(), out.clone());
                if !out.is_empty() {
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
        NodeKind::Shell => {
            let raw_cmd = cfg_str(cfg, "command").unwrap_or("");
            if raw_cmd.is_empty() {
                return Err(WorkflowError::Exec("comando shell vazio".into()));
            }
            let cmd = interpolate(raw_cmd, prev, outputs, now_dt);
            let timeout = cfg
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .clamp(1, 600);
            let workdir = cfg_str(cfg, "cwd").map(PathBuf::from);
            let out = run_shell_command(&cmd, workdir.as_deref(), Duration::from_secs(timeout))?;
            Ok((out, None))
        }
        NodeKind::SshCommand => {
            let host = cfg_str(cfg, "host")
                .ok_or_else(|| WorkflowError::Exec("host SSH obrigatório".into()))?;
            if host.starts_with('-') {
                return Err(WorkflowError::Exec("host SSH inválido".into()));
            }
            let user = cfg_str(cfg, "user");
            if let Some(u) = user {
                if u.starts_with('-') {
                    return Err(WorkflowError::Exec("usuário SSH inválido".into()));
                }
            }
            let port = cfg
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(22)
                .clamp(1, 65535);
            let raw_cmd = cfg_str(cfg, "command")
                .ok_or_else(|| WorkflowError::Exec("comando SSH vazio".into()))?;
            let cmd = interpolate(raw_cmd, prev, outputs, now_dt);
            let timeout = cfg
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .clamp(1, 300);
            let dest = match user {
                Some(u) => format!("{u}@{host}"),
                None => host.to_string(),
            };
            let port_str = port.to_string();
            let mut args = vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ConnectTimeout=10",
                "-p",
                &port_str,
            ];
            let key_path = cfg_str(cfg, "identity_file");
            if let Some(kp) = key_path {
                args.push("-i");
                args.push(kp);
            }
            args.push(&dest);
            args.push(&cmd);
            let out = run_direct_command("ssh", &args, Duration::from_secs(timeout))?;
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

            let mut req = ureq::request(&method, &url).timeout(Duration::from_secs(30));
            if let Some(obj) = cfg.get("headers").and_then(|h| h.as_object()) {
                for (k, v) in obj {
                    if let Some(vs) = v.as_str() {
                        req = req.set(k, &interpolate(vs, prev, outputs, now_dt));
                    }
                }
            }
            let resp = if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
                if cfg
                    .get("headers")
                    .and_then(|h| h.get("Content-Type"))
                    .is_none()
                {
                    req = req.set("Content-Type", "application/json");
                }
                req.send_string(&body)
            } else {
                req.call()
            };
            match resp {
                Ok(r) => {
                    let status = r.status();
                    let text = r.into_string().unwrap_or_default();
                    let clipped = clip_text(&text, 12_000);
                    Ok((format!("HTTP {status}\n{clipped}"), None))
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    let clipped = clip_text(&text, 4000);
                    let formatted = format!("HTTP {code}\n{clipped}");
                    if allow_err {
                        Ok((formatted, None))
                    } else {
                        Err(WorkflowError::Exec(formatted))
                    }
                }
                Err(e) => {
                    if allow_err {
                        Ok((format!("HTTP ERROR: {e}"), None))
                    } else {
                        Err(WorkflowError::Exec(e.to_string()))
                    }
                }
            }
        }
        NodeKind::AiAgent => {
            let model = cfg_str(cfg, "model");
            let system_raw =
                cfg_str(cfg, "system").unwrap_or("You are a helpful automation agent.");
            let prompt_raw = cfg_str(cfg, "prompt").unwrap_or("{{prev}}");
            let system = interpolate(system_raw, prev, outputs, now_dt);
            let prompt = interpolate(prompt_raw, prev, outputs, now_dt);
            let out = opts.ai.complete(model, &system, &prompt)?;
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
                _ => {
                    let tpl = cfg_str(cfg, "template").unwrap_or("{{prev}}");
                    interpolate(tpl, prev, outputs, now_dt)
                }
            };
            Ok((out, None))
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
            let res = channels::dispatch(&ch, target.as_deref(), &msg)?;
            Ok((res, None))
        }
        NodeKind::SaveFile => {
            let rel_raw = cfg_str(cfg, "path").unwrap_or("reports/workflow-{{date}}.md");
            let rel = interpolate(rel_raw, prev, outputs, now_dt);
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
            Ok((format!("Arquivo salvo em {}", target.display()), None))
        }
    }
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
    let (out, branch) = run_node(node, sample_input, &outputs, &now_dt, opts)?;
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

fn run_direct_command(bin: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let mut c = Command::new(bin);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    wait_command_with_timeout(c, timeout)
}

fn wait_command_with_timeout(mut c: Command, timeout: Duration) -> Result<String> {
    let mut child = c.spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            let err = String::from_utf8_lossy(&output.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            if status.success() {
                return Ok(clip_text(text.trim(), 16_000));
            }
            return Err(WorkflowError::Exec(format!(
                "código {:?}: {}",
                status.code(),
                clip_text(text.trim(), 2000)
            )));
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkflowError::Exec(format!(
                "comando expirou após {}s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

pub fn interpolate(
    tpl: &str,
    prev: &str,
    outputs: &HashMap<String, String>,
    now_dt: &LocalDateTime,
) -> String {
    let mut out = tpl
        .replace("{{prev}}", prev)
        .replace("{{date}}", &now_dt.date_string())
        .replace("{{time}}", &now_dt.time_string());
    for (id, val) in outputs {
        let key = format!("{{{{node.{id}}}}}");
        if out.contains(&key) {
            out = out.replace(&key, val);
        }
    }
    out
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

fn run_shell_command(cmd: &str, cwd: Option<&Path>, timeout: Duration) -> Result<String> {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("/S /C \"{cmd}\""));
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
        fn complete(&self, _model: Option<&str>, _system: &str, prompt: &str) -> Result<String> {
            Ok(format!("AI_REPORT: {prompt}"))
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
            on_event: &mut |ev| events.push(ev),
        };
        let run = execute_workflow(&wf, &mut opts).unwrap();
        assert_eq!(run.status, "ok");
        assert!(dir.path().join("out_true.txt").is_file());
        assert!(!dir.path().join("out_false.txt").is_file());
        let saved = std::fs::read_to_string(dir.path().join("out_true.txt")).unwrap();
        assert!(
            saved.contains("AI_REPORT: Check: OFFLINE_SWITCH_01"),
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
}
