//! The hook events added after the first nine, against a real turn:
//! `Notification`, `SubagentStart` / `SubagentStop`, `PostCompact`,
//! `TaskCompleted`, and a `PreToolUse` `updatedInput` that the policy
//! re-evaluates. Each hook writes its stdin to a file the test reads.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_core::settings::{HookSpec, Settings};
use zerith_providers::{Cancel, Client, Protocol, ProviderConfig, ProviderKind, RetryPolicy};
use zerith_runtime::{
    approval_channel, run_agent_turn, AgentOptions, Approval, RunEvent, SubagentOptions,
    TurnContext,
};
use zerith_security::{preset, Mode, Policy};
use zerith_sessions::{SessionSource, Store};
use zerith_tools::Registry;

/// Answers each request with `reply(body)`, any number of times.
fn serve(reply: impl Fn(&Value) -> String + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let reply = Arc::new(reply);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { return };
            let reply = reply.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let _ = sock.write_all(reply(&body).as_bytes());
            });
        }
    });
    url
}

fn sse(chunks: &[Value]) -> String {
    let mut s = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for c in chunks {
        s.push_str(&format!("data: {c}\n\n"));
    }
    s.push_str("data: [DONE]\n\n");
    s
}

fn text(t: &str) -> String {
    sse(&[
        json!({"choices":[{"delta":{"content":t}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
    ])
}

fn call(id: &str, name: &str, args: Value) -> String {
    sse(&[json!({"choices":[{"delta":{"tool_calls":[
        {"index": 0, "id": id, "function": {"name": name, "arguments": args.to_string()}}
    ]}}]})])
}

fn messages(b: &Value) -> Vec<Value> {
    b["messages"].as_array().cloned().unwrap_or_default()
}

fn has_tool_result(b: &Value) -> bool {
    messages(b).iter().any(|m| m["role"] == "tool")
}

/// The first user message: which conversation (parent or child) asks.
fn first_user(b: &Value) -> String {
    messages(b)
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string()
}

fn client(url: &str, context_limit: Option<u32>) -> Client {
    let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("k");
    cfg.base_url = url.into();
    cfg.protocol = Protocol::OpenAi;
    cfg.timeout_ms = 10_000;
    cfg.context_limit = context_limit;
    Client::new(cfg).unwrap().with_retry(RetryPolicy {
        max_retries: 0,
        base_ms: 1,
        max_ms: 5,
        max_retry_after_ms: 1_000,
    })
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// A hook that appends its stdin (one JSON object) as a line of `file`.
fn record(file: &Path) -> String {
    #[cfg(windows)]
    {
        format!("more >> \"{}\"", file.display())
    }
    #[cfg(not(windows))]
    {
        format!("cat >> '{}'; echo >> '{}'", file.display(), file.display())
    }
}

fn spec(command: String) -> HookSpec {
    HookSpec {
        matcher: None,
        command,
        timeout_ms: 20_000,
    }
}

/// The JSON objects a `record` hook wrote, waiting up to 10 s for `n`
/// (Notification hooks run in the background).
fn recorded(file: &Path, n: usize) -> Vec<Value> {
    let t = Instant::now();
    loop {
        let got: Vec<Value> = std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        if got.len() >= n || t.elapsed() > Duration::from_secs(10) {
            return got;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_waiting_approval_sends_a_notification() {
    let dir = tmp();
    let notes = dir.path().join("notes.jsonl");
    let mut settings = Settings::default();
    settings.hooks.notification.push(HookSpec {
        matcher: Some("permission_prompt".into()),
        ..spec(record(&notes))
    });
    let url = serve(|b| {
        if has_tool_result(b) {
            text("fim")
        } else {
            call("c1", "shell", json!({"command": "make deploy"}))
        }
    });
    let store = Store::open_in_memory().unwrap();
    let sid = store.create_session("t", SessionSource::Native).unwrap().id;
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let (tx, gate) = approval_channel();
    let reg = Registry::new();
    run_agent_turn(
        &store,
        &client(&url, None),
        TurnContext {
            settings: Some(&settings),
            no_project_instructions: true,
            ..TurnContext::default()
        },
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            approvals: Some(&gate),
            ..Default::default()
        },
        &sid,
        "m",
        "publique",
        &Cancel::default(),
        |e| {
            if let RunEvent::ApprovalNeeded { id, .. } = &e {
                assert!(tx.answer(*id, Approval::Deny));
            }
        },
    )
    .unwrap();
    let n = recorded(&notes, 1);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0]["hook_event_name"], "Notification");
    assert_eq!(n[0]["notification_type"], "permission_prompt");
    assert_eq!(n[0]["session_id"], sid.as_str());
    assert!(n[0]["message"].as_str().unwrap().contains("make deploy"));
}

#[test]
fn subagent_start_and_stop_fire_and_stop_can_send_the_child_back() {
    let dir = tmp();
    std::env::set_var("ZERITH_HOME", dir.path().join("home"));
    let log = dir.path().join("sub.jsonl");
    let marker = dir.path().join("once");
    let mut settings = Settings::default();
    settings.hooks.subagent_start.push(HookSpec {
        matcher: Some("explore".into()),
        ..spec(record(&log))
    });
    settings.hooks.subagent_stop.push(spec(record(&log)));
    // Blocks the first stop only: the child must run again with the reason.
    #[cfg(windows)]
    let once = format!(
        "if not exist \"{p}\" (break > \"{p}\" & echo {{\"decision\":\"block\",\"reason\":\"confira de novo\"}})",
        p = marker.display()
    );
    #[cfg(not(windows))]
    let once = format!(
        "if [ ! -f '{p}' ]; then : > '{p}'; echo '{{\"decision\":\"block\",\"reason\":\"confira de novo\"}}'; fi",
        p = marker.display()
    );
    settings.hooks.subagent_stop.push(spec(once));
    let child_rounds = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = child_rounds.clone();
    let url = serve(move |b| {
        if first_user(b) == "PAI" {
            return if has_tool_result(b) {
                text("pai terminou")
            } else {
                call(
                    "c1",
                    "agent",
                    json!({"description": "olhar", "prompt": "FILHO", "agent": "explore"}),
                )
            };
        }
        let last = messages(b)
            .last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();
        seen.lock().unwrap().push(last.clone());
        text(if last.contains("confira de novo") {
            "conferido"
        } else {
            "rascunho"
        })
    });
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let sid = store
        .create_session("pai", SessionSource::Native)
        .unwrap()
        .id;
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let reg = Registry::new();
    let report = run_agent_turn(
        &store,
        &client(&url, None),
        TurnContext {
            settings: Some(&settings),
            no_project_instructions: true,
            ..TurnContext::default()
        },
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            subagents: SubagentOptions {
                gate: Some(zerith_runtime::Gate::new()),
                ..SubagentOptions::default()
            },
            ..Default::default()
        },
        &sid,
        "m",
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.text, "pai terminou");
    // The child ran twice: the second time on the hook's reason, and the
    // parent got the second answer.
    let rounds = child_rounds.lock().unwrap().clone();
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert!(rounds[1].contains("confira de novo"), "{rounds:?}");
    let tool = store
        .messages(&sid, 100, 0)
        .unwrap()
        .into_iter()
        .find(|m| m.tool_name.as_deref() == Some("agent"))
        .unwrap();
    assert!(tool.content.contains("conferido"), "{}", tool.content);

    let got = recorded(&log, 3);
    let names: Vec<&str> = got
        .iter()
        .map(|v| v["hook_event_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["SubagentStart", "SubagentStop", "SubagentStop"]);
    assert_eq!(got[0]["agent_type"], "explore");
    assert!(got[0]["agent_id"].as_str().unwrap().starts_with("sub-"));
    assert_eq!(got[0]["session_id"], sid.as_str());
    assert_eq!(got[1]["stop_reason"], "done");
    assert_eq!(got[1]["stop_hook_active"], false);
    assert_eq!(got[1]["last_assistant_message"], "rascunho");
    assert_eq!(got[2]["stop_hook_active"], true);
    assert!(got[2]["steps"].as_u64().unwrap() >= 2);
    assert!(got[2]["tokens"]["prompt"].as_u64().unwrap() >= 20);
}

#[test]
fn post_compact_reports_what_the_compaction_did() {
    let dir = tmp();
    let log = dir.path().join("compact.jsonl");
    let mut settings = Settings::default();
    settings.hooks.post_compact.push(spec(record(&log)));
    let url = serve(|_| text("ok"));
    let store = Store::open_in_memory().unwrap();
    let sid = store.create_session("t", SessionSource::Native).unwrap().id;
    // Long enough for a 2500-token window to need a summary.
    for i in 0..30 {
        let (role, body) = if i % 2 == 0 {
            (
                zerith_sessions::Role::User,
                format!("pergunta {i} {}", "p".repeat(1200)),
            )
        } else {
            (
                zerith_sessions::Role::Assistant,
                format!("resposta {i} {}", "r".repeat(1200)),
            )
        };
        store
            .append(&sid, &zerith_sessions::NewMessage::new(role, body))
            .unwrap();
    }
    struct Fake;
    impl zerith_runtime::Summarizer for Fake {
        fn summarize(&self, _: &str, _: &Cancel) -> Result<String, String> {
            Ok("RESUMO".into())
        }
    }
    run_agent_turn(
        &store,
        &client(&url, Some(2500)),
        TurnContext {
            settings: Some(&settings),
            no_project_instructions: true,
            ..TurnContext::default()
        },
        AgentOptions {
            summarizer: Some(&Fake),
            history: 100,
            ..Default::default()
        },
        &sid,
        "m",
        "PERGUNTA FINAL",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let got = recorded(&log, 1);
    assert_eq!(got[0]["hook_event_name"], "PostCompact");
    assert_eq!(got[0]["trigger"], "auto");
    assert_eq!(got[0]["summarized"], true);
    assert!(got[0]["summary"]["messages"].as_u64().unwrap() > 0);
    assert!(got[0]["tokens_after"].as_u64().unwrap() < got[0]["tokens_before"].as_u64().unwrap());
}

#[test]
fn task_completed_can_keep_a_task_open() {
    let dir = tmp();
    let log = dir.path().join("task.jsonl");
    let mut settings = Settings::default();
    settings.hooks.task_completed.push(spec(record(&log)));
    #[cfg(windows)]
    let deny = "echo sem testes 1>&2 & exit /b 2";
    #[cfg(not(windows))]
    let deny = "echo sem testes 1>&2; exit 2";
    settings.hooks.task_completed.push(spec(deny.into()));
    let url = serve(|b| {
        let results = messages(b).iter().filter(|m| m["role"] == "tool").count();
        match results {
            0 => call("c1", "task_create", json!({"subject": "rodar testes"})),
            1 => call("c2", "task_update", json!({"id": 1, "status": "completed"})),
            _ => text("fim"),
        }
    });
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let sid = store.create_session("t", SessionSource::Native).unwrap().id;
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let reg = Registry::new();
    run_agent_turn(
        &store,
        &client(&url, None),
        TurnContext {
            settings: Some(&settings),
            no_project_instructions: true,
            ..TurnContext::default()
        },
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            ..Default::default()
        },
        &sid,
        "m",
        "faça",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let got = recorded(&log, 1);
    assert_eq!(got[0]["hook_event_name"], "TaskCompleted");
    assert_eq!(got[0]["task_id"], 1);
    assert_eq!(got[0]["task_subject"], "rodar testes");
    let todo = store.todo(&sid, 1).unwrap().unwrap();
    assert_ne!(todo.status, zerith_sessions::TodoStatus::Completed);
    let update = store
        .messages(&sid, 100, 0)
        .unwrap()
        .into_iter()
        .rfind(|m| m.tool_name.as_deref() == Some("task_update"))
        .unwrap();
    assert!(update.content.contains("sem testes"), "{}", update.content);
}
