//! Event log, middleware pipeline, parallel tools, spill, compaction and
//! steering, against a canned HTTP server on 127.0.0.1. No network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_providers::{Cancel, Client, Protocol, ProviderConfig, ProviderKind, RetryPolicy};
use zerith_runtime::compact::{compact_now, SUMMARY_HEADER};
use zerith_runtime::{
    approval_channel, cancel_with, inbox_channel, run_agent_turn, AgentOptions, Approval,
    CancelCause, CauseSlot, Flow, Middleware, RunError, RunEvent, Summarizer, ToolInvocation,
    ToolOutcome, TurnContext,
};
use zerith_security::{preset, Action, Mode, Policy};
use zerith_sessions::{NewMessage, Role, SessionSource, Store};
use zerith_tools::{CallCtx, Exec, Permission, Registry, Tool, ToolSpec};

// ---- fake server -----------------------------------------------------------

struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Value>>>,
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
                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
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

fn http(code: u16, ctype: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn client(url: &str, context_limit: Option<u32>) -> Client {
    let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("k");
    cfg.base_url = url.into();
    cfg.protocol = Protocol::OpenAi;
    cfg.timeout_ms = 5_000;
    cfg.context_limit = context_limit;
    Client::new(cfg).unwrap().with_retry(RetryPolicy {
        max_retries: 0,
        base_ms: 1,
        max_ms: 5,
        max_retry_after_ms: 1_000,
    })
}

/// A turn context that does not read `AGENTS.md` from the repository these tests
/// run in: its 50 KB would fill the context window the pressure tests size on
/// purpose, and a test must not change with the contents of the checkout.
fn quiet() -> TurnContext<'static> {
    TurnContext {
        no_project_instructions: true,
        ..TurnContext::default()
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

fn policy(dir: &Path) -> Policy {
    Policy::new(preset("workspace-write").unwrap(), Mode::Default, dir)
}

fn session(store: &Store) -> String {
    store.create_session("t", SessionSource::Native).unwrap().id
}

fn kinds(store: &Store, sid: &str) -> Vec<String> {
    store
        .events(sid, 0, 1000)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

/// One turn with the given options; returns the result and every event.
fn run(
    store: &Store,
    fake: &Fake,
    sid: &str,
    opts: AgentOptions<'_>,
    cancel: &Cancel,
    limit: Option<u32>,
) -> (
    zerith_runtime::Result<zerith_runtime::TurnReport>,
    Vec<RunEvent>,
) {
    let mut events = Vec::new();
    let r = run_agent_turn(
        store,
        &client(&fake.url, limit),
        quiet(),
        opts,
        sid,
        "m",
        "faça",
        cancel,
        |e| events.push(e),
    );
    (r, events)
}

fn tool_rows(store: &Store, sid: &str) -> Vec<(String, String)> {
    store
        .messages(sid, 1000, 0)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| {
            (
                m.tool_calls.unwrap()["tool_call_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                m.content,
            )
        })
        .collect()
}

// ---- a fake tool with a clock ----------------------------------------------

struct Slow {
    spec: ToolSpec,
    safe: bool,
    log: Arc<Mutex<Vec<String>>>,
}

fn slow(name: &str, safe: bool, log: &Arc<Mutex<Vec<String>>>) -> Box<dyn Tool> {
    Box::new(Slow {
        spec: ToolSpec {
            name: name.into(),
            description: "test".into(),
            parameters: json!({"type": "object"}),
            returns: "tag",
            permission: Permission::Read,
            timeout: Duration::from_secs(30),
            cancellable: true,
            local: true,
            source: "test".into(),
        },
        safe,
        log: log.clone(),
    })
}

impl Tool for Slow {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, _: &Value, _: &CallCtx<'_>) -> zerith_tools::Result<Option<Action>> {
        Ok(None)
    }
    fn run(&self, args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> zerith_tools::Result<String> {
        let tag = args["tag"].as_str().unwrap_or("?").to_string();
        let until = Instant::now() + Duration::from_millis(args["ms"].as_u64().unwrap_or(0));
        self.log.lock().unwrap().push(format!("start:{tag}"));
        while Instant::now() < until {
            exec.check()?;
            std::thread::sleep(Duration::from_millis(5));
        }
        self.log.lock().unwrap().push(format!("end:{tag}"));
        Ok(format!("done {tag}"))
    }
    fn concurrency_safe(&self, _: &Value) -> bool {
        self.safe
    }
}

fn pos(log: &[String], what: &str) -> usize {
    log.iter().position(|l| l == what).unwrap_or(usize::MAX)
}

// ---- event log -------------------------------------------------------------

#[test]
fn a_tool_turn_leaves_a_complete_event_trail_and_no_deltas() {
    let dir = tmp();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        text("fim"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let reg = Registry::new();
    let pol = policy(dir.path());
    let (r, _) = run(
        &store,
        &fake,
        &sid,
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            ..Default::default()
        },
        &Cancel::default(),
        None,
    );
    r.unwrap();
    assert_eq!(
        kinds(&store, &sid),
        [
            "turn/start",
            "step/start",
            "step/end",
            "tool/call",
            "tool/result",
            "step/start",
            "step/end",
            "turn/end"
        ]
    );
    let events = store.events(&sid, 0, 100).unwrap();
    assert_eq!(events.last().unwrap().payload["reason"], "done");
    let turn = events[0].turn_id.clone().unwrap();
    assert!(events.iter().all(|e| e.turn_id.as_deref() == Some(&turn)));
    let summary = store.turn_summary(&sid).unwrap();
    assert_eq!(summary.len(), 1);
    assert!(!summary[0].is_open());
}

#[test]
fn the_step_limit_is_recorded_as_max_steps() {
    let dir = tmp();
    std::fs::write(dir.path().join("a.txt"), "A").unwrap();
    let c = || calls(&[("c", "read_file", json!({"path": "a.txt"}))]);
    let fake = serve(vec![c(), c()]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (reg, pol) = (Registry::new(), policy(dir.path()));
    let (r, _) = run(
        &store,
        &fake,
        &sid,
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            max_steps: 2,
            ..Default::default()
        },
        &Cancel::default(),
        None,
    );
    r.unwrap();
    let t = &store.turn_summary(&sid).unwrap()[0];
    assert_eq!(t.ended.as_ref().unwrap().reason, "max_steps");
    let msgs = store.messages(&sid, 100, 0).unwrap();
    assert!(
        msgs.iter()
            .any(|m| m.content.contains("Resta 1 passo de 2")),
        "o modelo precisa ser avisado antes do último passo"
    );
}

#[test]
fn a_failed_stream_attempt_is_logged_but_never_enters_the_history() {
    let fake = serve(vec![
        http(400, "text/plain", "no streaming here"),
        http(
            200,
            "application/json",
            &json!({"choices":[{"message":{"role":"assistant","content":"resposta"}}]}).to_string(),
        ),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (r, _) = run(
        &store,
        &fake,
        &sid,
        AgentOptions::default(),
        &Cancel::default(),
        None,
    );
    assert_eq!(r.unwrap().text, "resposta");
    let events = store.events(&sid, 0, 100).unwrap();
    let attempt = events
        .iter()
        .find(|e| e.kind == "assistant/attempt")
        .expect("the failed stream is in the log");
    assert_eq!(attempt.payload["attempt"]["mode"], "stream");
    // History: the prompt and the answer, nothing from the failed attempt.
    let rows: Vec<Role> = store
        .messages(&sid, 10, 0)
        .unwrap()
        .iter()
        .map(|m| m.role)
        .collect();
    assert_eq!(rows, [Role::User, Role::Assistant]);
}

#[test]
fn cancel_cause_defaults_to_user_and_can_be_a_hook() {
    // Default: whoever pulled `Cancel` directly is the user.
    let fake = serve(vec![sse(&[
        json!({"choices":[{"delta":{"content":"um"}}]}),
        json!({"choices":[{"delta":{"content":"dois"}}]}),
    ])]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let cancel = Cancel::default();
    let mut events = Vec::new();
    let err = run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions::default(),
        &sid,
        "m",
        "oi",
        &cancel,
        |e| {
            if matches!(e, RunEvent::Delta(_)) {
                cancel.cancel();
            }
            events.push(e);
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        RunError::Provider(zerith_providers::ProviderError::Cancelled)
    ));
    let end = store.turn_summary(&sid).unwrap()[0].ended.clone().unwrap();
    assert_eq!(
        (end.reason.as_str(), end.cause.as_deref()),
        ("cancelled", Some("user"))
    );

    // With a slot: the cause the canceller gave.
    let fake = serve(vec![sse(&[
        json!({"choices":[{"delta":{"content":"um"}}]}),
        json!({"choices":[{"delta":{"content":"dois"}}]}),
    ])]);
    let sid = session(&store);
    let (cancel, slot) = (Cancel::default(), CauseSlot::default());
    let _ = run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            cancel_cause: Some(&slot),
            ..Default::default()
        },
        &sid,
        "m",
        "oi",
        &cancel,
        |e| {
            if matches!(e, RunEvent::Delta(_)) {
                cancel_with(&cancel, &slot, CancelCause::Hook);
            }
        },
    );
    assert_eq!(slot.get(), Some(CancelCause::Hook));
    let end = store.turn_summary(&sid).unwrap()[0].ended.clone().unwrap();
    assert_eq!(
        (end.reason.as_str(), end.cause.as_deref()),
        ("cancelled", Some("hook"))
    );
}

// ---- middleware ------------------------------------------------------------

#[derive(Default)]
struct Fake1 {
    deny: Option<String>,
    ask: Option<String>,
    replace: Option<String>,
    rewrite_path: Option<String>,
    context: Option<String>,
    suffix: Option<String>,
    seen: Mutex<Vec<String>>,
}

impl Middleware for Fake1 {
    fn before(&self, call: &mut ToolInvocation) -> Flow {
        self.seen
            .lock()
            .unwrap()
            .push(format!("before:{}", call.name));
        if let Some(p) = &self.rewrite_path {
            call.args["path"] = json!(p);
        }
        if let Some(w) = &self.deny {
            return Flow::Deny(w.clone());
        }
        if let Some(r) = &self.replace {
            return Flow::Replace(r.clone());
        }
        match &self.ask {
            Some(s) => Flow::Ask(s.clone()),
            None => Flow::Continue,
        }
    }
    fn after(&self, call: &ToolInvocation, result: &mut ToolOutcome) {
        self.seen
            .lock()
            .unwrap()
            .push(format!("after:{}", call.name));
        if let Some(c) = &self.context {
            result.context.push(c.clone());
        }
        if let Some(s) = &self.suffix {
            result.output.push_str(s);
        }
    }
}

/// Runs one `read_file a.txt` turn with `mw` in the chain.
fn mw_turn(
    mw: Fake1,
    gate: Option<&zerith_runtime::ApprovalGate>,
    on: impl FnMut(RunEvent),
) -> (Store, String, Fake, Arc<Fake1>) {
    let dir = tmp();
    std::fs::write(dir.path().join("a.txt"), "CONTEUDO-REAL").unwrap();
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        text("ok"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (reg, pol) = (Registry::new(), policy(dir.path()));
    let mw = Arc::new(mw);
    struct Shared(Arc<Fake1>);
    impl Middleware for Shared {
        fn before(&self, c: &mut ToolInvocation) -> Flow {
            self.0.before(c)
        }
        fn after(&self, c: &ToolInvocation, r: &mut ToolOutcome) {
            self.0.after(c, r)
        }
    }
    let mut on = on;
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            approvals: gate,
            middleware: vec![Box::new(Shared(mw.clone()))],
            ..Default::default()
        },
        &sid,
        "m",
        "faça",
        &Cancel::default(),
        &mut on,
    )
    .unwrap();
    (store, sid, fake, mw)
}

#[test]
fn middleware_can_deny_and_the_tool_never_runs() {
    let (store, sid, _, mw) = mw_turn(
        Fake1 {
            deny: Some("hook proibiu".into()),
            ..Default::default()
        },
        None,
        |_| {},
    );
    let rows = tool_rows(&store, &sid);
    assert_eq!(rows[0].1, "negado: hook proibiu");
    assert!(!rows[0].1.contains("CONTEUDO-REAL"));
    // `after` still sees the refusal, so a hook can log it.
    assert_eq!(
        *mw.seen.lock().unwrap(),
        ["before:read_file", "after:read_file"]
    );
}

#[test]
fn middleware_can_force_an_approval_and_the_answer_decides() {
    for (answer, runs) in [(Approval::AllowOnce, true), (Approval::Deny, false)] {
        let (tx, gate) = approval_channel();
        let mut summary = None;
        let (store, sid, _, _) = mw_turn(
            Fake1 {
                ask: Some("o hook quer confirmar".into()),
                ..Default::default()
            },
            Some(&gate),
            |e| {
                if let RunEvent::ApprovalNeeded { id, summary: s, .. } = e {
                    summary = Some(s);
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(30));
                        tx.answer(id, answer);
                    });
                }
            },
        );
        assert_eq!(summary.as_deref(), Some("o hook quer confirmar"));
        let out = &tool_rows(&store, &sid)[0].1;
        assert_eq!(out.contains("CONTEUDO-REAL"), runs, "{out}");
        if !runs {
            assert_eq!(out, "negado pelo usuário");
        }
    }
}

#[test]
fn a_middleware_ask_without_a_gate_fails_closed() {
    let (store, sid, _, _) = mw_turn(
        Fake1 {
            ask: Some("confirme".into()),
            ..Default::default()
        },
        None,
        |_| {},
    );
    let out = &tool_rows(&store, &sid)[0].1;
    assert!(
        out.starts_with("negado:") && !out.contains("CONTEUDO-REAL"),
        "{out}"
    );
}

#[test]
fn middleware_can_rewrite_arguments_and_the_policy_sees_the_rewrite() {
    let dir = tmp();
    std::fs::write(dir.path().join("b.txt"), "DE-B").unwrap();
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        text("ok"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (reg, pol) = (Registry::new(), policy(dir.path()));
    struct Redirect(String);
    impl Middleware for Redirect {
        fn before(&self, call: &mut ToolInvocation) -> Flow {
            call.args["path"] = json!(self.0);
            Flow::Continue
        }
    }
    let run_with = |target: &str, sid: &str| {
        let fake = serve(vec![
            calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
            text("ok"),
        ]);
        run_agent_turn(
            &store,
            &client(&fake.url, None),
            quiet(),
            AgentOptions {
                tools: Some(&reg),
                policy: Some(&pol),
                middleware: vec![Box::new(Redirect(target.into()))],
                ..Default::default()
            },
            sid,
            "m",
            "faça",
            &Cancel::default(),
            |_| {},
        )
        .unwrap();
    };
    let _ = fake;
    run_with("b.txt", &sid);
    assert!(tool_rows(&store, &sid)[0].1.contains("DE-B"));

    // Rewritten to a path outside the workspace: the policy asks, with no
    // gate that is a refusal. Middleware cannot smuggle a call past it.
    let sid2 = session(&store);
    let outside = std::env::temp_dir().join("fora.txt");
    run_with(outside.to_str().unwrap(), &sid2);
    let out = &tool_rows(&store, &sid2)[0].1;
    assert!(out.starts_with("negado:"), "{out}");
}

#[test]
fn middleware_replace_skips_the_tool_and_after_can_add_context() {
    let (store, sid, fake, _) = mw_turn(
        Fake1 {
            replace: Some("EM-CACHE".into()),
            context: Some("AVISO-DO-HOOK".into()),
            suffix: Some(" [+hook]".into()),
            ..Default::default()
        },
        None,
        |_| {},
    );
    assert_eq!(tool_rows(&store, &sid)[0].1, "EM-CACHE [+hook]");
    // The extra context is a user message after the tool result, in the
    // second request and in the store.
    let seen = fake.seen.lock().unwrap();
    let msgs = seen[1]["messages"].as_array().unwrap();
    let n = msgs.len();
    assert_eq!(msgs[n - 2]["role"], "tool");
    assert_eq!(msgs[n - 1]["role"], "user");
    assert_eq!(msgs[n - 1]["content"], "AVISO-DO-HOOK");
    let stored = store.messages(&sid, 100, 0).unwrap();
    assert!(stored.iter().any(|m| m.content == "AVISO-DO-HOOK"));
    assert!(kinds(&store, &sid).contains(&"tool/context".to_string()));
}

// ---- parallel tools --------------------------------------------------------

fn parallel_turn(
    tools: &[(&str, bool)],
    list: &[(&str, &str, Value)],
    max_parallel: usize,
    cancel: &Cancel,
    slot: Option<&CauseSlot>,
) -> (
    Store,
    String,
    Arc<Mutex<Vec<String>>>,
    zerith_runtime::Result<zerith_runtime::TurnReport>,
) {
    let dir = tmp();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut reg = Registry::empty();
    for (n, safe) in tools {
        reg.register(slow(n, *safe, &log));
    }
    let fake = serve(vec![calls(list), text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let pol = policy(dir.path());
    let r = run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            max_parallel_tools: max_parallel,
            cancel_cause: slot,
            ..Default::default()
        },
        &sid,
        "m",
        "faça",
        cancel,
        |_| {},
    );
    (store, sid, log, r)
}

#[test]
fn safe_calls_overlap_and_results_keep_the_model_order() {
    let list = [
        ("c1", "peek", json!({"tag": "a", "ms": 400})),
        ("c2", "peek", json!({"tag": "b", "ms": 150})),
        ("c3", "peek", json!({"tag": "c", "ms": 10})),
    ];
    let t = Instant::now();
    let (store, sid, log, r) = parallel_turn(&[("peek", true)], &list, 4, &Cancel::default(), None);
    let parallel = t.elapsed();
    r.unwrap();
    let log = log.lock().unwrap().clone();
    // The fast call finished first, but the rows are in call order.
    assert!(pos(&log, "end:c") < pos(&log, "end:a"), "{log:?}");
    let ids: Vec<String> = tool_rows(&store, &sid).into_iter().map(|r| r.0).collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
    assert!(parallel < Duration::from_millis(1000), "{parallel:?}");

    // max_parallel_tools = 1 is serial, in order.
    let (_, _, log, r) = parallel_turn(&[("peek", true)], &list, 1, &Cancel::default(), None);
    r.unwrap();
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        ["start:a", "end:a", "start:b", "end:b", "start:c", "end:c"]
    );
}

#[test]
fn an_unsafe_call_is_a_barrier() {
    let list = [
        ("c1", "peek", json!({"tag": "a", "ms": 150})),
        ("c2", "peek", json!({"tag": "b", "ms": 150})),
        ("c3", "write", json!({"tag": "w", "ms": 50})),
        ("c4", "peek", json!({"tag": "d", "ms": 20})),
    ];
    let (store, sid, log, r) = parallel_turn(
        &[("peek", true), ("write", false)],
        &list,
        4,
        &Cancel::default(),
        None,
    );
    r.unwrap();
    let log = log.lock().unwrap().clone();
    // a and b ran together ...
    assert!(pos(&log, "start:b") < pos(&log, "end:a"), "{log:?}");
    // ... both finished before the unsafe call began ...
    assert!(pos(&log, "end:a") < pos(&log, "start:w"), "{log:?}");
    assert!(pos(&log, "end:b") < pos(&log, "start:w"), "{log:?}");
    // ... and it finished before the next safe call began.
    assert!(pos(&log, "end:w") < pos(&log, "start:d"), "{log:?}");
    let ids: Vec<String> = tool_rows(&store, &sid).into_iter().map(|r| r.0).collect();
    assert_eq!(ids, ["c1", "c2", "c3", "c4"]);
}

#[test]
fn cancelling_mid_batch_stops_every_call_and_keeps_the_history_paired() {
    let list = [
        ("c1", "peek", json!({"tag": "a", "ms": 20000})),
        ("c2", "peek", json!({"tag": "b", "ms": 20000})),
        ("c3", "peek", json!({"tag": "c", "ms": 20000})),
    ];
    let (cancel, slot) = (Cancel::default(), CauseSlot::default());
    let (c2, s2) = (cancel.clone(), slot.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        cancel_with(&c2, &s2, CancelCause::Parent);
    });
    let t = Instant::now();
    let (store, sid, _, r) = parallel_turn(&[("peek", true)], &list, 4, &cancel, Some(&slot));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(matches!(
        r.unwrap_err(),
        RunError::Provider(zerith_providers::ProviderError::Cancelled)
    ));
    // Every call has its result row: nothing left dangling for the next turn.
    let ids: Vec<String> = tool_rows(&store, &sid).into_iter().map(|r| r.0).collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
    let end = store.turn_summary(&sid).unwrap()[0].ended.clone().unwrap();
    assert_eq!(
        (end.reason.as_str(), end.cause.as_deref()),
        ("cancelled", Some("parent"))
    );
}

// ---- spill -----------------------------------------------------------------

fn big_file(dir: &Path) {
    let body: String = (1..=2000)
        .map(|i| format!("linha numero {i:05}\n"))
        .collect();
    assert!(body.len() > 30_000);
    std::fs::write(dir.join("big.txt"), body).unwrap();
}

#[test]
fn a_big_output_spills_to_a_file_the_model_can_read_back_without_asking() {
    let ws = tmp();
    let spill_root = tmp();
    big_file(ws.path());
    let reg = Registry::new();
    let pol = policy(ws.path());
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);

    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "big.txt"}))]),
        text("li"),
    ]);
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            spill_dir: Some(spill_root.path().to_path_buf()),
            ..Default::default()
        },
        &sid,
        "m",
        "leia",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();

    let shown = &tool_rows(&store, &sid)[0].1;
    assert!(
        shown.contains("[saída completa:"),
        "{}",
        &shown[shown.len() - 200..]
    );
    assert!(shown.contains("2000 linhas") || shown.contains("linhas)"));
    assert!(shown.contains("read_file com offset/limit"));
    assert!(shown.len() < 10_000, "only the head reaches the model");
    let ev = store.events(&sid, 0, 100).unwrap();
    let res = ev.iter().find(|e| e.kind == "tool/result").unwrap();
    let spill_file = PathBuf::from(res.payload["spill"].as_str().unwrap());
    assert_eq!(spill_file.parent().unwrap(), spill_root.path().join(&sid));
    assert!(spill_file
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("c1-"));
    let full = std::fs::read_to_string(&spill_file).unwrap();
    assert!(
        full.contains("linha numero 02000"),
        "the spill holds everything"
    );

    // The model reads the rest from the spill file. It lives outside the
    // workspace, so without the exemption this would be refused (no gate).
    let fake = serve(vec![
        calls(&[(
            "c2",
            "read_file",
            json!({"path": spill_file.to_str().unwrap(), "offset": 1995, "limit": 3}),
        )]),
        text("li o resto"),
    ]);
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            spill_dir: Some(spill_root.path().to_path_buf()),
            ..Default::default()
        },
        &sid,
        "m",
        "continue",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let rows = tool_rows(&store, &sid);
    let last = &rows.last().unwrap().1;
    assert!(last.contains("linha numero 01995"), "{last}");

    // The exemption is for the spill directory only.
    let other = spill_root.path().join("..").join("nao-e-spill.txt");
    std::fs::write(&other, "x").unwrap();
    let fake = serve(vec![
        calls(&[("c3", "read_file", json!({"path": other.to_str().unwrap()}))]),
        text("ok"),
    ]);
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            spill_dir: Some(spill_root.path().to_path_buf()),
            ..Default::default()
        },
        &sid,
        "m",
        "outro",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    let rows = tool_rows(&store, &sid);
    assert!(rows.last().unwrap().1.starts_with("negado:"), "{rows:?}");
    let _ = std::fs::remove_file(other);
}

#[test]
fn without_a_spill_dir_the_output_is_truncated_as_before() {
    let ws = tmp();
    big_file(ws.path());
    let (reg, pol) = (Registry::new(), policy(ws.path()));
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "big.txt"}))]),
        text("li"),
    ]);
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            ..Default::default()
        },
        &sid,
        "m",
        "leia",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(tool_rows(&store, &sid)[0].1.contains("[saída truncada"));
}

#[test]
fn old_spill_files_are_deleted_when_a_turn_opens() {
    let root = tmp();
    let old_dir = root.path().join("sessao-velha");
    std::fs::create_dir_all(&old_dir).unwrap();
    let old = old_dir.join("x.txt");
    std::fs::write(&old, "velho").unwrap();
    let eight_days = Duration::from_secs(8 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - eight_days)
        .unwrap();
    let fresh_dir = root.path().join("sessao-nova");
    std::fs::create_dir_all(&fresh_dir).unwrap();
    std::fs::write(fresh_dir.join("y.txt"), "novo").unwrap();

    let fake = serve(vec![text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            spill_dir: Some(root.path().to_path_buf()),
            ..Default::default()
        },
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(!old.exists());
    assert!(!old_dir.exists(), "the emptied directory goes too");
    assert!(fresh_dir.join("y.txt").exists());
}

// ---- compaction ------------------------------------------------------------

struct FakeSummary {
    fail: bool,
    seen: Mutex<Vec<String>>,
}

impl Summarizer for FakeSummary {
    fn summarize(&self, transcript: &str, _: &Cancel) -> Result<String, String> {
        self.seen.lock().unwrap().push(transcript.to_string());
        if self.fail {
            Err("modelo indisponível".into())
        } else {
            Ok("RESUMO-FAKE: o usuário falou de girafas".into())
        }
    }
}

/// A long history of 14 messages of ~1200 chars (about 4000 tokens).
fn long_history(store: &Store, sid: &str) {
    for i in 0..14 {
        let (role, body) = if i % 2 == 0 {
            (Role::User, format!("pergunta {i} {}", "g".repeat(1200)))
        } else {
            (
                Role::Assistant,
                format!("resposta {i} {}", "h".repeat(1200)),
            )
        };
        store.append(sid, &NewMessage::new(role, body)).unwrap();
    }
}

#[test]
fn pressure_summarizes_the_oldest_span_and_replay_shows_the_summary() {
    let fake = serve(vec![text("um"), text("dois")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    long_history(&store, &sid);
    let summarizer = FakeSummary {
        fail: false,
        seen: Mutex::new(Vec::new()),
    };
    let mut compacted = false;
    run_agent_turn(
        &store,
        &client(&fake.url, Some(2500)),
        quiet(),
        AgentOptions {
            summarizer: Some(&summarizer),
            history: 100,
            ..Default::default()
        },
        &sid,
        "m",
        "PERGUNTA FINAL",
        &Cancel::default(),
        |e| compacted |= matches!(e, RunEvent::Compacted { .. }),
    )
    .unwrap();
    assert!(compacted);
    assert_eq!(summarizer.seen.lock().unwrap().len(), 1);
    assert!(summarizer.seen.lock().unwrap()[0].contains("pergunta 0"));

    // The model saw the summary, then the recent tail, ending at the prompt.
    let sent = fake.seen.lock().unwrap();
    let msgs = sent[0]["messages"].as_array().unwrap();
    assert!(msgs[1]["content"]
        .as_str()
        .unwrap()
        .starts_with(SUMMARY_HEADER));
    assert!(msgs[1]["content"].as_str().unwrap().contains("RESUMO-FAKE"));
    assert_eq!(msgs.last().unwrap()["content"], "PERGUNTA FINAL");
    assert!(msgs
        .iter()
        .all(|m| !m["content"].as_str().unwrap_or("").contains("pergunta 0 ")));
    drop(sent);

    // Events: the bracket, in order, with the summary inside it.
    let k = kinds(&store, &sid);
    let (s, m, e) = (
        k.iter().position(|x| x == "compaction/start").unwrap(),
        k.iter().position(|x| x == "compaction/summary").unwrap(),
        k.iter().position(|x| x == "compaction/end").unwrap(),
    );
    assert!(s < m && m < e, "{k:?}");
    assert!(!store.has_open_compaction(&sid).unwrap());

    // Nothing was deleted: the summary is one more row, marked in `raw`.
    let rows = store.messages(&sid, 1000, 0).unwrap();
    assert!(rows.iter().any(|r| r.content.contains("pergunta 0 ")));
    let summary_row = rows
        .iter()
        .find(|r| r.raw.as_ref().is_some_and(|v| v.get("summary").is_some()))
        .unwrap();
    assert_eq!(summary_row.role, Role::User);
    assert!(
        summary_row.raw.as_ref().unwrap()["summary"]["start_id"]
            .as_i64()
            .unwrap()
            >= 1
    );

    // The next turn replays the summary instead of the hidden range.
    let sid_rows_before = rows.len();
    let fake2 = serve(vec![text("tres")]);
    run_agent_turn(
        &store,
        &client(&fake2.url, None),
        quiet(),
        AgentOptions {
            history: 100,
            ..Default::default()
        },
        &sid,
        "m",
        "OUTRA",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert!(store.messages(&sid, 1000, 0).unwrap().len() > sid_rows_before);
    let sent = fake2.seen.lock().unwrap();
    let msgs = sent[0]["messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| m["content"]
        .as_str()
        .unwrap_or("")
        .starts_with(SUMMARY_HEADER)));
    assert!(msgs
        .iter()
        .all(|m| !m["content"].as_str().unwrap_or("").contains("pergunta 0 ")));
}

#[test]
fn a_failing_summarizer_never_fails_the_turn() {
    let fake = serve(vec![text("ainda respondo")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    long_history(&store, &sid);
    let summarizer = FakeSummary {
        fail: true,
        seen: Mutex::new(Vec::new()),
    };
    let (r, _) = run(
        &store,
        &fake,
        &sid,
        AgentOptions {
            summarizer: Some(&summarizer),
            history: 100,
            ..Default::default()
        },
        &Cancel::default(),
        Some(2500),
    );
    assert_eq!(r.unwrap().text, "ainda respondo");
    let events = store.events(&sid, 0, 100).unwrap();
    let end = events.iter().find(|e| e.kind == "compaction/end").unwrap();
    assert_eq!(end.payload["ok"], false);
    assert!(
        events.iter().any(|e| e.kind == "compaction/fallback"),
        "dropped instead"
    );
    assert!(!store.has_open_compaction(&sid).unwrap());
    // The request still carried the prompt and fit.
    let sent = fake.seen.lock().unwrap();
    let msgs = sent[0]["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["content"], "faça");
    assert!(msgs.len() < 16);
}

#[test]
fn a_crash_inside_compaction_leaves_an_open_bracket_and_an_open_turn() {
    let dir = tmp();
    let db = dir.path().join("z.db");
    let sid = {
        let store = Store::open(&db).unwrap();
        let sid = session(&store);
        long_history(&store, &sid);
        let sid2 = sid.clone();
        let db2 = db.clone();
        drop(store);
        // The summarizer panics: the thread dies mid-compaction, as a crash would.
        let died = std::thread::spawn(move || {
            struct Boom;
            impl Summarizer for Boom {
                fn summarize(&self, _: &str, _: &Cancel) -> Result<String, String> {
                    panic!("processo morreu");
                }
            }
            let store = Store::open(&db2).unwrap();
            let fake = serve(vec![text("nunca")]);
            let _ = run_agent_turn(
                &store,
                &client(&fake.url, Some(2500)),
                quiet(),
                AgentOptions {
                    summarizer: Some(&Boom),
                    history: 100,
                    ..Default::default()
                },
                &sid2,
                "m",
                "oi",
                &Cancel::default(),
                |_| {},
            );
        })
        .join();
        assert!(died.is_err());
        sid
    };
    let store = Store::open(&db).unwrap();
    assert!(
        store.has_open_compaction(&sid).unwrap(),
        "start without end"
    );
    let turns = store.turn_summary(&sid).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].is_open(), "turn/start without turn/end");
}

#[test]
fn compact_now_summarizes_everything_before_the_last_message() {
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    long_history(&store, &sid);
    store
        .append(&sid, &NewMessage::new(Role::User, "ULTIMA"))
        .unwrap();
    let c = client("http://127.0.0.1:9/v1", None);
    let summarizer = FakeSummary {
        fail: false,
        seen: Mutex::new(Vec::new()),
    };
    let report = compact_now(&store, &c, &sid, "m", &Cancel::default(), Some(&summarizer))
        .unwrap()
        .expect("something to summarize");
    assert_eq!(report.summarized, 14);
    assert!(report.tokens_after < report.tokens_before);
    assert!(!store.has_open_compaction(&sid).unwrap());
    let k = kinds(&store, &sid);
    assert_eq!(
        k,
        ["compaction/start", "compaction/summary", "compaction/end"]
    );
    // Events outside a turn have no turn id and do not count as a turn.
    assert!(store.turn_summary(&sid).unwrap().is_empty());

    // Nothing left to compact but the summary and the last message.
    let again = compact_now(&store, &c, &sid, "m", &Cancel::default(), Some(&summarizer)).unwrap();
    assert!(again.is_none());

    // And a failing summarizer is an error here, not a silent fallback.
    let sid2 = session(&store);
    long_history(&store, &sid2);
    store
        .append(&sid2, &NewMessage::new(Role::User, "U"))
        .unwrap();
    let bad = FakeSummary {
        fail: true,
        seen: Mutex::new(Vec::new()),
    };
    assert!(compact_now(&store, &c, &sid2, "m", &Cancel::default(), Some(&bad)).is_err());
    assert!(!store.has_open_compaction(&sid2).unwrap());
}

// ---- steering --------------------------------------------------------------

#[test]
fn an_inbox_message_sent_mid_turn_joins_the_next_request() {
    let dir = tmp();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let fake = serve(vec![
        calls(&[("c1", "read_file", json!({"path": "a.txt"}))]),
        text("mudei"),
    ]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (reg, pol) = (Registry::new(), policy(dir.path()));
    let (tx, rx) = inbox_channel();
    let mut sent = false;
    run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            inbox: Some(&rx),
            ..Default::default()
        },
        &sid,
        "m",
        "faça",
        &Cancel::default(),
        |e| {
            if matches!(e, RunEvent::ToolCall { .. }) && !sent {
                sent = tx.send("MUDE DE RUMO");
            }
        },
    )
    .unwrap();
    assert!(sent);
    // First request: no steer yet. Second: tool result, then the steer.
    let seen = fake.seen.lock().unwrap();
    let first = seen[0]["messages"].as_array().unwrap();
    assert!(first.iter().all(|m| m["content"] != "MUDE DE RUMO"));
    let second = seen[1]["messages"].as_array().unwrap();
    let n = second.len();
    assert_eq!(second[n - 2]["role"], "tool");
    assert_eq!(second[n - 1]["content"], "MUDE DE RUMO");
    assert_eq!(second[n - 1]["role"], "user");
    // Persisted and logged.
    let rows = store.messages(&sid, 100, 0).unwrap();
    assert!(rows
        .iter()
        .any(|m| m.role == Role::User && m.content == "MUDE DE RUMO"));
    assert!(kinds(&store, &sid).contains(&"user/steer".to_string()));
}

#[test]
fn a_steer_sent_while_the_final_answer_streams_keeps_the_turn_going() {
    let fake = serve(vec![text("primeira"), text("segunda")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let (tx, rx) = inbox_channel();
    let mut sent = false;
    let report = run_agent_turn(
        &store,
        &client(&fake.url, None),
        quiet(),
        AgentOptions {
            inbox: Some(&rx),
            ..Default::default()
        },
        &sid,
        "m",
        "oi",
        &Cancel::default(),
        |e| {
            if matches!(e, RunEvent::Delta(_)) && !sent {
                sent = tx.send("e mais isto");
            }
        },
    )
    .unwrap();
    assert_eq!(report.text, "segunda");
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
    // The ended turn closed its inbox: a late steer is refused, not acked and lost.
    assert!(!tx.send("tarde demais"));
}

// ---- gateway failover & token saver ---------------------------------------

#[test]
fn gateway_failover_switches_accounts_records_usage_and_skips_cooled_down_account() {
    let fake1 = serve(vec![http(429, "Retry-After: 5\r\n", "rate limited")]);
    let fake2 = serve(vec![text("ok da conta 2")]);

    let mut settings = zerith_core::Settings {
        accounts: vec![
            zerith_core::AccountEntry {
                id: "acc-1".into(),
                provider: "openai".into(),
                base_url: Some(fake1.url.clone()),
                priority: 1,
                enabled: true,
                ..Default::default()
            },
            zerith_core::AccountEntry {
                id: "acc-2".into(),
                provider: "openai".into(),
                base_url: Some(fake2.url.clone()),
                priority: 2,
                enabled: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let mut cfg = ProviderConfig::new(ProviderKind::OpenAi).with_key("test-key");
    cfg.base_url = fake1.url.clone();
    cfg.timeout_ms = 5_000;
    let fb_client = Client::new(cfg).unwrap().with_retry(RetryPolicy {
        max_retries: 0,
        base_ms: 1,
        max_ms: 5,
        max_retry_after_ms: 1_000,
    });

    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let mut events = Vec::new();
    let report = run_agent_turn(
        &store,
        &fb_client,
        TurnContext {
            settings: Some(&settings),
            ..quiet()
        },
        AgentOptions::default(),
        &sid,
        "gpt-4o",
        "oi",
        &Cancel::default(),
        |e| events.push(e),
    )
    .unwrap();

    assert_eq!(report.text, "ok da conta 2");
    assert_eq!(report.account_id.as_deref(), Some("acc-2"));
    assert_eq!(report.model_used.as_deref(), Some("gpt-4o"));
    assert!(events.contains(&RunEvent::Route {
        account_id: "acc-2".into(),
        provider: "openai".into(),
        model: "gpt-4o".into(),
        attempts: 2,
    }));
    assert_eq!(fake1.seen.lock().unwrap().len(), 1);
    assert_eq!(fake2.seen.lock().unwrap().len(), 1);

    let summary = store.usage_summary(24).unwrap();
    assert_eq!(summary.active_account.as_deref(), Some("acc-2"));
    assert_eq!(summary.active_model.as_deref(), Some("gpt-4o"));
    let acc1 = summary
        .accounts
        .iter()
        .find(|a| a.account_id == "acc-1")
        .expect("acc-1 recorded");
    assert_eq!(acc1.rate_limits, 1);
    assert!(acc1.cooldown_until > 0);
    let acc2 = summary
        .accounts
        .iter()
        .find(|a| a.account_id == "acc-2")
        .expect("acc-2 recorded");
    assert_eq!(acc2.requests, 1);
    assert_eq!(acc2.prompt_tokens, 10);
    assert_eq!(acc2.completion_tokens, 2);
    assert!(!store.is_account_available("acc-1", 5, None, None).unwrap());
    assert!(store.is_account_available("acc-2", 5, None, None).unwrap());

    let log_events = store.events(&sid, 0, 100).unwrap();
    assert!(log_events.iter().any(|e| {
        e.kind == "assistant/attempt" && e.payload["attempt"]["mode"] == "gateway_failover"
    }));
    assert!(log_events
        .iter()
        .any(|e| e.kind == "step/route" && e.payload["account"] == "acc-2"));

    // Next turn skips acc-1 while it is in cooldown without hitting its endpoint.
    let fake1_skip = serve(vec![]);
    let fake2_turn2 = serve(vec![text("segundo turno")]);
    settings.accounts[0].base_url = Some(fake1_skip.url.clone());
    settings.accounts[1].base_url = Some(fake2_turn2.url.clone());
    let mut events2 = Vec::new();
    let report2 = run_agent_turn(
        &store,
        &fb_client,
        TurnContext {
            settings: Some(&settings),
            ..quiet()
        },
        AgentOptions::default(),
        &sid,
        "gpt-4o",
        "continuar",
        &Cancel::default(),
        |e| events2.push(e),
    )
    .unwrap();
    assert_eq!(report2.text, "segundo turno");
    assert!(events2.contains(&RunEvent::Route {
        account_id: "acc-2".into(),
        provider: "openai".into(),
        model: "gpt-4o".into(),
        attempts: 1,
    }));
    assert!(fake1_skip.seen.lock().unwrap().is_empty());
    assert_eq!(fake2_turn2.seen.lock().unwrap().len(), 1);
}

/// With one account there is nowhere to fail over to: a failed request must
/// not cool the account down, or every later turn is refused unsent with a
/// "rate limit" that never happened.
#[test]
fn a_single_account_is_never_cooled_down_and_shows_the_real_error() {
    let fake = serve(vec![http(503, "text/plain", "upstream busy")]);
    let settings = zerith_core::Settings {
        accounts: vec![zerith_core::AccountEntry {
            id: "solo".into(),
            provider: "openai".into(),
            base_url: Some(fake.url.clone()),
            priority: 1,
            enabled: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut cfg = ProviderConfig::new(ProviderKind::OpenAi).with_key("test-key");
    cfg.base_url = fake.url.clone();
    cfg.timeout_ms = 5_000;
    let fb = Client::new(cfg).unwrap().with_retry(RetryPolicy::none());
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    let turn = |prompt: &str| {
        run_agent_turn(
            &store,
            &fb,
            TurnContext {
                settings: Some(&settings),
                ..quiet()
            },
            AgentOptions::default(),
            &sid,
            "gpt-4o",
            prompt,
            &Cancel::default(),
            |_| {},
        )
    };
    let err = turn("oi").unwrap_err();
    assert!(
        matches!(
            err,
            RunError::Provider(zerith_providers::ProviderError::Http { status: 503, .. })
        ),
        "{err}"
    );
    assert!(store.is_account_available("solo", 5, None, None).unwrap());
    let acc = store.account_window_stats("solo", 5).unwrap();
    assert_eq!(acc.cooldown_until, 0);
    assert_eq!(acc.requests, 1, "the failed attempt is still on the record");

    // The next turn is sent, not refused.
    let fake2 = serve(vec![text("voltou")]);
    let mut settings2 = settings.clone();
    settings2.accounts[0].base_url = Some(fake2.url.clone());
    let r = run_agent_turn(
        &store,
        &fb,
        TurnContext {
            settings: Some(&settings2),
            ..quiet()
        },
        AgentOptions::default(),
        &sid,
        "gpt-4o",
        "de novo",
        &Cancel::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(r.text, "voltou");
    assert_eq!(fake2.seen.lock().unwrap().len(), 1);
}

/// No `context_limit` anywhere: the turn assumes the default window, so a
/// history past 80% of it is compacted in the turn instead of sent whole.
#[test]
fn in_turn_compaction_runs_without_a_configured_context_limit() {
    let fake = serve(vec![text("ok")]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);
    // 14 messages of ~32k chars: ~112k tokens, over 80% of the 128k default
    for i in 0..14 {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store
            .append(
                &sid,
                &NewMessage::new(role, format!("pergunta {i} {}", "g".repeat(32_000))),
            )
            .unwrap();
    }
    let summarizer = FakeSummary {
        fail: false,
        seen: Mutex::new(Vec::new()),
    };
    let (r, events) = run(
        &store,
        &fake,
        &sid,
        AgentOptions {
            summarizer: Some(&summarizer),
            history: 100,
            ..Default::default()
        },
        &Cancel::default(),
        None,
    );
    r.unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, RunEvent::Compacted { .. })));
    assert_eq!(summarizer.seen.lock().unwrap().len(), 1);
}

struct FakeBuildTool {
    spec: ToolSpec,
    output: String,
}

impl Tool for FakeBuildTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, _: &Value, _: &CallCtx<'_>) -> zerith_tools::Result<Option<Action>> {
        Ok(None)
    }
    fn run(&self, _: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> zerith_tools::Result<String> {
        Ok(self.output.clone())
    }
}

#[test]
fn token_saver_full_injects_prompts_compresses_build_output_and_records_savings() {
    let mut build_out = String::new();
    for i in 0..25 {
        build_out.push_str(&format!("   Compiling pkg-{i:02} v0.1.0\n"));
    }
    build_out.push_str(
        "error[E0308]: mismatched types\n \
         --> src/lib.rs:42:9\n  \
         |\n\
         42 |     let x: u32 = \"hi\";\n  \
         |                  ^^^^ expected `u32`, found `&str`\n\
         error: could not compile `demo` due to 1 previous error\n",
    );
    assert!(build_out.len() > 500);

    let dir = tmp();
    let mut reg = Registry::empty();
    reg.register(Box::new(FakeBuildTool {
        spec: ToolSpec {
            name: "list_dir".into(),
            description: "runs build".into(),
            parameters: json!({"type": "object"}),
            returns: "build log",
            permission: Permission::Read,
            timeout: Duration::from_secs(10),
            cancellable: true,
            local: true,
            source: "test".into(),
        },
        output: build_out,
    }));
    let pol = policy(dir.path());
    let final_reply = sse(&[
        json!({"choices":[{"delta":{"content":"```rust\nlet x: &str = \"hi\";\n```\ncorrigido"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}),
    ]);
    let fake = serve(vec![calls(&[("b1", "list_dir", json!({}))]), final_reply]);
    let store = Store::open_in_memory().unwrap();
    let sid = session(&store);

    let settings = zerith_core::Settings {
        token_saver: zerith_core::TokenSaverSettings {
            ponytail: zerith_core::PonytailLevel::Full,
            caveman: zerith_core::CavemanLevel::Full,
            rtk_enabled: true,
        },
        ..Default::default()
    };

    let mut events = Vec::new();
    let report = run_agent_turn(
        &store,
        &client(&fake.url, None),
        TurnContext {
            settings: Some(&settings),
            ..quiet()
        },
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&pol),
            ..Default::default()
        },
        &sid,
        "m",
        "compile",
        &Cancel::default(),
        |e| events.push(e),
    )
    .unwrap();

    assert!(report.text.contains("corrigido"), "{}", report.text);
    assert!(report.saved_rtk_tokens > 0);
    assert!(report.saved_ponytail_tokens > 0);
    assert!(report.saved_caveman_tokens > 0);
    assert_eq!(
        report.saved_tokens,
        report.saved_rtk_tokens + report.saved_ponytail_tokens + report.saved_caveman_tokens
    );
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::TokenSaved { tool, before_bytes, after_bytes, filter }
            if tool == "list_dir"
                && *after_bytes < *before_bytes
                && filter.as_deref() == Some("build-output")
    )));

    let seen = fake.seen.lock().unwrap();
    let sys = seen[0]["messages"][0]["content"].as_str().unwrap();
    assert!(sys.contains("Ponytail Full"), "{sys}");
    assert!(sys.contains("Caveman Full"), "{sys}");

    let second_msgs = seen[1]["messages"].as_array().unwrap();
    let tool_msg = second_msgs.iter().find(|m| m["role"] == "tool").unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(tool_msg.contains("Compiled 25 packages"), "{tool_msg}");
    assert!(
        tool_msg.contains("error[E0308]: mismatched types"),
        "{tool_msg}"
    );
    assert!(tool_msg.contains("--> src/lib.rs:42:9"), "{tool_msg}");
    assert!(!tool_msg.contains("Compiling pkg-12"), "{tool_msg}");

    let summary = store.usage_summary(24).unwrap();
    assert_eq!(summary.total_saved_rtk_tokens, report.saved_rtk_tokens);
    assert_eq!(
        summary.total_saved_ponytail_tokens,
        report.saved_ponytail_tokens
    );
    assert_eq!(
        summary.total_saved_caveman_tokens,
        report.saved_caveman_tokens
    );
    assert_eq!(summary.total_saved_tokens, report.saved_tokens);
}
