//! The shell's own logic, without a GPU: `App::new` builds the state container
//! but no window, so these run beside the rest of the workspace.

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
