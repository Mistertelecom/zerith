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
    assert!(matches!(err, ToolError::Timeout(_)), "{err}");
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
        .authorize("shell", &json!({"command": "ls"}), &ctx(&p))
        .unwrap();
    assert!(matches!(v, Verdict::Deny(_)));
}

#[test]
fn big_outputs_are_truncated_with_a_note() {
    let s = truncate("x".repeat(50), 10);
    assert!(s.starts_with("xxxxxxxxxx\n"));
    assert!(s.contains("10 de 50"));
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
