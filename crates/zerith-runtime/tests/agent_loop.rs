//! The tool loop against a canned HTTP server on 127.0.0.1. No network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use zerith_providers::{Cancel, Client, Protocol, ProviderConfig, ProviderKind, RetryPolicy};
use zerith_runtime::{
    approval_channel, run_agent_turn, run_turn_cancellable, AgentOptions, Approval, RunError,
    RunEvent, TurnContext,
};
use zerith_security::{preset, Mode, Policy};
use zerith_sessions::{NewMessage, Role, SessionSource, Store};
use zerith_tools::Registry;

/// Serves `responses` in order, one per connection, and records each request
/// as (path, json body).
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

fn serve(responses: Vec<String>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut r = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
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
            log.lock()
                .unwrap()
                .push((path, serde_json::from_slice(&body).unwrap_or(Value::Null)));
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    Fake { url, seen }
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

fn calls(list: &[(&str, &str, Value)]) -> String {
    let frags: Vec<Value> = list
        .iter()
        .enumerate()
        .map(|(i, (id, name, args))| {
            json!({"index": i, "id": id, "function": {"name": name, "arguments": args.to_string()}})
        })
        .collect();
    sse(&[json!({"choices":[{"delta":{"tool_calls": frags}}]})])
}

fn status(code: u16, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} X\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn client(url: &str, protocol: Protocol) -> Client {
    let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("k");
    cfg.base_url = url.into();
    cfg.protocol = protocol;
    cfg.timeout_ms = 5_000;
    Client::new(cfg).unwrap().with_retry(RetryPolicy {
        max_retries: 2,
        base_ms: 1,
        max_ms: 5,
        max_retry_after_ms: 1_000,
    })
}

fn workspace() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

fn session(store: &Store) -> String {
    store.create_session("t", SessionSource::Native).unwrap().id
}

/// Runs one tool turn with the built-in registry and collects its events.
fn turn(
    store: &Store,
    client: &Client,
    policy: &Policy,
    sid: &str,
    on: impl FnMut(RunEvent),
) -> zerith_runtime::Result<zerith_runtime::TurnReport> {
    let reg = Registry::new();
    let opts = AgentOptions {
        tools: Some(&reg),
        policy: Some(policy),
        ..Default::default()
    };
    run_agent_turn(
        store,
        client,
        TurnContext::default(),
        opts,
        sid,
        "m",
        "faça",
        &Cancel::default(),
        on,
    )
}

fn rows(store: &Store, sid: &str) -> Vec<(Role, String)> {
    store
        .messages(sid, 100, 0)
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect()
}

#[test]
fn a_plain_answer_streams_and_is_stored_once() {
    let fake = serve(vec![text("olá")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let mut events = Vec::new();
    let report = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |e| events.push(e),
    )
    .unwrap();
    assert_eq!(report.text, "olá");
    assert_eq!(report.prompt_tokens, 10);
    assert!(events.contains(&RunEvent::Delta("olá".into())));
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::Usage {
            input: 10,
            output: 2,
            ..
        }
    )));
    assert_eq!(rows(&store, &sid).len(), 2);
    let req = &fake.seen.lock().unwrap()[0];
    assert_eq!(req.0, "/v1/chat/completions");
    assert!(
        req.1.get("tools").is_none(),
        "the plain turn offers no tools"
    );
}

#[test]
fn one_tool_call_round_trips_and_history_is_paired() {
    let dir = workspace();
    std::fs::write(dir.path().join("a.txt"), "conteudo secreto").unwrap();
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        text("li o arquivo"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let mut events = Vec::new();
    let report = turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        &policy,
        &sid,
        |e| events.push(e),
    )
    .unwrap();
    assert_eq!(report.text, "li o arquivo");
    assert!(events.iter().any(|e| matches!(e, RunEvent::ToolResult { ok: true, output_preview, .. } if output_preview.contains("conteudo secreto"))));
    assert!(events.contains(&RunEvent::Step { n: 2 }));

    let r = rows(&store, &sid);
    let roles: Vec<Role> = r.iter().map(|x| x.0).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );

    let seen = fake.seen.lock().unwrap();
    assert!(seen[0].1["tools"].as_array().unwrap().len() >= 8);
    let msgs = seen[1].1["messages"].as_array().unwrap();
    let n = msgs.len();
    assert_eq!(msgs[n - 2]["tool_calls"][0]["id"], "c1");
    assert_eq!(msgs[n - 1]["role"], "tool");
    assert_eq!(msgs[n - 1]["tool_call_id"], "c1");

    // Replayed in a later turn, the pair survives the round trip through SQLite.
    let fake2 = serve(vec![text("ok")]);
    turn(
        &store,
        &client(&fake2.url, Protocol::OpenAi),
        &policy,
        &sid,
        |_| {},
    )
    .unwrap();
    let seen2 = fake2.seen.lock().unwrap();
    let replayed = seen2[0].1["messages"].as_array().unwrap();
    let tool = replayed.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(tool["tool_call_id"], "c1");
}

#[test]
fn two_tool_calls_in_one_step_both_run_and_a_tool_error_does_not_end_the_turn() {
    let dir = workspace();
    std::fs::write(dir.path().join("a.txt"), "A").unwrap();
    let fake = serve(vec![
        calls(&[
            ("c1", "read_file", json!({"path": "a.txt"})),
            ("c2", "read_file", json!({"path": "missing.txt"})),
        ]),
        text("pronto"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let mut results = Vec::new();
    turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        &policy,
        &sid,
        |e| {
            if let RunEvent::ToolResult { id, ok, .. } = e {
                results.push((id, ok));
            }
        },
    )
    .unwrap();
    assert_eq!(results, [("c1".into(), true), ("c2".into(), false)]);
    let r = rows(&store, &sid);
    assert!(r[3].1.starts_with("erro:"), "{:?}", r[3]);
    assert_eq!(r.last().unwrap().1, "pronto");
}

fn approval_turn(answer: Approval) -> (Vec<RunEvent>, bool) {
    let dir = workspace();
    let fake = serve(vec![
        calls(&[(
            "w",
            "write_file",
            json!({"path": "novo.txt", "content": "x"}),
        )]),
        text("fim"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let (tx, gate) = approval_channel();
    let reg = Registry::new();
    let mut events = Vec::new();
    run_agent_turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        TurnContext::default(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            approvals: Some(&gate),
            ..Default::default()
        },
        &sid,
        "m",
        "escreva",
        &Cancel::default(),
        |e| {
            // A UI would answer from its own thread; answering from another
            // thread here proves the turn really blocks on the channel.
            if let RunEvent::ApprovalNeeded { id, .. } = &e {
                let (tx, id) = (tx.clone(), *id);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    tx.answer(id, answer);
                });
            }
            events.push(e);
        },
    )
    .unwrap();
    (events, dir.path().join("novo.txt").exists())
}

#[test]
fn an_approved_write_runs() {
    let (events, written) = approval_turn(Approval::AllowOnce);
    assert!(events
        .iter()
        .any(|e| matches!(e, RunEvent::ApprovalNeeded { tool, .. } if tool == "write_file")));
    assert!(written);
}

#[test]
fn a_denied_write_does_not_touch_the_disk_and_tells_the_model() {
    let (events, written) = approval_turn(Approval::Deny);
    assert!(!written);
    assert!(events.iter().any(|e| matches!(e, RunEvent::ToolResult { ok: false, output_preview, .. } if output_preview.contains("negado"))));
}

#[test]
fn without_an_approval_gate_ask_fails_closed() {
    let dir = workspace();
    let fake = serve(vec![
        calls(&[("s", "shell", json!({"command": "rm -rf alvo"}))]),
        text("ok"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let mut denied = false;
    turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        &policy,
        &sid,
        |e| {
            if let RunEvent::ToolResult { ok: false, .. } = e {
                denied = true;
            }
        },
    )
    .unwrap();
    assert!(denied);
}

#[test]
fn cancel_stops_the_stream_and_keeps_what_was_painted() {
    let fake = serve(vec![sse(&[
        json!({"choices":[{"delta":{"content":"um"}}]}),
        json!({"choices":[{"delta":{"content":"dois"}}]}),
        json!({"choices":[{"delta":{"content":"tres"}}]}),
    ])]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let cancel = Cancel::default();
    let err = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &cancel,
        |e| {
            if matches!(e, RunEvent::Delta(_)) {
                cancel.cancel();
            }
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        RunError::Provider(zerith_providers::ProviderError::Cancelled)
    ));
    // The painted text stays, marked as cut; the prompt is not left unanswered.
    let r = rows(&store, &sid);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[1].0, Role::Assistant);
    assert!(r[1].1.starts_with("um"), "{r:?}");
    assert!(r[1]
        .1
        .contains("_[resposta interrompida: parada pelo usuário]_"));
    let last = store.messages(&sid, 10, 0).unwrap().pop().unwrap();
    assert_eq!(last.raw.unwrap()["partial"], true);
    // and the next turn replays it as a plain answer, no dangling calls
    let replay = zerith_runtime::compact::replay(&store.recent_messages(&sid, 10).unwrap());
    assert_eq!(replay.len(), 2);
    assert!(replay[1].tool_calls.is_empty());
}

#[test]
fn a_stream_error_after_text_keeps_the_text_with_the_reason() {
    // the connection dies mid-answer: no [DONE], no finish_reason
    let cut = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
               data: {\"choices\":[{\"delta\":{\"content\":\"metade da \"}}]}\n\n"
        .to_string();
    let fake = serve(vec![cut]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let err = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            RunError::Provider(zerith_providers::ProviderError::Transport(_))
        ),
        "{err}"
    );
    let r = rows(&store, &sid);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r[1].1.starts_with("metade da "));
    assert!(r[1].1.contains("cortada antes do fim"), "{r:?}");
}

/// Claude stops at max_tokens inside a `write_file`: the call must not run
/// (its JSON is cut), the model is told why in a tool result, and the turn
/// goes on instead of looping on invalid JSON.
#[test]
fn a_tool_call_cut_by_the_output_cap_is_not_run() {
    let dir = workspace();
    let fake = serve(vec![
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":7}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu1","name":"write_file","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"big.txt\",\"content\":\"aaaa"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":32000}}),
            json!({"type":"message_stop"}),
        ]),
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":20}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"vou dividir"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ]),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let mut events = Vec::new();
    let report = turn(
        &store,
        &client(&fake.url, Protocol::Anthropic),
        &policy,
        &sid,
        |e| events.push(e),
    )
    .unwrap();
    assert_eq!(report.text, "vou dividir");
    assert!(
        !dir.path().join("big.txt").exists(),
        "half a file was written"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::ToolResult { ok: false, output_preview, .. }
            if output_preview.contains("cortada pelo limite de saída")
    )));
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen[0].1["max_tokens"], 8192,
        "non-Claude model name: safe default"
    );
    let msgs = seen[1].1["messages"].as_array().unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last["content"][0]["tool_use_id"], "tu1");
    assert!(last["content"][0]["content"]
        .as_str()
        .unwrap()
        .contains("escreva o arquivo em partes"));
}

#[test]
fn a_429_is_retried_before_anything_streams() {
    let fake = serve(vec![
        status(429, "Retry-After: 0\r\n", "slow down"),
        status(503, "", "busy"),
        text("depois"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let report = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.text, "depois");
    assert_eq!(fake.seen.lock().unwrap().len(), 3);
}

#[test]
fn a_429_that_never_clears_is_a_rate_limit_error() {
    let r = || status(429, "Retry-After: 0\r\n", "no");
    let fake = serve(vec![r(), r(), r()]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let err = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            RunError::Provider(zerith_providers::ProviderError::RateLimited { .. })
        ),
        "{err}"
    );
}

#[test]
fn a_malformed_answer_is_an_error_not_an_empty_reply() {
    let garbage = || status(200, "Content-Type: text/plain\r\n", "<html>proxy</html>");
    let fake = serve(vec![garbage(), garbage()]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let err = run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    );
    assert!(err.is_err());
    assert_eq!(rows(&store, &sid).len(), 1, "no assistant row was invented");
}

#[test]
fn a_long_session_replays_its_newest_messages_including_the_prompt() {
    let fake = serve(vec![text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    // 100 user messages: past the largest profile window (80).
    for i in 0..200 {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store
            .append(&sid, &NewMessage::new(role, format!("msg {i}")))
            .unwrap();
    }
    run_turn_cancellable(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        None,
        None,
        &sid,
        "m",
        "PERGUNTA ATUAL",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let seen = fake.seen.lock().unwrap();
    let msgs = seen[0].1["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["content"], "PERGUNTA ATUAL");
    assert!(msgs.iter().all(|m| m["content"] != "msg 0"));
    assert!(msgs.iter().any(|m| m["content"] == "msg 199"));
}

#[test]
fn the_history_option_bounds_what_the_model_sees() {
    let fake = serve(vec![text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    for i in 0..30 {
        store
            .append(&sid, &NewMessage::new(Role::User, format!("msg {i}")))
            .unwrap();
    }
    run_agent_turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        TurnContext::default(),
        AgentOptions {
            history: 5,
            ..Default::default()
        },
        &sid,
        "m",
        "AGORA",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let seen = fake.seen.lock().unwrap();
    let msgs = seen[0].1["messages"].as_array().unwrap();
    // System prompt plus the five newest rows, the prompt among them.
    assert_eq!(msgs.len(), 6);
    assert_eq!(msgs.last().unwrap()["content"], "AGORA");
    assert!(msgs.iter().all(|m| m["content"] != "msg 24"));
}

#[test]
fn the_step_limit_stops_a_model_that_never_stops_calling_tools() {
    let dir = workspace();
    std::fs::write(dir.path().join("a.txt"), "A").unwrap();
    let c = || calls(&[("c", "read_file", json!({"path": "a.txt"}))]);
    let fake = serve(vec![c(), c()]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let reg = Registry::new();
    let report = run_agent_turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        TurnContext::default(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            max_steps: 2,
            ..Default::default()
        },
        &sid,
        "m",
        "loop",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(
        report.text.contains("limite de 2 passos"),
        "{}",
        report.text
    );
}

fn anthropic_sse(events: &[Value]) -> String {
    let mut s = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for e in events {
        s.push_str(&format!(
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap()
        ));
    }
    s
}

#[test]
fn anthropic_streams_tool_use_and_gets_tool_results_back() {
    let dir = workspace();
    std::fs::write(dir.path().join("a.txt"), "dados").unwrap();
    let fake = serve(vec![
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":7}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"vou ler"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu1","name":"read_file","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
            json!({"type":"message_stop"}),
        ]),
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":20}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"diz dados"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ]),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let mut events = Vec::new();
    let report = turn(
        &store,
        &client(&fake.url, Protocol::Anthropic),
        &policy,
        &sid,
        |e| events.push(e),
    )
    .unwrap();
    assert_eq!(report.text, "diz dados");
    assert_eq!(report.prompt_tokens, 27);
    assert_eq!(
        report.last_prompt_tokens, 20,
        "the last step alone: the window fill"
    );
    assert!(events.contains(&RunEvent::Reasoning("vou ler".into())));
    assert!(events.contains(&RunEvent::ToolCall {
        id: "tu1".into(),
        name: "read_file".into(),
        args: r#"{"path":"a.txt"}"#.into(),
    }));

    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[0].0, "/v1/messages");
    assert!(seen[0].1["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Zerith"));
    assert_eq!(
        seen[0].1["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(seen[0].1["tools"][0]["input_schema"].is_object());
    let msgs = seen[1].1["messages"].as_array().unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["type"], "tool_result");
    assert_eq!(last["content"][0]["tool_use_id"], "tu1");
    assert!(last["content"][0]["content"]
        .as_str()
        .unwrap()
        .contains("dados"));
}

#[cfg(not(windows))]
const HOOK_EXIT2: &str = "echo proibido pelo hook 1>&2; exit 2";
#[cfg(windows)]
const HOOK_EXIT2: &str = "echo proibido pelo hook 1>&2 & exit /b 2";

#[cfg(not(windows))]
const HOOK_DENY: &str = r#"echo '{"decision":"deny","reason":"recusado json"}'"#;
#[cfg(windows)]
const HOOK_DENY: &str = r#"echo {"decision":"deny","reason":"recusado json"}"#;

#[cfg(not(windows))]
const HOOK_CONTEXT: &str = r#"echo '{"additional_context":"lembrete do hook"}'"#;
#[cfg(windows)]
const HOOK_CONTEXT: &str = r#"echo {"additional_context":"lembrete do hook"}"#;

#[cfg(not(windows))]
fn stop_once_cmd(marker: &std::path::Path, reason: &str) -> String {
    let p = marker.display().to_string().replace('\'', "'\\''");
    format!(
        "if [ ! -f '{p}' ]; then : > '{p}' && printf '%s\\n' '{{\"continue\":false,\"reason\":\"{reason}\"}}'; fi"
    )
}

#[cfg(windows)]
fn stop_once_cmd(marker: &std::path::Path, reason: &str) -> String {
    format!(
        "if not exist \"{p}\" (break > \"{p}\" & echo {{\"continue\":false,\"reason\":\"{reason}\"}})",
        p = marker.display()
    )
}

#[test]
fn user_prompt_submit_hook_blocks_without_persisting_prompt() {
    for (cmd, expected) in [(HOOK_EXIT2, "proibido"), (HOOK_DENY, "recusado json")] {
        let fake = serve(vec![]);
        let store = Store::open_in_memory().unwrap();
        let sid = session(&store);
        let mut settings = zerith_core::Settings::default();
        settings
            .hooks
            .user_prompt_submit
            .push(zerith_core::settings::HookSpec {
                matcher: None,
                command: cmd.into(),
                timeout_ms: 5_000,
            });
        let err = run_agent_turn(
            &store,
            &client(&fake.url, Protocol::OpenAi),
            TurnContext {
                settings: Some(&settings),
                ..TurnContext::default()
            },
            AgentOptions::default(),
            &sid,
            "m",
            "oi",
            &Cancel::default(),
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, RunError::Blocked(ref r) if r.contains(expected)),
            "unexpected error: {err:?}"
        );
        assert!(rows(&store, &sid).is_empty(), "prompt should not persist");
        assert!(fake.seen.lock().unwrap().is_empty());
    }
}

#[test]
fn user_prompt_submit_hook_injects_additional_context() {
    let fake = serve(vec![text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let mut settings = zerith_core::Settings::default();
    settings
        .hooks
        .user_prompt_submit
        .push(zerith_core::settings::HookSpec {
            matcher: None,
            command: HOOK_CONTEXT.into(),
            timeout_ms: 5_000,
        });
    let report = run_agent_turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        TurnContext {
            settings: Some(&settings),
            ..TurnContext::default()
        },
        AgentOptions::default(),
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.text, "ok");

    let seen = fake.seen.lock().unwrap();
    let msgs = seen[0].1["messages"].as_array().unwrap();
    let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(contents.contains(&"oi"), "{contents:?}");
    assert!(contents.contains(&"lembrete do hook"), "{contents:?}");

    let stored = store.messages(&sid, 100, 0).unwrap();
    assert!(stored.iter().any(|m| {
        m.role == Role::User
            && m.content == "lembrete do hook"
            && m.raw == Some(json!({"hook_context": true}))
    }));
}

#[test]
fn stop_hook_continue_false_runs_another_step_with_reason() {
    let dir = workspace();
    let marker = dir.path().join("stop.once");
    let fake = serve(vec![text("rascunho"), text("finalizado")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let mut settings = zerith_core::Settings::default();
    settings.hooks.stop.push(zerith_core::settings::HookSpec {
        matcher: None,
        command: stop_once_cmd(&marker, "rode os testes"),
        timeout_ms: 5_000,
    });
    let mut events = Vec::new();
    let report = run_agent_turn(
        &store,
        &client(&fake.url, Protocol::OpenAi),
        TurnContext {
            settings: Some(&settings),
            ..TurnContext::default()
        },
        AgentOptions::default(),
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |e| events.push(e),
    )
    .unwrap();
    assert_eq!(report.text, "finalizado");
    assert!(events.contains(&RunEvent::Step { n: 2 }));

    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let second_msgs = seen[1].1["messages"].as_array().unwrap();
    let last = second_msgs.last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"], "rode os testes");

    let stored = store.messages(&sid, 100, 0).unwrap();
    assert!(stored.iter().any(|m| {
        m.role == Role::User
            && m.content == "rode os testes"
            && m.raw == Some(json!({"hook_stop": true}))
    }));
}
