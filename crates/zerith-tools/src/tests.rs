use super::*;
use serde_json::json;
use zerith_security::{preset, Mode};

fn scratch() -> tempfile::TempDir {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&p).unwrap();
    tempfile::Builder::new().tempdir_in(p).unwrap()
}

fn policy(dir: &Path, mode: Mode) -> Policy {
    Policy::new(preset("workspace-write").unwrap(), mode, dir)
}

fn ctx(policy: &Policy) -> CallCtx<'_> {
    CallCtx {
        workspace: policy.workspace.as_path(),
        policy,
        memory: None,
        scope: None,
    }
}

#[test]
fn the_catalog_lists_every_tool_with_a_description() {
    let r = Registry::new();
    let cat = r.catalog();
    assert!(cat.contains("read_file: "));
    assert_eq!(cat.lines().count(), r.specs().len());
}

#[test]
fn writing_inside_the_workspace_succeeds_and_reads_back_numbered() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::AcceptEdits);
    let (r, c) = (Registry::new(), ctx(&p));
    r.call(
        "write_file",
        &json!({"path": "a/b.txt", "content": "um\ndois\ntres"}),
        &c,
    )
    .unwrap();
    let out = r
        .call(
            "read_file",
            &json!({"path": "a/b.txt", "offset": 2, "limit": 1}),
            &c,
        )
        .unwrap();
    assert!(out.starts_with("2\tdois\n"), "{out}");
    assert!(out.contains("linhas 2-2 de 3"), "{out}");
}

#[test]
fn a_write_outside_the_workspace_is_not_silently_allowed() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::AcceptEdits);
    let r = Registry::new();
    let args = json!({"path": "../../outside.txt", "content": "x"});
    let (v, _) = r.authorize("write_file", &args, &ctx(&p)).unwrap();
    assert!(matches!(v, Verdict::Ask(_)));
    assert!(matches!(
        r.call("write_file", &args, &ctx(&p)),
        Err(ToolError::Denied(_))
    ));
}

#[test]
fn reads_outside_the_workspace_ask_instead_of_reading_anything() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Default);
    let r = Registry::new();
    let outside = dir.path().parent().unwrap().to_string_lossy().to_string();
    for (tool, args) in [
        ("read_file", json!({"path": format!("{outside}/x")})),
        ("list_dir", json!({"path": outside})),
        ("glob", json!({"pattern": "*", "path": outside})),
        ("grep", json!({"query": "x", "path": outside})),
    ] {
        let (v, _) = r.authorize(tool, &args, &ctx(&p)).unwrap();
        assert!(matches!(v, Verdict::Ask(_)), "{tool}: {v:?}");
    }
}

#[test]
fn edit_requires_a_unique_match_unless_replace_all() {
    let dir = scratch();
    std::fs::write(dir.path().join("f.txt"), "a x a").unwrap();
    let p = policy(dir.path(), Mode::AcceptEdits);
    let (r, c) = (Registry::new(), ctx(&p));
    let dup = r
        .call(
            "edit_file",
            &json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
            &c,
        )
        .unwrap_err();
    assert!(dup.to_string().contains("2 vezes"), "{dup}");
    let missing = r
        .call(
            "edit_file",
            &json!({"path": "f.txt", "old_string": "zz", "new_string": "b"}),
            &c,
        )
        .unwrap_err();
    assert!(missing.to_string().contains("não encontrado"));
    r.call(
        "edit_file",
        &json!({"path": "f.txt", "old_string": "a", "new_string": "b", "replace_all": true}),
        &c,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "b x b"
    );
}

#[test]
fn edit_tolerates_lost_indentation_and_points_at_the_closest_line() {
    let dir = scratch();
    let f = dir.path().join("m.rs");
    std::fs::write(
        &f,
        "fn main() {
    let total = soma(a, b);
    println!(\"{total}\");
}
",
    )
    .unwrap();
    let p = policy(dir.path(), Mode::AcceptEdits);
    let (r, c) = (Registry::new(), ctx(&p));
    r.call("read_file", &json!({"path": "m.rs"}), &c).unwrap();
    let out = r
        .call(
            "edit_file",
            &json!({"path": "m.rs", "old_string": "let total = soma(a, b);
println!(\"{total}\");", "new_string": "    let total = soma(a, c);
    println!(\"{total}\");"}),
            &c,
        )
        .unwrap();
    assert!(out.contains("ignorando espaços"), "{out}");
    assert!(std::fs::read_to_string(&f).unwrap().contains(
        "    let total = soma(a, c);
    println!"
    ));
    let miss = r
        .call(
            "edit_file",
            &json!({"path": "m.rs", "old_string": "let total = soma(x, y, z);", "new_string": "x"}),
            &c,
        )
        .unwrap_err()
        .to_string();
    assert!(
        miss.contains("linhas mais parecidas") && miss.contains("2	    let total = soma(a, c);"),
        "{miss}"
    );
}

#[test]
fn glob_and_grep_walk_the_tree_and_skip_target() {
    let dir = scratch();
    std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("src/sub/a.rs"), "fn Main() {}").unwrap();
    std::fs::write(dir.path().join("src/b.txt"), "main").unwrap();
    std::fs::write(dir.path().join("target/c.rs"), "fn main() {}").unwrap();
    let p = policy(dir.path(), Mode::Default);
    let (r, c) = (Registry::new(), ctx(&p));
    let g = r.call("glob", &json!({"pattern": "**/*.rs"}), &c).unwrap();
    assert_eq!(g, "src/sub/a.rs");
    let hits = r
        .call("grep", &json!({"query": "main", "glob": "*.rs"}), &c)
        .unwrap();
    assert_eq!(hits, "src/sub/a.rs:1: fn Main() {}");
    std::fs::write(
        dir.path().join("src/d.rs"),
        "fn f() {
    let alvo = 1;
}
",
    )
    .unwrap();
    let indented = r.call("grep", &json!({"query": "alvo"}), &c).unwrap();
    assert!(
        indented.ends_with(":2:     let alvo = 1;"),
        "a indentação é o que o edit_file precisa copiar: {indented}"
    );
}

#[test]
fn list_dir_sorts_and_marks_directories() {
    let dir = scratch();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("b.txt"), "x").unwrap();
    let p = policy(dir.path(), Mode::Default);
    let out = Registry::new()
        .call("list_dir", &json!({"path": "."}), &ctx(&p))
        .unwrap();
    assert_eq!(out, "b.txt\nsub/");
}

#[test]
fn shell_runs_captures_output_and_reports_failure() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let (r, c) = (Registry::new(), ctx(&p));
    let ok = r
        .call("shell", &json!({"command": "echo zerith"}), &c)
        .unwrap();
    assert!(ok.starts_with("exit 0") && ok.contains("zerith"), "{ok}");
    let bad = r
        .call("shell", &json!({"command": "exit 3"}), &c)
        .unwrap_err();
    assert!(bad.to_string().starts_with("exit 3"), "{bad}");
}

#[test]
fn shell_is_killed_at_its_timeout_not_after_it_returns() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let sleep = if cfg!(windows) {
        "ping -n 30 127.0.0.1 > NUL"
    } else {
        "sleep 30"
    };
    let started = Instant::now();
    let err = Registry::new()
        .call(
            "shell",
            &json!({"command": sleep, "timeout_secs": 1}),
            &ctx(&p),
        )
        .unwrap_err();
    // The timeout now says what happened and shows what was printed before it.
    assert!(err.to_string().contains("tempo esgotado"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn shell_stops_when_the_turn_is_cancelled() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c2.cancel();
    });
    let sleep = if cfg!(windows) {
        "ping -n 30 127.0.0.1 > NUL"
    } else {
        "sleep 30"
    };
    let err = Registry::new()
        .run_approved("shell", &json!({"command": sleep}), &ctx(&p), &cancel)
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled), "{err}");
}

#[test]
fn plan_mode_denies_shell_and_writes() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Plan);
    let r = Registry::new();
    let (v, _) = r
        .authorize("shell", &json!({"command": "rm -rf alvo"}), &ctx(&p))
        .unwrap();
    assert!(matches!(v, Verdict::Deny(_)));
}

#[test]
fn big_outputs_are_truncated_with_a_note() {
    let s = truncate(format!("{}error: falhou", "x".repeat(50)), 30);
    assert!(s.starts_with("xxxxxxxxxxxxxxxxxxxx\n"));
    assert!(s.contains("30 de 63"));
    assert!(
        s.ends_with("\n\nor: falhou"),
        "o fim, onde ficam os erros, precisa sobrar: {s}"
    );
}

#[test]
fn an_unknown_tool_names_itself_in_the_error() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Default);
    let err = Registry::new()
        .call("nope", &json!({}), &ctx(&p))
        .unwrap_err();
    assert!(err.to_string().contains("nope"));
}

#[test]
fn only_read_only_tools_are_concurrency_safe() {
    let r = Registry::new();
    for name in ["read_file", "list_dir", "glob", "grep", "memory_recall"] {
        assert!(r.concurrency_safe(name, &json!({})), "{name}");
    }
    for name in ["write_file", "edit_file", "shell", "nope"] {
        assert!(!r.concurrency_safe(name, &json!({})), "{name}");
    }
    assert!(r.uses_memory("memory_recall") && !r.uses_memory("grep"));
}

#[test]
fn grep_modes_case_and_head_limit() {
    let dir = scratch();
    std::fs::write(dir.path().join("a.txt"), "Foo\nfoo\nbar\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "FOO\n").unwrap();
    let p = policy(dir.path(), Mode::Default);
    let (r, c) = (Registry::new(), ctx(&p));
    let g = |args: serde_json::Value| r.call("grep", &args, &c).unwrap();

    assert_eq!(
        g(json!({"query": "foo", "output_mode": "files"})),
        "a.txt\nb.txt"
    );
    assert_eq!(
        g(json!({"query": "foo", "output_mode": "count"})),
        "a.txt:2\nb.txt:1"
    );
    // `-i: false` is case-sensitive.
    assert_eq!(
        g(json!({"query": "foo", "-i": false, "output_mode": "count"})),
        "a.txt:1"
    );
    let limited = g(json!({"query": "foo", "head_limit": 2}));
    assert_eq!(limited.lines().count(), 3, "{limited}");
    assert!(limited.contains("[parado em 2 resultados]"));
    assert!(r
        .call("grep", &json!({"query": "x", "output_mode": "xml"}), &c)
        .is_err());
}

/// A grandchild of the shell must die with it. The grandchild appends to a
/// file; once the shell is killed the file must stop growing.
#[test]
fn shell_timeout_kills_the_whole_process_tree() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let cmd = if cfg!(windows) {
        // cmd starts ping as a child: ping is the grandchild of the shell tool.
        "ping -n 40 127.0.0.1 > grand.txt"
    } else {
        "sh -c 'while :; do echo x >> grand.txt; sleep 1; done'"
    };
    let err = Registry::new()
        .call(
            "shell",
            &json!({"command": cmd, "timeout_secs": 2}),
            &ctx(&p),
        )
        .unwrap_err();
    assert!(err.to_string().contains("tempo esgotado"), "{err}");
    let size = || std::fs::metadata(dir.path().join("grand.txt")).map(|m| m.len());
    std::thread::sleep(Duration::from_millis(500));
    let first = size().unwrap();
    assert!(first > 0, "the grandchild never ran");
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(size().unwrap(), first, "grandchild survived the kill");
}

#[test]
fn memory_save_writes_once_and_respects_the_pause() {
    use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
    let dir = scratch();
    let p = policy(dir.path(), Mode::Default);
    let mem = SqliteMemory::open(dir.path().join("m.db")).unwrap();
    let scope = Scope::user("t");
    let c = CallCtx {
        memory: Some(&mem),
        scope: Some(&scope),
        ..ctx(&p)
    };
    let r = Registry::new();
    let args = json!({"text": "O usuário prefere o editor Helix", "kind": "preference"});
    // No approval needed, in the strictest asking mode.
    let out = r.call("memory_save", &args, &c).unwrap();
    assert!(out.starts_with("salvo"), "{out}");
    let again = r.call("memory_save", &args, &c).unwrap();
    assert!(again.starts_with("já estava"), "{again}");
    let facts = mem.list(&scope, 10).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, zerith_memory::FactKind::Preference);
    assert!(r.uses_memory("memory_save"));

    let bad = r.call("memory_save", &json!({"text": "x", "kind": "segredo"}), &c);
    assert!(bad.is_err());
    mem.set_paused(true).unwrap();
    let paused = r.call("memory_save", &json!({"text": "outro fato"}), &c);
    assert!(matches!(paused, Err(ToolError::Denied(_))), "{paused:?}");
    assert_eq!(mem.list(&scope, 10).unwrap().len(), 1);
}

/// `cmd` never understood the `\"` that Rust wrote through `.arg`: every path
/// with a space and every `-m "..."` broke.
/// Same check for Git Bash, the default shell when it is installed.
#[cfg(windows)]
#[test]
fn quotes_reach_the_shell_verbatim() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    std::fs::write(dir.path().join("a b.txt"), "conteudo").unwrap();
    let show = if zerith_core::shellparse::windows_bash().is_some() {
        "cat"
    } else {
        "type"
    };
    let ok = Registry::new()
        .call(
            "shell",
            &json!({"command": format!("{show} \"a b.txt\"")}),
            &ctx(&p),
        )
        .expect("a quoted path with a space");
    assert!(ok.contains("conteudo"), "{ok}");
    let echo = Registry::new()
        .call("shell", &json!({"command": "echo \"a b\""}), &ctx(&p))
        .expect("echo with quotes");
    assert!(echo.contains("a b"), "{echo}");
}

/// A file written with CRLF is edited with strings the model saw through
/// `.lines()`, i.e. with LF.
#[test]
fn a_crlf_file_takes_a_multi_line_edit() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let path = dir.path().join("crlf.rs");
    std::fs::write(&path, "um\r\ndois\r\ntres\r\n").unwrap();
    Registry::new()
        .call(
            "edit_file",
            &json!({"path": "crlf.rs", "old_string": "um\ndois", "new_string": "um\nDOIS\nmeio"}),
            &ctx(&p),
        )
        .expect("multi-line edit of a CRLF file");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "um\r\nDOIS\r\nmeio\r\ntres\r\n", "{after:?}");
}

/// A legacy Latin-1 file still reads; editing it says why it cannot.
#[test]
fn a_non_utf8_file_reads_and_refuses_to_be_edited() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    std::fs::write(dir.path().join("velho.txt"), b"N\xfamero 1\n").unwrap();
    let text = Registry::new()
        .call("read_file", &json!({"path": "velho.txt"}), &ctx(&p))
        .expect("a CP1252 file is readable");
    assert!(text.contains("mero"), "{text}");
    let err = Registry::new()
        .call(
            "edit_file",
            &json!({"path": "velho.txt", "old_string": "mero", "new_string": "x"}),
            &ctx(&p),
        )
        .unwrap_err();
    assert!(err.to_string().contains("UTF-8"), "{err}");
}

/// `ping`/`dir` write the OEM code page: read as UTF-8, `Número` came out
/// as `N\uFFFDmero`.
#[test]
fn console_output_is_read_in_the_machine_code_page() {
    let text = if cfg!(windows) {
        // CP850: 'N' 'u' 'm' 'e' 'r' 'o'  with 'u' = 0x97 in CP850.
        super::builtin::decode(b"N\x97mero")
    } else {
        "Número".to_string()
    };
    assert!(text.contains("mero"), "{text}");
    assert_eq!(super::builtin::decode(b"plain ascii"), "plain ascii");
}

/// A command that leaves a program behind (`start /b`, a dev server) used to
/// keep the output pipes open forever: `out.join()` waited for a process that
/// is not the command. It must return, and say where that program belongs.
/// Written for the shell that runs it: `&` in Git Bash, `start /b` in cmd.
#[cfg(windows)]
#[test]
fn a_grandchild_holding_the_pipe_does_not_hang_the_shell() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let cmd = if zerith_core::shellparse::windows_bash().is_some() {
        "ping -n 12 127.0.0.1 > /dev/null &"
    } else {
        "start /b ping -n 12 127.0.0.1 > NUL"
    };
    let started = Instant::now();
    let out = Registry::new()
        .call(
            "shell",
            &json!({"command": cmd, "timeout_secs": 20}),
            &ctx(&p),
        )
        .expect("the shell itself finished at once");
    let took = started.elapsed();
    assert!(took < Duration::from_secs(8), "took {took:?}");
    assert!(out.contains("exit 0"), "{out}");
}

/// Rewind snapshots the workspace before a shell command that may change it,
/// in the foreground or the background, and never before a read-only one.
#[test]
fn the_shell_hook_fires_only_for_commands_that_may_mutate() {
    let dir = scratch();
    let p = policy(dir.path(), Mode::Bypass);
    let (r, c) = (Registry::new(), ctx(&p));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    r.host()
        .on_before_shell(move |ws| log.lock().unwrap().push(ws.to_path_buf()));
    r.call("shell", &json!({"command": "echo oi"}), &c).unwrap();
    assert!(seen.lock().unwrap().is_empty(), "read-only: no snapshot");
    r.call("shell", &json!({"command": "echo x > f.txt"}), &c)
        .unwrap();
    r.call(
        "shell",
        &json!({"command": "echo y > g.txt", "run_in_background": true}),
        &c,
    )
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![dir.path().to_path_buf(); 2]);
}
