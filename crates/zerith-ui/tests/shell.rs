//! The shell's own logic, without a GPU: `App::new` builds the state container
//! but no window, so these run beside the rest of the workspace.

use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_sessions::{NewMessage, Role, SessionSource, Store};
use zerith_ui::{view, Shell};

/// A shell over a database in `home`. The path is passed explicitly instead of
/// through `ZERITH_HOME` so tests can run in parallel without racing on the
/// process environment.
fn shell_at(home: &std::path::Path) -> Shell {
    let spec = rune::AppSpec::new("Zerith", (1100.0, 720.0), || unreachable!(), view);
    let app = rune::App::new(spec);
    Shell::open(home.join("zerith.db"), app.handle())
}

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir")
}

#[test]
fn an_empty_database_opens_with_no_sessions_and_no_lines() {
    let dir = tempdir();
    let shell = shell_at(dir.path());
    assert!(shell.sessions.is_empty());
    assert!(shell.lines.is_empty());
    assert!(!shell.busy);
}

#[test]
fn opening_the_window_loads_sessions_and_their_messages() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let store = Store::open(&db).expect("store");
    let s = store
        .create_session("primeira", SessionSource::Native)
        .expect("session");
    store
        .append(&s.id, &NewMessage::new(Role::User, "oi"))
        .expect("append");
    store
        .append(&s.id, &NewMessage::new(Role::Assistant, "olá"))
        .expect("append");

    let shell = shell_at(dir.path());
    assert_eq!(shell.sessions.len(), 1);
    assert_eq!(shell.sessions[0].title, "primeira");
    let roles: Vec<&str> = shell.lines.iter().map(|l| l.role).collect();
    assert_eq!(roles, vec!["user", "assistant"]);
    assert_eq!(shell.lines[1].content, "olá");
}

#[test]
fn the_window_shows_the_facts_the_turn_would_recall() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let mem = SqliteMemory::open(&db).expect("memory");
    let scope = Scope::user("local");
    mem.remember(
        &scope,
        FactKind::Fact,
        "usa Rust",
        Confidence::Confirmed,
        None,
    )
    .expect("remember");

    let mut shell = shell_at(dir.path());
    assert_eq!(shell.facts.len(), 1);
    assert_eq!(shell.facts[0].text, "usa Rust");

    let id = shell.facts[0].id;
    shell.forget(id);
    assert!(shell.facts.is_empty());
    assert!(mem.list(&scope, 10).expect("list").is_empty());
}

#[test]
fn an_unfinished_plan_shows_up_in_the_window() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let store = Store::open(&db).expect("store");
    store
        .create_plan(
            "p",
            &[zerith_sessions::NewTask {
                title: "um".into(),
                prompt: "faz um".into(),
                agent: "default".into(),
                depends_on: vec![],
            }],
        )
        .expect("plan");
    let shell = shell_at(dir.path());
    assert_eq!(shell.plan_tasks.len(), 1);
    assert_eq!(shell.plan_tasks[0].1, "um");
    assert_eq!(shell.plan_tasks[0].0, "pending");
}

#[test]
fn stop_cancels_the_token_and_does_nothing_without_a_turn() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    // No turn in flight: pressing stop must not pretend something happened.
    shell.stop();
    assert!(!shell.busy);
    let token = zerith_providers::Cancel::default();
    shell.cancel = Some(token.clone());
    shell.busy = true;
    shell.stop();
    assert!(token.is_cancelled());
    assert_eq!(shell.status, "cancelado");
    assert!(shell.cancel.is_none());
}
