//! Read-before-edit, background shell, web_fetch and the argument checks of
//! the tools the runtime settles (tasks, ask_user, skill).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_providers::Cancel;
use zerith_security::{preset, Mode, Policy};
use zerith_tools::{AgentRequest, CallCtx, EventSink, Registry, Services, ToolError};

struct NoServices;

impl Services for NoServices {
    fn agent(
        &self,
        _: &str,
        _: AgentRequest,
        _: &Cancel,
        _: Instant,
        _: EventSink,
    ) -> Result<String, String> {
        Err("sem subagentes".into())
    }
    fn agent_result(&self, _: &str, _: &str) -> Result<String, String> {
        Err("sem subagentes".into())
    }
    fn tasks(&self, _: &str, _: &str, _: &Value) -> Result<String, String> {
        Err("sem tarefas".into())
    }
    fn skill(&self, _: &str) -> Result<zerith_tools::SkillLoad, String> {
        Err("sem skills".into())
    }
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

fn policy(dir: &std::path::Path) -> Policy {
    Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, dir)
}

fn call(r: &Registry, p: &Policy, name: &str, args: Value) -> Result<String, ToolError> {
    let ctx = CallCtx {
        workspace: &p.workspace,
        policy: p,
        memory: None,
        scope: None,
    };
    r.call(name, &args, &ctx)
}

fn bound(session: &str) -> Registry {
    let r = Registry::new();
    r.host().bind(session, Arc::new(NoServices));
    r
}

// --- read-before-edit -------------------------------------------------------

#[test]
fn editing_an_unread_file_is_refused_until_it_is_read() {
    let dir = scratch();
    std::fs::write(dir.path().join("a.txt"), "um dois").unwrap();
    let p = policy(dir.path());
    let r = bound("s-unread");
    let edit = json!({"path": "a.txt", "old_string": "um", "new_string": "1"});

    let e = call(&r, &p, "edit_file", edit.clone())
        .unwrap_err()
        .to_string();
    assert!(e.contains("read_file"), "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "um dois"
    );
    let w = call(
        &r,
        &p,
        "write_file",
        json!({"path": "a.txt", "content": "x"}),
    )
    .unwrap_err()
    .to_string();
    assert!(w.contains("read_file"), "{w}");

    call(&r, &p, "read_file", json!({"path": "a.txt"})).unwrap();
    call(&r, &p, "edit_file", edit).unwrap();
    // The edit refreshed what the session knows: a second edit needs no re-read.
    call(
        &r,
        &p,
        "edit_file",
        json!({"path": "a.txt", "old_string": "dois", "new_string": "2"}),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "1 2"
    );
}

#[test]
fn a_file_changed_after_the_read_must_be_read_again() {
    let dir = scratch();
    let f = dir.path().join("b.txt");
    std::fs::write(&f, "v1").unwrap();
    let p = policy(dir.path());
    let r = bound("s-stale");
    call(&r, &p, "read_file", json!({"path": "b.txt"})).unwrap();

    // Someone else rewrites it; force a distinct mtime on coarse filesystems.
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&f, "v2 externo").unwrap();
    let t = std::fs::metadata(&f).unwrap().modified().unwrap() + Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(t)
        .unwrap();

    let e = call(
        &r,
        &p,
        "edit_file",
        json!({"path": "b.txt", "old_string": "v2", "new_string": "v3"}),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("mudou"), "{e}");
    call(&r, &p, "read_file", json!({"path": "b.txt"})).unwrap();
    call(
        &r,
        &p,
        "edit_file",
        json!({"path": "b.txt", "old_string": "v2", "new_string": "v3"}),
    )
    .unwrap();
}

#[test]
fn new_files_need_no_read_and_sessions_do_not_share_reads() {
    let dir = scratch();
    let p = policy(dir.path());
    let a = bound("s-a");
    call(
        &a,
        &p,
        "write_file",
        json!({"path": "novo.txt", "content": "oi"}),
    )
    .unwrap();
    // `a` wrote it, so it may edit; `b` (another session) has not seen it.
    call(
        &a,
        &p,
        "edit_file",
        json!({"path": "novo.txt", "old_string": "oi", "new_string": "ola"}),
    )
    .unwrap();
    let b = bound("s-b");
    assert!(call(
        &b,
        &p,
        "edit_file",
        json!({"path": "novo.txt", "old_string": "ola", "new_string": "x"}),
    )
    .is_err());
}

#[test]
fn an_unbound_registry_keeps_the_old_permissive_behaviour() {
    let dir = scratch();
    std::fs::write(dir.path().join("c.txt"), "x").unwrap();
    let p = policy(dir.path());
    let r = Registry::new();
    call(
        &r,
        &p,
        "edit_file",
        json!({"path": "c.txt", "old_string": "x", "new_string": "y"}),
    )
    .unwrap();
}

// --- background shell -------------------------------------------------------

fn wait_for(r: &Registry, p: &Policy, id: &str, needle: &str) -> String {
    let mut all = String::new();
    for _ in 0..200 {
        all.push_str(&call(r, p, "shell_output", json!({"id": id})).unwrap());
        if all.contains(needle) {
            return all;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never saw {needle:?} in {all:?}");
}

fn job_id(s: &str) -> String {
    s.split_whitespace().next().unwrap().to_string()
}

#[test]
fn a_background_job_returns_at_once_and_streams_new_lines_only() {
    let dir = scratch();
    let p = policy(dir.path());
    let r = bound("s-bg");
    let cmd = if cfg!(windows) {
        "echo primeiro&& powershell -NoProfile -Command Start-Sleep 1&& echo segundo"
    } else {
        "echo primeiro; sleep 1; echo segundo"
    };
    let t = Instant::now();
    let started = call(
        &r,
        &p,
        "shell",
        json!({"command": cmd, "run_in_background": true}),
    )
    .unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "did not return at once"
    );
    let id = job_id(&started);

    let first = wait_for(&r, &p, &id, "primeiro");
    assert!(first.contains("primeiro"));
    let rest = wait_for(&r, &p, &id, "segundo");
    assert!(!rest.contains("primeiro\nprimeiro"), "{rest}");
    // After the exit the state says so and the old lines are not repeated.
    let mut done = String::new();
    for _ in 0..100 {
        done = call(&r, &p, "shell_output", json!({"id": id})).unwrap();
        if done.contains("terminou") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(done.contains("terminou (exit 0)"), "{done}");
    assert!(
        !done.contains("primeiro") && !done.contains("segundo"),
        "{done}"
    );
}

#[test]
fn a_job_can_be_killed_and_the_table_is_capped_at_four() {
    let dir = scratch();
    let p = policy(dir.path());
    let r = bound("s-cap");
    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1 >nul"
    } else {
        "sleep 30"
    };
    let mut ids = Vec::new();
    for _ in 0..4 {
        let s = call(
            &r,
            &p,
            "shell",
            json!({"command": long, "run_in_background": true}),
        )
        .unwrap();
        ids.push(job_id(&s));
    }
    let fifth = call(
        &r,
        &p,
        "shell",
        json!({"command": long, "run_in_background": true}),
    )
    .unwrap_err()
    .to_string();
    assert!(fifth.contains("4 jobs"), "{fifth}");

    let k = call(&r, &p, "shell_kill", json!({"id": ids[0]})).unwrap();
    assert!(k.contains("encerrado"), "{k}");
    let after = call(&r, &p, "shell_output", json!({"id": ids[0]})).unwrap();
    assert!(after.contains("terminou"), "{after}");
    // A slot is free again.
    let again = call(
        &r,
        &p,
        "shell",
        json!({"command": long, "run_in_background": true}),
    )
    .unwrap();
    assert!(again.contains("job-"));
    assert!(call(&r, &p, "shell_kill", json!({"id": "job-0"})).is_err());
    // Dropping the registry kills the rest (no leaked ping/sleep for 30 s).
    drop(r);
}

// --- web_fetch --------------------------------------------------------------

fn serve_once(body: &'static str, ctype: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/p", l.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn web_fetch_blocks_loopback_unless_private_is_allowed() {
    let dir = scratch();
    let p = policy(dir.path());
    let r = Registry::new();
    let url = serve_once("<p>segredo interno</p>", "text/html");
    let e = call(&r, &p, "web_fetch", json!({"url": url}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("bloqueado") || e.contains("privado"), "{e}");

    let url = serve_once(
        "<html><body><h1>Oi</h1><p>mundo &amp; mais</p></body></html>",
        "text/html",
    );
    let out = call(
        &r,
        &p,
        "web_fetch",
        json!({"url": url, "allow_private": true, "prompt": "ache o título"}),
    )
    .unwrap();
    assert!(out.starts_with("[pedido: ache o título]"), "{out}");
    assert!(out.contains("Oi\nmundo & mais"), "{out}");
}

#[test]
fn web_fetch_rejects_other_schemes_and_asks_through_the_policy() {
    let dir = scratch();
    let p = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let r = Registry::new();
    for url in ["file:///etc/passwd", "ftp://x/y", "javascript:alert(1)"] {
        let e = call(
            &r,
            &policy(dir.path()),
            "web_fetch",
            json!({"url": url, "allow_private": true}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("http"), "{url}: {e}");
    }
    let ctx = CallCtx {
        workspace: &p.workspace,
        policy: &p,
        memory: None,
        scope: None,
    };
    let (v, action) = r
        .authorize("web_fetch", &json!({"url": "https://example.com"}), &ctx)
        .unwrap();
    assert!(matches!(v, zerith_security::Verdict::Ask(_)), "{v:?}");
    assert!(matches!(action, Some(zerith_security::Action::Network(_))));
}

#[test]
fn web_fetch_caps_the_body_and_stops_on_cancel() {
    let dir = scratch();
    let p = policy(dir.path());
    let r = Registry::new();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/big", l.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let chunk = vec![b'a'; 64 * 1024];
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n"
            );
            for _ in 0..40 {
                if s.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
    });
    // run_raw: the registry's own 30k output cap would hide the fetch cap.
    let ctx = CallCtx {
        workspace: &p.workspace,
        policy: &p,
        memory: None,
        scope: None,
    };
    let out = r
        .run_raw(
            "web_fetch",
            &json!({"url": url, "allow_private": true}),
            &ctx,
            &Cancel::default(),
        )
        .unwrap();
    assert!(
        out.ends_with("[página cortada em 1 MB]"),
        "tail: {}",
        &out[out.len().saturating_sub(40)..]
    );
    assert!(out.len() < (1 << 20) + 100);

    // A server that never answers: cancel must return promptly.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hang", l.local_addr().unwrap());
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.cancel();
    });
    let ctx = CallCtx {
        workspace: &p.workspace,
        policy: &p,
        memory: None,
        scope: None,
    };
    let t = Instant::now();
    let e = r
        .run_approved(
            "web_fetch",
            &json!({"url": url, "allow_private": true}),
            &ctx,
            &cancel,
        )
        .unwrap_err();
    assert!(matches!(e, ToolError::Cancelled), "{e}");
    assert!(t.elapsed() < Duration::from_secs(3));
    drop(l);
}

#[test]
fn web_search_exists_without_configuration() {
    // No ZERITH_SEARCH_URL: DuckDuckGo. With it: the endpoint.
    assert!(Registry::new().get("web_search").is_some());
    let mut r = Registry::empty();
    r.register(Box::new(zerith_tools::SearchTool::new(
        "http://127.0.0.1:1/s",
    )));
    assert!(r.get("web_search").is_some());
}

#[test]
fn search_tool_returns_the_endpoint_json() {
    let dir = scratch();
    let p = policy(dir.path());
    let url = serve_once(r#"{"results":[{"title":"x"}]}"#, "application/json");
    let mut r = Registry::empty();
    r.register(Box::new(zerith_tools::SearchTool::new(format!(
        "{url}?q={{query}}"
    ))));
    let out = call(&r, &p, "web_search", json!({"query": "rust lang"})).unwrap();
    assert!(out.contains("\"title\": \"x\""), "{out}");
}

// --- tools the runtime settles ---------------------------------------------

#[test]
fn argument_checks_for_tasks_ask_user_and_skill() {
    assert!(zerith_tools::check_task_args("task_create", &json!({})).is_err());
    assert!(zerith_tools::check_task_args("task_create", &json!({"subject": "a"})).is_ok());
    assert!(
        zerith_tools::check_task_args("task_update", &json!({"id": 1, "status": "feito"})).is_err()
    );
    assert!(
        zerith_tools::check_task_args("task_update", &json!({"id": 1, "status": "completed"}))
            .is_ok()
    );
    assert!(zerith_tools::check_skill_args(&json!({"name": "x"})).is_ok());
    assert!(zerith_tools::check_skill_args(&json!({})).is_err());

    let q = json!({"questions": [{"question": "Qual?", "header": "Escolha muito longa mesmo",
        "options": [{"label": "A"}, {"label": "B", "description": "d"}], "multi_select": true}]});
    let parsed = zerith_tools::parse_questions(&q).unwrap();
    assert_eq!(parsed[0].header.chars().count(), 12);
    assert!(parsed[0].multi_select);
    assert!(zerith_tools::parse_questions(&json!({"questions": []})).is_err());
    assert!(zerith_tools::parse_questions(
        &json!({"questions": [{"question": "q", "options": [{"label": "só uma"}]}]})
    )
    .is_err());

    // Called straight through an unbound registry, they fail with a reason.
    let dir = scratch();
    let p = policy(dir.path());
    let r = Registry::new();
    let e = call(&r, &p, "task_create", json!({"subject": "a"}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("indisponível"), "{e}");
}

#[test]
fn the_new_tools_are_in_the_default_registry() {
    let r = Registry::new();
    for n in [
        "task_create",
        "task_update",
        "task_list",
        "agent",
        "agent_result",
        "skill",
        "skill_search",
        "ask_user",
        "shell_output",
        "shell_kill",
        "web_fetch",
    ] {
        assert!(r.get(n).is_some(), "{n}");
    }
    assert!(r.concurrency_safe("agent", &json!({})));
    assert!(!r.concurrency_safe("task_create", &json!({})));
}

/// The shell tool runs inside the jail: a real command still works, and a
/// timeout takes the whole tree down instead of leaving a grandchild behind.
#[test]
#[cfg(windows)]
fn shell_runs_jailed_and_a_timeout_kills_the_tree() {
    use std::time::{Duration, Instant};
    use zerith_providers::Cancel;
    use zerith_security::{Policy, PRESETS};

    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let policy = Policy::new(&PRESETS[2], zerith_security::Mode::Bypass, dir.path());
    let reg = zerith_tools::Registry::new();
    let ctx = zerith_tools::CallCtx {
        workspace: dir.path(),
        policy: &policy,
        memory: None,
        scope: None,
    };
    let ok = reg
        .run_approved(
            "shell",
            &serde_json::json!({"command": "echo jailed-ok"}),
            &ctx,
            &Cancel::default(),
        )
        .expect("a jailed command still runs");
    assert!(ok.contains("jailed-ok"), "{ok}");

    let start = Instant::now();
    let err = reg
        .run_approved(
            "shell",
            &serde_json::json!({"command": "ping -n 30 127.0.0.1 >nul", "timeout_secs": 1}),
            &ctx,
            &Cancel::default(),
        )
        .unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "timeout did not stop the tree: {err}"
    );
}

/// The point of shipping a program with a skill: the model's shell finds it
/// with no install step and without touching the user's own PATH. Uses a tiny
/// stand-in program so the test needs no real tool.
#[test]
#[cfg(windows)]
fn the_shell_finds_a_program_that_ships_with_the_app() {
    use zerith_providers::Cancel;
    use zerith_security::{Mode, Policy, PRESETS};

    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    // A batch file named like a program: `zerith-bundled-probe` on PATH.
    std::fs::write(
        tools.join("zerith-bundled-probe.cmd"),
        "@echo bundled-ok\r\n",
    )
    .unwrap();

    let policy = Policy::new(&PRESETS[2], Mode::Bypass, dir.path());
    let reg = zerith_tools::Registry::new();
    let ctx = zerith_tools::CallCtx {
        workspace: dir.path(),
        policy: &policy,
        memory: None,
        scope: None,
    };
    let run = |cmd: &str| {
        reg.run_approved(
            "shell",
            &serde_json::json!({"command": cmd}),
            &ctx,
            &Cancel::default(),
        )
    };

    // Not on the user's PATH: not found.
    assert!(run("zerith-bundled-probe").is_err());

    // Same call with the bundled folder announced: found.
    std::env::set_var("ZERITH_BUNDLED_TOOLS", &tools);
    let out = run("zerith-bundled-probe").expect("found through the bundled folder");
    std::env::remove_var("ZERITH_BUNDLED_TOOLS");
    assert!(out.contains("bundled-ok"), "{out}");
}
