//! Guards middleware: rules, plan gate, hooks, against the pipeline contract.

use serde_json::json;
use std::path::PathBuf;
use zerith_core::settings::Settings;
use zerith_runtime::guards::{Guards, EXIT_PLAN_MODE};
use zerith_runtime::pipeline::{Flow, Middleware, ToolInvocation, ToolOutcome};
use zerith_security::{Mode, Policy};

fn scratch() -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new().tempdir_in(root).unwrap()
}

fn guards(dir: &std::path::Path, toml_text: &str) -> (Guards, Policy) {
    let settings: Settings = toml::from_str(toml_text).unwrap();
    let mut policy = Policy::from_settings_with_env(&settings, dir, |_| None).unwrap();
    (Guards::new(settings, &mut policy, "s1"), policy)
}

fn call(dir: &std::path::Path, name: &str, args: serde_json::Value) -> ToolInvocation {
    ToolInvocation {
        id: "c1".into(),
        name: name.into(),
        args,
        session_id: "s1".into(),
        cwd: dir.to_path_buf(),
    }
}

#[cfg(windows)]
const DENY_JSON: &str = r#"echo {"decision":"deny","reason":"nao"}"#;
#[cfg(not(windows))]
const DENY_JSON: &str = r#"echo '{"decision":"deny","reason":"nao"}'"#;
#[cfg(windows)]
const ASK_JSON: &str = r#"echo {"decision":"ask","reason":"confirme"}"#;
#[cfg(not(windows))]
const ASK_JSON: &str = r#"echo '{"decision":"ask","reason":"confirme"}'"#;
#[cfg(windows)]
const REWRITE: &str =
    r#"echo {"updated_input":{"command":"echo reescrito"},"additional_context":"visto"}"#;
#[cfg(not(windows))]
const REWRITE: &str =
    r#"echo '{"updated_input":{"command":"echo reescrito"},"additional_context":"visto"}'"#;
#[cfg(windows)]
const CRASH: &str = "exit /b 1";
#[cfg(not(windows))]
const CRASH: &str = "exit 1";
const NOTE: &str = "echo lembrete";

fn hook(event: &str, matcher: &str, command: &str) -> String {
    format!("[[hooks.{event}]]\nmatcher = {matcher:?}\ncommand = {command:?}\ntimeout_ms = 20000\n")
}

#[test]
fn pre_tool_use_hook_denies_asks_and_rewrites() {
    let dir = scratch();
    let (g, _) = guards(dir.path(), &hook("PreToolUse", "shell(rm *)", DENY_JSON));
    let mut c = call(dir.path(), "shell", json!({"command": "rm -rf x"}));
    assert_eq!(g.before(&mut c), Flow::Deny("nao".into()));
    let mut ok = call(dir.path(), "shell", json!({"command": "ls"}));
    assert_eq!(
        g.before(&mut ok),
        Flow::Continue,
        "matcher specifier gates it"
    );

    let (g, _) = guards(dir.path(), &hook("PreToolUse", "shell", ASK_JSON));
    let mut c = call(dir.path(), "shell", json!({"command": "ls"}));
    assert_eq!(g.before(&mut c), Flow::Ask("confirme".into()));

    let (g, _) = guards(dir.path(), &hook("PreToolUse", "shell", REWRITE));
    let mut c = call(dir.path(), "shell", json!({"command": "ls"}));
    assert_eq!(g.before(&mut c), Flow::Continue);
    assert_eq!(c.args, json!({"command": "echo reescrito"}));
    // The injected context reaches the model with the result.
    let mut out = ToolOutcome::new(true, "x");
    g.after(&c, &mut out);
    assert!(
        out.context.contains(&"visto".to_string()),
        "{:?}",
        out.context
    );
}

#[test]
fn a_crashing_hook_fails_open_and_is_reported() {
    let dir = scratch();
    let (g, _) = guards(dir.path(), &hook("PreToolUse", "", CRASH));
    let mut c = call(dir.path(), "shell", json!({"command": "ls"}));
    assert_eq!(g.before(&mut c), Flow::Continue);
    let errs = g.take_errors();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(g.take_errors().is_empty());
}

#[test]
fn post_hooks_pick_the_event_by_result_and_add_context() {
    let dir = scratch();
    let text = format!(
        "{}{}",
        hook("PostToolUse", "", NOTE),
        hook("PostToolUseFailure", "", "echo falhou")
    );
    let (g, _) = guards(dir.path(), &text);
    let c = call(dir.path(), "shell", json!({"command": "ls"}));
    let mut ok = ToolOutcome::new(true, "saida");
    g.after(&c, &mut ok);
    assert_eq!(ok.context, vec!["lembrete"]);
    let mut bad = ToolOutcome::new(false, "erro: x");
    g.after(&c, &mut bad);
    assert_eq!(bad.context, vec!["falhou"]);
}

#[test]
fn permission_request_hook_can_deny_a_call_that_would_ask() {
    let dir = scratch();
    let text = format!(
        "[permissions]\ndefault_mode = \"default\"\n{}",
        hook("PermissionRequest", "shell", DENY_JSON)
    );
    let (g, _) = guards(dir.path(), &text);
    let mut c = call(dir.path(), "shell", json!({"command": "make"}));
    assert!(matches!(g.before(&mut c), Flow::Deny(_)));
    // A read-only command never asks, so the hook never runs.
    let mut ls = call(dir.path(), "shell", json!({"command": "git status"}));
    assert_eq!(g.before(&mut ls), Flow::Continue);
}

#[test]
fn external_tools_follow_rules_and_modes() {
    let dir = scratch();
    let text = r#"[permissions]
default_mode = "default"
allow = ["mcp__docs__*", "skill(deploy)"]
deny = ["mcp__gh__push"]
"#;
    let (g, _) = guards(dir.path(), text);
    let mut c = call(dir.path(), "mcp__docs__search", json!({}));
    assert_eq!(g.before(&mut c), Flow::Continue);
    let mut c = call(dir.path(), "mcp__gh__push", json!({}));
    assert!(matches!(g.before(&mut c), Flow::Deny(_)));
    let mut c = call(dir.path(), "mcp__other__thing", json!({}));
    assert!(
        matches!(g.before(&mut c), Flow::Ask(_)),
        "unknown mcp tool asks"
    );
    let mut c = call(dir.path(), "skill", json!({"name": "deploy"}));
    assert_eq!(g.before(&mut c), Flow::Continue);

    let (g, _) = guards(dir.path(), "[permissions]\ndefault_mode = \"dontAsk\"");
    let mut c = call(dir.path(), "mcp__other__thing", json!({}));
    assert!(
        matches!(g.before(&mut c), Flow::Deny(_)),
        "dontAsk denies instead of asking"
    );

    let (g, _) = guards(
        dir.path(),
        "[permissions]\ndefault_mode = \"bypassPermissions\"",
    );
    let mut c = call(dir.path(), "mcp__other__thing", json!({}));
    assert_eq!(g.before(&mut c), Flow::Continue);
}

#[test]
fn the_plan_gate_asks_with_the_plan_and_switches_the_mode() {
    let dir = scratch();
    let (g, policy) = guards(dir.path(), "[permissions]\ndefault_mode = \"plan\"");
    let gate = g.plan_gate();
    assert!(gate.in_plan());

    // Empty plan: refused. Real plan: the prompt carries its text.
    let mut empty = call(dir.path(), EXIT_PLAN_MODE, json!({"plan": "  "}));
    assert!(matches!(g.before(&mut empty), Flow::Deny(_)));
    let mut c = call(
        dir.path(),
        EXIT_PLAN_MODE,
        json!({"plan": "1. mudar a\n2. testar"}),
    );
    match g.before(&mut c) {
        Flow::Ask(s) => assert!(s.contains("1. mudar a") && s.contains("2. testar"), "{s}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        policy.mode(),
        Mode::Plan,
        "asking does not leave plan; denial stays"
    );

    // Approval = the tool runs. Starting in plan: falls back to acceptEdits,
    // and the policy the agent holds sees it (shared handle).
    assert_eq!(gate.approve(), Mode::AcceptEdits);
    assert_eq!(policy.mode(), Mode::AcceptEdits);

    // Outside plan the tool is refused.
    let mut again = call(dir.path(), EXIT_PLAN_MODE, json!({"plan": "x"}));
    assert!(matches!(g.before(&mut again), Flow::Deny(_)));
}

#[test]
fn the_plan_gate_returns_to_the_previous_mode() {
    let dir = scratch();
    let (g, policy) = guards(dir.path(), "[permissions]\ndefault_mode = \"default\"");
    let gate = g.plan_gate();
    gate.enter();
    assert_eq!(policy.mode(), Mode::Plan);
    assert_eq!(gate.approve(), Mode::Default);
    assert_eq!(policy.mode(), Mode::Default);
    // The UI toggling mode while planning is respected: approve is a no-op
    // outside plan.
    gate.enter();
    g.mode_handle().set(Mode::Bypass);
    assert_eq!(gate.approve(), Mode::Bypass);
}
