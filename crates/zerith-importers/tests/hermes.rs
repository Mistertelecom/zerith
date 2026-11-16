//! Importer behavior against a miniature Hermes database.
//!
//! These live here, not inline, because the fixture needs
//! `CARGO_TARGET_TMPDIR`, which Cargo only sets for integration tests.

use std::path::{Path, PathBuf};
use zerith_importers::{import, ImportError};
use zerith_sessions::{SessionSource, Store};

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// Builds a miniature Hermes database. The importer must run from a file,
/// because that is what exercises the read-only open flag.
fn fixture(dir: &Path) -> PathBuf {
    let path = dir.join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, source TEXT, model TEXT, system_prompt TEXT,
            parent_session_id TEXT, started_at REAL, ended_at REAL,
            message_count INTEGER, tool_call_count INTEGER, input_tokens INTEGER,
            output_tokens INTEGER, cache_read_tokens INTEGER, reasoning_tokens INTEGER,
            cwd TEXT, git_branch TEXT, title TEXT, profile_name TEXT,
            archived INTEGER, pinned INTEGER, hidden INTEGER
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
            timestamp REAL, token_count INTEGER, finish_reason TEXT, reasoning TEXT,
            reasoning_content TEXT, active INTEGER, compacted INTEGER,
            api_content TEXT, display_kind TEXT, display_metadata TEXT, message_uid TEXT
         );
         INSERT INTO sessions(id, model, started_at, title, profile_name, archived)
            VALUES ('h1', 'hermes-3', 1700000000.0, 'Conversa antiga', 'default', 0);
         INSERT INTO messages(session_id, role, content, timestamp, token_count, active)
            VALUES ('h1', 'user', 'oi', 1700000001.0, 2, 1),
                   ('h1', 'assistant', 'olá', 1700000002.0, 3, 1),
                   ('h1', 'assistant', 'tombstone', 1700000003.0, 1, 0);",
    )
    .unwrap();
    drop(conn);
    path
}

#[test]
fn imports_titles_messages_and_skips_inactive_rows() {
    let dir = tmp();
    let src = fixture(dir.path());
    let store = Store::open(dir.path().join("zerith.db")).unwrap();

    let report = import(&src, "default", &store).unwrap();
    assert_eq!(report.sessions_imported, 1);
    assert_eq!(
        report.messages_imported, 2,
        "the inactive row is a tombstone"
    );

    let sessions = store.list_sessions(10).unwrap();
    assert_eq!(sessions[0].title, "Conversa antiga");
    assert_eq!(sessions[0].source, SessionSource::Hermes);
    assert_eq!(sessions[0].created_at, 1_700_000_000_000);
}

#[test]
fn a_second_run_imports_nothing_and_leaves_one_session() {
    let dir = tmp();
    let src = fixture(dir.path());
    let store = Store::open(dir.path().join("zerith.db")).unwrap();

    import(&src, "default", &store).unwrap();
    let second = import(&src, "default", &store).unwrap();
    assert_eq!(second.sessions_imported, 0);
    assert_eq!(second.sessions_skipped, 1);
    assert_eq!(store.list_sessions(10).unwrap().len(), 1);
}

#[test]
fn the_source_database_is_not_modified() {
    let dir = tmp();
    let src = fixture(dir.path());
    let before = std::fs::read(&src).unwrap();
    let store = Store::open(dir.path().join("zerith.db")).unwrap();
    import(&src, "default", &store).unwrap();
    let after = std::fs::read(&src).unwrap();
    assert_eq!(before, after, "import must not write to the source");
}

#[test]
fn unknown_columns_survive_in_raw() {
    let dir = tmp();
    let src = fixture(dir.path());
    let store = Store::open(dir.path().join("zerith.db")).unwrap();
    import(&src, "default", &store).unwrap();

    let id = store.list_sessions(1).unwrap()[0].id.clone();
    let msg = &store.messages(&id, 10, 0).unwrap()[0];
    let raw = msg.raw.as_ref().expect("raw is kept");
    assert!(raw.get("display_kind").is_some());
    assert!(raw.get("message_uid").is_some());
}

#[test]
fn a_missing_database_is_reported_not_panicked() {
    let dir = tmp();
    let store = Store::open(dir.path().join("zerith.db")).unwrap();
    let err = import(&dir.path().join("nope.db"), "default", &store).unwrap_err();
    assert!(matches!(err, ImportError::NotFound(_)));
}
