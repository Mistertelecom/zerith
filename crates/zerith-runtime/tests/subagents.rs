//! Subagents, the task list, ask_user and the skill tool through the real
//! loop, against a fake provider on 127.0.0.1 that answers by what it is asked
//! (not by order, so parallel children are possible). No network, no real home.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_providers::{Cancel, Client, Protocol, ProviderConfig, ProviderKind};
use zerith_runtime::plan::{parse_plan, run_plan, run_plan_opts, PlanOptions, TaskBudget};
use zerith_runtime::{
    approval_channel, question_channel, run_agent_turn, AgentOptions, Approval, Flow, Middleware,
    RunError, RunEvent, SubagentOptions, ToolInvocation, TurnContext,
};
use zerith_security::{preset, Mode, Policy};
use zerith_sessions::{Role, SessionSource, Store, TodoStatus};
use zerith_tools::Registry;

// ---- fake provider -----------------------------------------------------------

enum Reply {
    Whole(String),
    /// Head and first chunk, a pause, then the rest: a stream that is alive
    /// but slow, so a cancel has something to interrupt.
    Slow(String, Duration, String),
}

const HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

fn chunk(c: &Value) -> String {
    format!("data: {c}\n\n")
}

fn done() -> String {
    format!(
        "{}data: [DONE]\n\n",
        chunk(
            &json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}})
        )
    )
}

fn text(t: &str) -> Reply {
    Reply::Whole(format!(
        "{HEAD}{}{}",
        chunk(&json!({"choices":[{"delta":{"content":t}}]})),
        done()
    ))
}

fn slow(t: &str, pause: Duration) -> Reply {
    Reply::Slow(
        format!(
            "{HEAD}{}",
            chunk(&json!({"choices":[{"delta":{"content":t}}]}))
        ),
        pause,
        format!(
            "{}{}",
            chunk(&json!({"choices":[{"delta":{"content":" fim"}}]})),
            done()
        ),
    )
}

fn calls(list: &[(&str, &str, Value)]) -> Reply {
    let frags: Vec<Value> = list
        .iter()
        .enumerate()
        .map(|(i, (id, name, args))| {
            json!({"index": i, "id": id, "function": {"name": name, "arguments": args.to_string()}})
        })
        .collect();
    Reply::Whole(format!(
        "{HEAD}{}data: [DONE]\n\n",
        chunk(&json!({"choices":[{"delta":{"tool_calls": frags}}]}))
    ))
}

fn bad_request() -> Reply {
    let body = r#"{"error":"nope"}"#;
    Reply::Whole(format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}

/// One thread per connection; `handler` sees the request body.
fn serve(handler: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut sock) = conn else { continue };
            let h = handler.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    return;
                }
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                if r.read_exact(&mut body).is_err() {
                    return;
                }
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                match h(&body) {
                    Reply::Whole(s) => {
                        let _ = sock.write_all(s.as_bytes());
                    }
                    Reply::Slow(a, pause, b) => {
                        let _ = sock.write_all(a.as_bytes());
                        let _ = sock.flush();
                        std::thread::sleep(pause);
                        let _ = sock.write_all(b.as_bytes());
                    }
                }
            });
        }
    });
    url
}

// ---- request helpers ------------------------------------------------------------

fn messages(body: &Value) -> Vec<Value> {
    body["messages"].as_array().cloned().unwrap_or_default()
}

/// The first user message: what this conversation was asked to do.
fn prompt_of(body: &Value) -> String {
    messages(body)
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string()
}

fn tool_msgs(body: &Value) -> Vec<Value> {
    messages(body)
        .into_iter()
        .filter(|m| m["role"] == "tool")
        .collect()
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    t["function"]["name"]
                        .as_str()
                        .or_else(|| t["name"].as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn system_of(body: &Value) -> String {
    messages(body)
        .first()
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string()
}

// ---- environment ---------------------------------------------------------------

/// Nothing here may read or write the real user home.
fn hermetic() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::Builder::new()
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap()
            .keep();
        std::env::set_var("ZERITH_HOME", home);
        std::env::remove_var("ZERITH_PROFILE");
    });
}

struct Env {
    dir: tempfile::TempDir,
    store: Store,
    sid: String,
    policy: Policy,
}

fn env(mode: Mode) -> Env {
    hermetic();
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let sid = store
        .create_session("pai", SessionSource::Native)
        .unwrap()
        .id;
    let policy = Policy::new(preset("workspace-write").unwrap(), mode, dir.path());
    Env {
        dir,
        store,
        sid,
        policy,
    }
}

fn client(url: &str) -> Client {
    let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("k");
    cfg.base_url = url.into();
    cfg.protocol = Protocol::OpenAi;
    cfg.timeout_ms = 10_000;
    Client::new(cfg).unwrap()
}

fn subs(max: usize) -> SubagentOptions {
    SubagentOptions {
        max_concurrent: Some(max),
        // Own gate: tests in this binary run in parallel and must not eat
        // each other's permits.
        gate: Some(zerith_runtime::Gate::new()),
        ..SubagentOptions::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn turn<'a>(
    e: &'a Env,
    url: &str,
    reg: &'a Registry,
    mut opts: AgentOptions<'a>,
    ctx: TurnContext<'_>,
    prompt: &str,
    cancel: &Cancel,
    on: impl FnMut(RunEvent),
) -> zerith_runtime::Result<zerith_runtime::TurnReport> {
    opts.tools = Some(reg);
    opts.policy = Some(&e.policy);
    run_agent_turn(
        &e.store,
        &client(url),
        ctx,
        opts,
        &e.sid,
        "m",
        prompt,
        cancel,
        on,
    )
}

fn subagent_events(events: &[RunEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Subagent { id, state, .. } => Some((id.clone(), state.clone())),
            _ => None,
        })
        .collect()
}

// ---- tests -----------------------------------------------------------------------

#[test]
fn two_agent_calls_run_in_parallel_and_the_parent_sees_only_the_summaries() {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let parent_second: Arc<Mutex<Option<Value>>> = Default::default();
    let (l, p, seen) = (live.clone(), peak.clone(), parent_second.clone());
    let url = serve(move |b| {
        let prompt = prompt_of(b);
        if prompt == "PAI" {
            return if tool_msgs(b).is_empty() {
                calls(&[
                    (
                        "c1",
                        "agent",
                        json!({"description": "a", "prompt": "FILHO-A", "agent": "explore"}),
                    ),
                    (
                        "c2",
                        "agent",
                        json!({"description": "b", "prompt": "FILHO-B", "agent": "explore"}),
                    ),
                ])
            } else {
                *seen.lock().unwrap() = Some(b.clone());
                text("li os dois")
            };
        }
        let n = l.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(400));
        l.fetch_sub(1, Ordering::SeqCst);
        text(&format!("resposta de {prompt}"))
    });

    let e = env(Mode::Default);
    let reg = Registry::new();
    let mut events = Vec::new();
    let report = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |ev| events.push(ev),
    )
    .unwrap();
    assert_eq!(report.text, "li os dois");
    assert_eq!(peak.load(Ordering::SeqCst), 2, "children did not overlap");

    // Both results reached the parent, in the model's order.
    let second = parent_second.lock().unwrap().clone().unwrap();
    let results = tool_msgs(&second);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_call_id"], "c1");
    assert_eq!(results[0]["content"], "resposta de FILHO-A");
    assert_eq!(results[1]["content"], "resposta de FILHO-B");

    // Isolation: the parent's history holds the summaries, not the children's
    // conversations; those live in their own sessions, linked to the parent.
    let parent_rows = e.store.messages(&e.sid, 100, 0).unwrap();
    assert_eq!(parent_rows.len(), 5, "user, assistant, 2 tool, assistant");
    let kids = e.store.subagent_sessions(&e.sid).unwrap();
    assert_eq!(kids.len(), 2);
    for k in &kids {
        assert_eq!(k.source, SessionSource::Subagent);
        assert_eq!(k.source_id.as_deref(), Some(e.sid.as_str()));
        let rows = e.store.messages(&k.id, 100, 0).unwrap();
        assert_eq!(rows[0].role, Role::User);
        assert!(rows[0].content.starts_with("FILHO-"));
        assert!(rows
            .last()
            .unwrap()
            .content
            .starts_with("resposta de FILHO-"));
    }

    // The UI hears about both: running (with live progress) then done.
    let ev = subagent_events(&events);
    let ran: std::collections::HashSet<&String> = ev
        .iter()
        .filter(|(_, s)| s == "running")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ran.len(), 2, "{ev:?}");
    assert_eq!(ev.iter().filter(|(_, s)| s == "done").count(), 2, "{ev:?}");
}

#[test]
fn the_process_wide_gate_caps_children_at_the_limit() {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (l, p) = (live.clone(), peak.clone());
    let url = serve(move |b| {
        let prompt = prompt_of(b);
        if prompt == "PAI" {
            return if tool_msgs(b).is_empty() {
                calls(&[
                    (
                        "c1",
                        "agent",
                        json!({"description": "a", "prompt": "K1", "agent": "explore"}),
                    ),
                    (
                        "c2",
                        "agent",
                        json!({"description": "b", "prompt": "K2", "agent": "explore"}),
                    ),
                    (
                        "c3",
                        "agent",
                        json!({"description": "c", "prompt": "K3", "agent": "explore"}),
                    ),
                ])
            } else {
                text("ok")
            };
        }
        let n = l.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        l.fetch_sub(1, Ordering::SeqCst);
        text("pronto")
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let opts = AgentOptions {
        subagents: SubagentOptions {
            max_concurrent: Some(1),
            // A gate of its own: other tests in this binary use the global one.
            gate: Some(zerith_runtime::Gate::new()),
            depth: 0,
        },
        ..AgentOptions::default()
    };
    turn(
        &e,
        &url,
        &reg,
        opts,
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    assert_eq!(e.store.subagent_sessions(&e.sid).unwrap().len(), 3);
}

#[test]
fn explore_cannot_write_even_when_the_policy_would_allow_it() {
    let tools_seen: Arc<Mutex<Vec<String>>> = Default::default();
    let ts = tools_seen.clone();
    let url = serve(move |b| match prompt_of(b).as_str() {
        "PAI" => {
            if tool_msgs(b).is_empty() {
                calls(&[(
                    "c1",
                    "agent",
                    json!({"description": "x", "prompt": "EXPLORA", "agent": "explore"}),
                )])
            } else {
                text("fim")
            }
        }
        _ => {
            if tool_msgs(b).is_empty() {
                *ts.lock().unwrap() = tool_names(b);
                calls(&[("w1", "write_file", json!({"path": "x.txt", "content": "c"}))])
            } else {
                text(&format!(
                    "resultado: {}",
                    tool_msgs(b)[0]["content"].as_str().unwrap()
                ))
            }
        }
    });
    // Bypass: the policy allows everything, so only the allowlist can stop it.
    let e = env(Mode::Bypass);
    let reg = Registry::new();
    let mut events = Vec::new();
    turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |ev| events.push(ev),
    )
    .unwrap();
    assert_eq!(
        *tools_seen.lock().unwrap(),
        ["read_file", "list_dir", "glob", "grep"]
    );
    assert!(
        !e.dir.path().join("x.txt").exists(),
        "the child wrote a file"
    );
    let kid = &e.store.subagent_sessions(&e.sid).unwrap()[0];
    let last = e.store.messages(&kid.id, 100, 0).unwrap().pop().unwrap();
    assert!(
        last.content.contains("no such tool: write_file"),
        "{}",
        last.content
    );
}

#[test]
fn the_parents_middleware_denies_inside_the_child() {
    struct NoWrites(Arc<Mutex<Vec<String>>>);
    impl Middleware for NoWrites {
        fn before(&self, call: &mut ToolInvocation) -> Flow {
            self.0.lock().unwrap().push(call.session_id.clone());
            if call.name == "write_file" {
                Flow::Deny("regra do usuário".into())
            } else {
                Flow::Continue
            }
        }
    }
    let url = serve(|b| match (prompt_of(b).as_str(), tool_msgs(b).first()) {
        ("PAI", None) => calls(&[(
            "c1",
            "agent",
            json!({"description": "x", "prompt": "ESCREVE", "agent": "general"}),
        )]),
        ("ESCREVE", None) => {
            calls(&[("w1", "write_file", json!({"path": "x.txt", "content": "c"}))])
        }
        (_, Some(m)) => text(&format!("visto: {}", m["content"].as_str().unwrap())),
        _ => text("?"),
    });
    // Bypass: the policy allows the write, so only the middleware can stop it.
    let e = env(Mode::Bypass);
    let reg = Registry::new();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            middleware: vec![Box::new(NoWrites(seen.clone()))],
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(
        !e.dir.path().join("x.txt").exists(),
        "the child bypassed the middleware"
    );
    let kid = &e.store.subagent_sessions(&e.sid).unwrap()[0];
    let last = e.store.messages(&kid.id, 100, 0).unwrap().pop().unwrap();
    assert!(
        last.content.contains("negado: regra do usuário"),
        "{}",
        last.content
    );
    // The same instance saw the parent's `agent` call and the child's write.
    assert_eq!(*seen.lock().unwrap(), [e.sid.clone(), kid.id.clone()]);
}

#[test]
fn depth_is_capped_at_two_levels() {
    type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;
    let seen: Seen = Default::default();
    let s = seen.clone();
    let url = serve(move |b| {
        let prompt = prompt_of(b);
        if tool_msgs(b).is_empty() {
            s.lock().unwrap().push((prompt.clone(), tool_names(b)));
            let next = match prompt.as_str() {
                "N0" => Some("N1"),
                "N1" => Some("N2"),
                "N2" => Some("N3"),
                _ => None,
            };
            if let Some(n) = next {
                return calls(&[(
                    "c",
                    "agent",
                    json!({"description": n, "prompt": n, "agent": "general"}),
                )]);
            }
        }
        text(&format!("{prompt} concluiu"))
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "N0",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let seen = seen.lock().unwrap();
    let has_agent = |p: &str| {
        seen.iter()
            .find(|(n, _)| n == p)
            .map(|(_, t)| t.iter().any(|x| x == "agent"))
            .unwrap_or_else(|| panic!("{p} never ran: {seen:?}"))
    };
    assert!(has_agent("N0"), "root may spawn");
    assert!(has_agent("N1"), "a child may spawn");
    assert!(!has_agent("N2"), "a grandchild must not have the tool");
    assert!(
        !seen.iter().any(|(n, _)| n == "N3"),
        "a great-grandchild must not exist"
    );
}

#[test]
fn a_turn_already_at_the_depth_limit_refuses_the_call() {
    let url = serve(|b| {
        if tool_msgs(b).is_empty() {
            calls(&[("c", "agent", json!({"description": "d", "prompt": "X"}))])
        } else {
            text(&format!(
                "visto: {}",
                tool_msgs(b)[0]["content"].as_str().unwrap()
            ))
        }
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let report = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: SubagentOptions {
                depth: 2,
                ..subs(2)
            },
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "Y",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(
        report.text.contains("profundidade máxima"),
        "{}",
        report.text
    );
    assert!(e.store.subagent_sessions(&e.sid).unwrap().is_empty());
}

#[test]
fn cancelling_the_parent_cancels_the_child() {
    let url = serve(|b| match prompt_of(b).as_str() {
        "PAI" => calls(&[(
            "c1",
            "agent",
            json!({"description": "lento", "prompt": "LENTO", "agent": "explore"}),
        )]),
        _ => slow("comecei", Duration::from_millis(1500)),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        c2.cancel();
    });
    let mut events = Vec::new();
    let t = Instant::now();
    let err = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &cancel,
        |ev| events.push(ev),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            RunError::Provider(zerith_providers::ProviderError::Cancelled)
        ),
        "{err}"
    );
    assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
    let ev = subagent_events(&events);
    assert!(ev.iter().any(|(_, s)| s == "cancelled"), "{ev:?}");
    // The child is recorded, and the parent's call is closed (history paired).
    assert_eq!(e.store.subagent_sessions(&e.sid).unwrap().len(), 1);
    let rows = e.store.messages(&e.sid, 100, 0).unwrap();
    assert_eq!(rows.last().unwrap().role, Role::Tool);
}

#[test]
fn a_failing_child_is_a_tool_error_and_the_parent_carries_on() {
    let url = serve(|b| match prompt_of(b).as_str() {
        "PAI" => {
            if tool_msgs(b).is_empty() {
                calls(&[(
                    "c1",
                    "agent",
                    json!({"description": "x", "prompt": "QUEBRA", "agent": "explore"}),
                )])
            } else {
                text(&format!(
                    "o filho disse: {}",
                    tool_msgs(b)[0]["content"].as_str().unwrap()
                ))
            }
        }
        _ => bad_request(),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let mut events = Vec::new();
    let report = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |ev| events.push(ev),
    )
    .unwrap();
    assert!(report.text.contains("subagente falhou"), "{}", report.text);
    assert!(events
        .iter()
        .any(|ev| matches!(ev, RunEvent::ToolResult { ok: false, name, .. } if name == "agent")));
    let ev = subagent_events(&events);
    assert!(ev.iter().any(|(_, s)| s == "failed"), "{ev:?}");
}

#[test]
fn an_unknown_agent_name_lists_what_exists() {
    let url = serve(|b| {
        if tool_msgs(b).is_empty() {
            calls(&[(
                "c1",
                "agent",
                json!({"description": "x", "prompt": "p", "agent": "fantasma"}),
            )])
        } else {
            text(tool_msgs(b)[0]["content"].as_str().unwrap())
        }
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(
        r.text.contains("agente desconhecido: fantasma") && r.text.contains("explore"),
        "{}",
        r.text
    );
}

#[test]
fn a_background_agent_returns_an_id_and_agent_result_collects_it() {
    let url = serve(|b| match prompt_of(b).as_str() {
        "PAI" => match tool_msgs(b).len() {
            0 => calls(&[(
                "c1",
                "agent",
                json!({"description": "bg", "prompt": "BG", "agent": "explore", "run_in_background": true}),
            )]),
            1 => {
                // The id is in the first tool result: "sub-N iniciado ...".
                let first = tool_msgs(b)[0]["content"].as_str().unwrap().to_string();
                let id = first.split_whitespace().next().unwrap().to_string();
                std::thread::sleep(Duration::from_millis(800));
                calls(&[("c2", "agent_result", json!({"id": id}))])
            }
            _ => text(&format!(
                "fim: {}",
                tool_msgs(b)[1]["content"].as_str().unwrap()
            )),
        },
        _ => text("resultado do fundo"),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(r.text, "fim: resultado do fundo");
}

#[test]
fn a_background_child_the_model_forgets_is_awaited_and_its_result_handed_back() {
    // The model starts a background child and answers right away, never
    // calling agent_result. The turn must not end with the child running
    // unseen: it waits, feeds the result back, and the model finishes with it.
    let url = serve(|b| match prompt_of(b).as_str() {
        "PAI" => {
            let msgs = messages(b);
            let joined = msgs
                .iter()
                .filter_map(|m| m["content"].as_str())
                .find(|c| c.contains("subagentes em segundo plano terminaram"));
            match (tool_msgs(b).len(), joined) {
                (0, _) => calls(&[(
                    "c1",
                    "agent",
                    json!({"description": "fundo", "prompt": "BG", "agent": "explore", "run_in_background": true}),
                )]),
                (_, None) => text("já volto"),
                (_, Some(r)) => text(&format!("final com: {}", r.contains("resultado esquecido"))),
            }
        }
        _ => slow("resultado esquecido", Duration::from_millis(400)),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let mut events = Vec::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |ev| events.push(ev),
    )
    .unwrap();
    assert_eq!(r.text, "final com: true");
    let states: Vec<String> = subagent_events(&events)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert!(states.contains(&"done".to_string()), "{states:?}");
    // progress was shown while it ran, not only at the end
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, RunEvent::Subagent { state, summary, .. }
        if state == "running" && summary.starts_with("passo "))),
        "{states:?}"
    );
}

#[test]
fn a_turn_that_stops_takes_its_background_children_with_it_and_says_so() {
    // The parent is stopped while its background child is still working:
    // the child is cancelled and the person sees it, nothing keeps running.
    let url = serve(move |b| match prompt_of(b).as_str() {
        // Deterministic on any machine: one call starts the child, then the
        // parent answers and waits for it, and the stop lands in that wait.
        "PAI" if tool_msgs(b).is_empty() => calls(&[(
            "c1",
            "agent",
            json!({"description": "longo", "prompt": "LONGO", "agent": "explore", "run_in_background": true}),
        )]),
        "PAI" => text("já volto"),
        _ => slow("nunca", Duration::from_secs(5)),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let cancel = Cancel::default();
    let stopper = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        stopper.cancel();
    });
    let mut events = Vec::new();
    let started = Instant::now();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            max_steps: 50,
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &cancel,
        |ev| events.push(ev),
    );
    assert!(r.is_err(), "a stopped turn is an error");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "no wait for the child"
    );
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, RunEvent::Subagent { state, .. } if state == "cancelled")),
        "the person is told the child was stopped"
    );
}

#[test]
fn the_task_list_persists_and_every_change_emits_the_whole_list() {
    let url = serve(|b| match tool_msgs(b).len() {
        0 => calls(&[("t1", "task_create", json!({"subject": "ler o código"}))]),
        1 => calls(&[(
            "t2",
            "task_create",
            json!({"subject": "escrever o teste", "description": "d"}),
        )]),
        2 => calls(&[(
            "t3",
            "task_update",
            json!({"id": 1, "status": "in_progress"}),
        )]),
        3 => calls(&[("t4", "task_list", json!({}))]),
        _ => text(&format!(
            "lista: {}",
            tool_msgs(b)[3]["content"].as_str().unwrap()
        )),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let mut events = Vec::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions::default(),
        TurnContext::default(),
        "TAREFAS",
        &Cancel::default(),
        |ev| events.push(ev),
    )
    .unwrap();
    assert!(
        r.text.contains("#1 [in_progress] ler o código"),
        "{}",
        r.text
    );
    assert!(
        r.text.contains("#2 [pending] escrever o teste"),
        "{}",
        r.text
    );

    let snaps: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Tasks { items } => Some(items.clone()),
            _ => None,
        })
        .collect();
    // create, create, update: three mutations, three snapshots (list adds none).
    assert_eq!(snaps.len(), 3, "{snaps:?}");
    assert_eq!(snaps[0].len(), 1);
    assert_eq!(snaps[1].len(), 2);
    assert_eq!(snaps[2][0].status, TodoStatus::InProgress);

    // Durable: a new connection sees the same list.
    let again = Store::open(e.dir.path().join("t.db")).unwrap();
    assert_eq!(again.todo_list(&e.sid).unwrap().len(), 2);
    // The system prompt taught the model the working method.
}

#[test]
fn the_system_prompt_teaches_tasks_subagents_and_read_before_edit() {
    let sys: Arc<Mutex<String>> = Default::default();
    let s = sys.clone();
    let url = serve(move |b| {
        *s.lock().unwrap() = system_of(b);
        text("ok")
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    turn(
        &e,
        &url,
        &reg,
        AgentOptions::default(),
        TurnContext::default(),
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let sys = sys.lock().unwrap();
    assert!(sys.contains("exatamente uma tarefa `in_progress`"), "{sys}");
    assert!(sys.contains("`agent`") && sys.contains("`ask_user`"));
    assert!(sys.contains("read_file") && sys.contains("antes de alterá-lo"));
}

#[test]
fn ask_user_blocks_on_the_answer_and_hands_it_to_the_model() {
    let url = serve(|b| {
        if tool_msgs(b).is_empty() {
            calls(&[(
                "q1",
                "ask_user",
                json!({"questions": [{
                "question": "Qual banco?", "header": "Banco",
                "options": [{"label": "SQLite", "description": "local"}, {"label": "Postgres"}]}]}),
            )])
        } else {
            text(&format!(
                "escolheu: {}",
                tool_msgs(b)[0]["content"].as_str().unwrap()
            ))
        }
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let (tx, gate) = question_channel();
    let mut events = Vec::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            questions: Some(&gate),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "decida",
        &Cancel::default(),
        |ev| {
            if let RunEvent::Question { id, questions } = &ev {
                assert_eq!(questions[0].options.len(), 2);
                assert!(tx.answer(*id, vec!["SQLite".into()]));
            }
            events.push(ev);
        },
    )
    .unwrap();
    assert_eq!(r.text, "escolheu: Qual banco? -> SQLite");
    assert!(events
        .iter()
        .any(|ev| matches!(ev, RunEvent::Question { .. })));
}

#[test]
fn ask_user_without_a_gate_fails_closed() {
    let url = serve(|b| {
        if tool_msgs(b).is_empty() {
            calls(&[(
                "q1",
                "ask_user",
                json!({"questions": [{
                "question": "?", "options": [{"label": "a"}, {"label": "b"}]}]}),
            )])
        } else {
            text(tool_msgs(b)[0]["content"].as_str().unwrap())
        }
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions::default(),
        TurnContext::default(),
        "x",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(r.text.contains("o usuário não respondeu"), "{}", r.text);
}

#[test]
fn the_skill_tool_loads_a_body_on_demand_and_records_allowed_tools() {
    let first_system: Arc<Mutex<String>> = Default::default();
    let fs = first_system.clone();
    let url = serve(move |b| {
        if tool_msgs(b).is_empty() {
            *fs.lock().unwrap() = system_of(b);
            calls(&[("s1", "skill", json!({"name": "demo"}))])
        } else {
            text(&format!(
                "skill: {}",
                tool_msgs(b)[0]["content"].as_str().unwrap()
            ))
        }
    });
    let e = env(Mode::Default);
    let src = e.dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("SKILL.md"),
        "---\nname: demo\ndescription: d-demo\nlicense: MIT\nallowed-tools: Read Grep\n---\n\nCORPO-SECRETO\n",
    )
    .unwrap();
    let lib = zerith_skills::Library::open(e.dir.path().join("skills")).unwrap();
    lib.install(&src, "t").unwrap();
    let reg = Registry::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions::default(),
        TurnContext {
            skills: Some(&lib),
            ..TurnContext::default()
        },
        // The old heuristic would have inlined the body for this prompt.
        "use a skill demo",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let sys = first_system.lock().unwrap();
    assert!(sys.contains("- demo: d-demo"), "{sys}");
    assert!(
        !sys.contains("CORPO-SECRETO"),
        "the body must not be in the prompt"
    );
    assert!(r.text.contains("CORPO-SECRETO"), "{}", r.text);
    assert!(r.text.contains("allowed-tools: Read, Grep"), "{}", r.text);
    assert_eq!(
        reg.host().skill_allowed_tools("demo"),
        Some(vec!["Read".to_string(), "Grep".to_string()])
    );
}

#[test]
fn an_unknown_skill_is_a_tool_error() {
    let url = serve(|b| {
        if tool_msgs(b).is_empty() {
            calls(&[("s1", "skill", json!({"name": "nada"}))])
        } else {
            text(tool_msgs(b)[0]["content"].as_str().unwrap())
        }
    });
    let e = env(Mode::Default);
    let lib = zerith_skills::Library::open(e.dir.path().join("skills")).unwrap();
    let reg = Registry::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions::default(),
        TurnContext {
            skills: Some(&lib),
            ..TurnContext::default()
        },
        "x",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(r.text.contains("skill não encontrada"), "{}", r.text);
}

// ---- the plan runner on top of the same machinery ----------------------------------

/// ZERITH_BASE_URL is process-wide and only the plan runner reads it, so the
/// plan tests share one server and one lock.
fn plan_env(url: &str) -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    hermetic();
    std::env::set_var("ZERITH_PROVIDER", "custom");
    std::env::set_var("ZERITH_BASE_URL", url);
    std::env::set_var("ZERITH_API_KEY", "k");
    g
}

fn plan_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("zerith.db");
    (dir, db)
}

#[test]
fn plan_tasks_run_as_subagents_and_after_orders_them() {
    let order: Arc<Mutex<Vec<String>>> = Default::default();
    let o = order.clone();
    let url = serve(move |b| {
        let p = prompt_of(b);
        o.lock().unwrap().push(format!("start {p}"));
        std::thread::sleep(Duration::from_millis(200));
        o.lock().unwrap().push(format!("end {p}"));
        text(&format!("feito {p}"))
    });
    let _g = plan_env(&url);
    let (_d, db) = plan_db();
    let store = Store::open(&db).unwrap();
    store
        .create_plan("p1", &parse_plan("p1", "a | PA\nb | PB | after: 1\nc | PC"))
        .unwrap();
    let r = run_plan(&db, "p1", "m", 3, None, |_, _| {}).unwrap();
    assert_eq!((r.done, r.failed, r.skipped), (3, 0, 0), "{:?}", r.tasks);
    let b = r.tasks.iter().find(|t| t.title == "b").unwrap();
    assert_eq!(b.result.as_deref(), Some("feito PB"));

    // B started only after A ended.
    let order = order.lock().unwrap();
    let pos = |s: &str| order.iter().position(|x| x == s).unwrap();
    assert!(pos("end PA") < pos("start PB"), "{order:?}");

    // Every task ran in its own subagent session, linked to the plan.
    let kids = store.subagent_sessions("p1").unwrap();
    assert_eq!(kids.len(), 3);
    for t in &r.tasks {
        let sid = t.session_id.as_deref().unwrap();
        assert!(kids.iter().any(|k| k.id == sid));
    }
}

#[test]
fn plan_time_budget_fails_the_task_with_tempo_esgotado() {
    let url = serve(|_| slow("devagar", Duration::from_millis(2500)));
    let _g = plan_env(&url);
    let (_d, db) = plan_db();
    Store::open(&db)
        .unwrap()
        .create_plan("p2", &parse_plan("p2", "a | LENTO"))
        .unwrap();
    let opts = PlanOptions {
        concurrency: 1,
        budget: TaskBudget {
            max_steps: None,
            max_time: Some(Duration::from_millis(400)),
        },
        ..PlanOptions::default()
    };
    let t = Instant::now();
    let r = run_plan_opts(&db, "p2", "m", None, opts, |_, _| {}).unwrap();
    assert!(t.elapsed() < Duration::from_secs(8));
    assert_eq!(r.failed, 1);
    assert_eq!(r.tasks[0].error.as_deref(), Some("tempo esgotado"));
}

#[test]
fn plan_step_budget_fails_a_task_that_never_answers() {
    // The model keeps asking for a tool; one step is not enough to answer.
    let url = serve(|_| calls(&[("c", "list_dir", json!({"path": "."}))]));
    let _g = plan_env(&url);
    let (_d, db) = plan_db();
    Store::open(&db)
        .unwrap()
        .create_plan("p3", &parse_plan("p3", "a | LOOP"))
        .unwrap();
    let opts = PlanOptions {
        concurrency: 1,
        budget: TaskBudget {
            max_steps: Some(2),
            max_time: None,
        },
        ..PlanOptions::default()
    };
    let r = run_plan_opts(&db, "p3", "m", None, opts, |_, _| {}).unwrap();
    assert_eq!(r.failed, 1);
    let err = r.tasks[0].error.clone().unwrap();
    assert!(err.contains("limite de 2 passos"), "{err}");
}

// ---- multi-model delegation ------------------------------------------------

fn anthropic_tool_sse(id: &str, name: &str, input: &Value) -> Reply {
    let events = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":12}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}),
        json!({"type":"message_stop"}),
    ];
    let mut s = String::from(HEAD);
    for e in events {
        s.push_str(&format!(
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap()
        ));
    }
    Reply::Whole(s)
}

fn anthropic_text_sse(txt: &str) -> Reply {
    let events = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":20}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":txt}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
        json!({"type":"message_stop"}),
    ];
    let mut s = String::from(HEAD);
    for e in events {
        s.push_str(&format!(
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap()
        ));
    }
    Reply::Whole(s)
}

#[test]
fn multi_model_delegation_routes_subagent_to_deepseek_and_tracks_roles() {
    static KEY_ONCE: Once = Once::new();
    KEY_ONCE.call_once(|| {
        std::env::set_var("ZERITH_API_KEY", "k");
    });

    let seen_a: Arc<Mutex<Vec<Value>>> = Default::default();
    let seen_b: Arc<Mutex<Vec<Value>>> = Default::default();

    let sa = seen_a.clone();
    let url_a = serve(move |b| {
        sa.lock().unwrap().push(b.clone());
        let has_tool_result = b["messages"].as_array().is_some_and(|ms| {
            ms.iter().any(|m| {
                m["content"]
                    .as_array()
                    .is_some_and(|arr| arr.iter().any(|blk| blk["type"] == "tool_result"))
            })
        });
        if !has_tool_result {
            anthropic_tool_sse(
                "tu1",
                "agent",
                &json!({"description": "check", "prompt": "SUB-DEEPSEEK", "agent": "explore"}),
            )
        } else {
            let reply = b["messages"]
                .as_array()
                .and_then(|ms| ms.last())
                .and_then(|m| m["content"].as_array())
                .and_then(|arr| arr.first())
                .and_then(|blk| blk["content"].as_str())
                .unwrap_or("");
            anthropic_text_sse(&format!("pai concluiu: {reply}"))
        }
    });

    let sb = seen_b.clone();
    let url_b = serve(move |b| {
        sb.lock().unwrap().push(b.clone());
        text("achado-deepseek")
    });

    let e = env(Mode::Default);
    let reg = Registry::new();

    let mut settings = zerith_core::Settings {
        accounts: vec![
            zerith_core::AccountEntry {
                id: "acc-anth".into(),
                provider: "anthropic".into(),
                base_url: Some(url_a.clone()),
                priority: 0,
                enabled: true,
                ..Default::default()
            },
            zerith_core::AccountEntry {
                id: "acc-ds".into(),
                provider: "deepseek".into(),
                base_url: Some(url_b.clone()),
                priority: 0,
                enabled: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    settings.routing.subagent_model = Some("deepseek/deepseek-4.1-flash".into());

    let mut parent_cfg = ProviderConfig::new(ProviderKind::Anthropic).with_key("k");
    parent_cfg.base_url = url_a;
    parent_cfg.timeout_ms = 10_000;
    let parent_client = Client::new(parent_cfg).unwrap();

    let report = run_agent_turn(
        &e.store,
        &parent_client,
        TurnContext {
            settings: Some(&settings),
            ..TurnContext::default()
        },
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&e.policy),
            subagents: subs(2),
            ..AgentOptions::default()
        },
        &e.sid,
        "claude-opus-5-5",
        "delegue",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();

    assert_eq!(report.text, "pai concluiu: achado-deepseek");
    assert_eq!(report.account_id.as_deref(), Some("acc-anth"));
    assert_eq!(report.model_used.as_deref(), Some("claude-opus-5-5"));

    let sa = seen_a.lock().unwrap();
    assert_eq!(sa.len(), 2);
    assert!(sa.iter().all(|r| r["model"] == "claude-opus-5-5"));

    let sb = seen_b.lock().unwrap();
    assert_eq!(sb.len(), 1);
    assert_eq!(sb[0]["model"], "deepseek-4.1-flash");

    let summary = e.store.usage_summary(24).unwrap();
    assert!(
        summary.models.iter().any(|m| {
            m.model == "claude-opus-5-5"
                && m.provider == "anthropic"
                && m.role_kind == "main"
                && m.requests == 2
        }),
        "{:?}",
        summary.models
    );
    assert!(
        summary.models.iter().any(|m| {
            m.model == "deepseek-4.1-flash"
                && m.provider == "deepseek"
                && m.role_kind == "subagent"
                && m.requests == 1
        }),
        "{:?}",
        summary.models
    );
}

/// A child must never be able to write skills or agents: what it makes would
/// outlive it and reach the person as if it were theirs.
#[test]
fn a_subagent_registry_has_no_authoring_tools() {
    use zerith_runtime::subagent::{child_registry, resolve_agent};
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let mut base = zerith_tools::Registry::new();
    for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
        paths: zerith_core::Paths::with_root(dir.path()),
        project_dir: None,
        tools: vec!["read_file".into()],
        mode: "default".into(),
    }) {
        base.register(t);
    }
    // The plan tools flip the parent's shared mode: never a child's.
    let mut pol = zerith_security::Policy::new(
        zerith_security::preset("workspace-write").unwrap(),
        zerith_security::Mode::Default,
        dir.path(),
    );
    let guards =
        zerith_runtime::guards::Guards::new(zerith_core::Settings::default(), &mut pol, "s");
    base.register(guards.enter_plan_mode_tool());
    base.register(guards.exit_plan_mode_tool());
    assert!(base.get("create_agent").is_some(), "the parent has them");
    // `general` is the broadest child: every tool of the parent, minus these.
    let resolved = resolve_agent(dir.path(), Some("general")).unwrap();
    let child = child_registry(&base, &resolved, 1);
    for name in [
        "create_skill",
        "create_agent",
        "delete_ai_item",
        "enter_plan_mode",
        "exit_plan_mode",
    ] {
        assert!(child.get(name).is_none(), "{name} leaked into a subagent");
    }
    assert!(child.get("read_file").is_some());
}

/// A `general` child in the default (ask) mode writes a file and runs a shell
/// command. Both prompts reach the parent's chat through the parent's gate;
/// what the person answers is what happens.
fn delegated_write_and_shell(answer: Approval) -> (Env, Vec<RunEvent>, String) {
    let url = serve(move |b| match prompt_of(b).as_str() {
        "PAI" => {
            if tool_msgs(b).is_empty() {
                calls(&[(
                    "c1",
                    "agent",
                    json!({"description": "escreve", "prompt": "FILHO", "agent": "general"}),
                )])
            } else {
                text(tool_msgs(b)[0]["content"].as_str().unwrap())
            }
        }
        _ => {
            if tool_msgs(b).is_empty() {
                calls(&[
                    ("w1", "write_file", json!({"path": "x.txt", "content": "c"})),
                    ("s1", "shell", json!({"command": "echo oi> y.txt"})),
                ])
            } else {
                let outs: Vec<String> = tool_msgs(b)
                    .iter()
                    .map(|m| m["content"].as_str().unwrap().to_string())
                    .collect();
                text(&outs.join(" | "))
            }
        }
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let (tx, gate) = approval_channel();
    let mut events = Vec::new();
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            approvals: Some(&gate),
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext::default(),
        "PAI",
        &Cancel::default(),
        |ev| {
            if let RunEvent::ApprovalNeeded { id, .. } = &ev {
                assert!(tx.answer(*id, answer));
            }
            events.push(ev);
        },
    )
    .unwrap();
    (e, events, r.text)
}

fn approvals_asked(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::ApprovalNeeded { tool, summary, .. } => {
                assert!(summary.starts_with("[subagente]"), "{summary}");
                Some(tool.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_child_in_ask_mode_asks_the_person_through_the_parents_gate() {
    let (e, events, text) = delegated_write_and_shell(Approval::AllowOnce);
    assert_eq!(approvals_asked(&events), ["write_file", "shell"]);
    assert!(e.dir.path().join("x.txt").exists(), "{text}");
    assert!(e.dir.path().join("y.txt").exists(), "{text}");
}

#[test]
fn a_child_in_ask_mode_is_refused_when_the_person_denies() {
    let (e, events, text) = delegated_write_and_shell(Approval::Deny);
    assert_eq!(approvals_asked(&events), ["write_file", "shell"]);
    assert!(!e.dir.path().join("x.txt").exists());
    assert!(!e.dir.path().join("y.txt").exists());
    assert_eq!(text.matches("negado pelo usuário").count(), 2, "{text}");
}

#[test]
fn a_delegating_turn_reports_what_its_children_saved() {
    let url = serve(|b| match prompt_of(b).as_str() {
        "PAI" if tool_msgs(b).is_empty() => calls(&[(
            "c1",
            "agent",
            json!({"description": "x", "prompt": "FILHO", "agent": "explore"}),
        )]),
        "PAI" => text("ok"),
        // Prose with real usage: Caveman counts a saving on the child's request.
        _ => Reply::Whole(format!(
            "{HEAD}{}{}data: [DONE]\n\n",
            chunk(&json!({"choices":[{"delta":{"content":"o filho respondeu em prosa"}}]})),
            chunk(
                &json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":100,"total_tokens":105}})
            )
        )),
    });
    let e = env(Mode::Default);
    let reg = Registry::new();
    let settings = zerith_core::Settings {
        token_saver: zerith_core::TokenSaverSettings {
            caveman: zerith_core::CavemanLevel::Full,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = turn(
        &e,
        &url,
        &reg,
        AgentOptions {
            subagents: subs(2),
            ..AgentOptions::default()
        },
        TurnContext {
            settings: Some(&settings),
            ..TurnContext::default()
        },
        "PAI",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    // One row per request, as before: the child's own row holds its saving.
    let rows: Vec<(String, i64)> = {
        let mut st = e
            .store
            .conn()
            .prepare("SELECT session_id, saved_caveman_tokens FROM usage_records")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let parent_own: i64 = rows.iter().filter(|r| r.0 == e.sid).map(|r| r.1).sum();
    let child: i64 = rows.iter().filter(|r| r.0 != e.sid).map(|r| r.1).sum();
    assert!(child > 0, "{rows:?}");
    assert_eq!(
        r.saved_caveman_tokens as i64,
        parent_own + child,
        "{rows:?}"
    );
    assert_eq!(
        r.saved_tokens,
        r.saved_rtk_tokens + r.saved_ponytail_tokens + r.saved_caveman_tokens
    );
    // And what the child spent: the report used to show the root's requests
    // only, so a delegating turn looked cheaper than it was.
    let (prompt, completion): (i64, i64) = e
        .store
        .conn()
        .query_row(
            "SELECT SUM(prompt_tokens), SUM(completion_tokens) FROM usage_records",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(completion >= 100, "the child's 100 tokens are in the table");
    assert_eq!(r.prompt_tokens as i64, prompt, "parent + child prompt");
    assert_eq!(
        r.completion_tokens as i64, completion,
        "parent + child output"
    );
}
