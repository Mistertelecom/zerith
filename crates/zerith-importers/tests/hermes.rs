//! Importer behavior against a miniature Hermes database.
//!
//! These live here, not inline, because the fixture needs
//! `CARGO_TARGET_TMPDIR`, which Cargo only sets for integration tests.

use std::path::{Path, PathBuf};
use zerith_importers::{
    import, import_selected, import_with, preview, ImportError, SessionOptions,
};
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

#[test]
fn an_interrupted_import_resumes_per_message() {
    let dir = tmp();
    let src = fixture(dir.path());
    let store = Store::open(dir.path().join("zerith.db")).unwrap();
    import(&src, "default", &store).unwrap();
    let id = store.list_sessions(1).unwrap()[0].id.clone();
    // Simulate a crash after the first message was committed.
    store
        .conn()
        .execute(
            "DELETE FROM messages WHERE id = (SELECT MAX(id) FROM messages WHERE session_id = ?1)",
            [&id],
        )
        .unwrap();
    // And Hermes kept talking in the meantime.
    let conn = rusqlite::Connection::open(&src).unwrap();
    conn.execute(
        "INSERT INTO messages(session_id, role, content, active) VALUES ('h1', 'user', 'de novo', 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let r = import(&src, "default", &store).unwrap();
    assert_eq!((r.sessions_resumed, r.messages_imported), (1, 2));
    let msgs = store.messages(&id, 10, 0).unwrap();
    let texts: Vec<_> = msgs.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(texts, ["oi", "olá", "de novo"]);
    let again = import(&src, "default", &store).unwrap();
    assert_eq!((again.sessions_skipped, again.messages_imported), (1, 0));
}

fn fixture_multi(dir: &Path) -> PathBuf {
    let path = dir.join("multi.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, model TEXT, started_at REAL, ended_at REAL,
            title TEXT, profile_name TEXT, archived INTEGER
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, role TEXT,
            content TEXT, timestamp REAL, active INTEGER
         );
         INSERT INTO sessions(id, model, started_at, ended_at, title, profile_name, archived)
            VALUES ('h1', 'hermes-3', 1700000000.0, 1700000050.0, 'Ativa um', 'default', 0),
                   ('h2', 'hermes-4', 1700000100.0, 1700000200.0, 'Ativa dois', 'default', 0),
                   ('h3', NULL,       1700000300.0, 1700000400.0, 'Arquivada', 'default', 1);
         INSERT INTO messages(session_id, role, content, timestamp, active)
            VALUES ('h1', 'user', 'oi', 1700000001.0, 1),
                   ('h1', 'assistant', 'olá', 1700000002.0, 1),
                   ('h1', 'assistant', 'apagada', 1700000003.0, 0),
                   ('h2', 'user', 'segunda', 1700000150.0, 1),
                   ('h3', 'user', 'antiga 1', 1700000350.0, 1),
                   ('h3', 'assistant', 'antiga 2', 1700000360.0, 1);",
    )
    .unwrap();
    drop(conn);
    path
}

#[test]
fn preview_counts_active_messages_dates_and_is_read_only() {
    let dir = tmp();
    let src = fixture_multi(dir.path());
    let before = std::fs::read(&src).unwrap();

    let p = preview(&src).unwrap();
    assert!(p.layout.sessions_table && p.layout.messages_table);
    assert_eq!(p.sessions.len(), 3);
    assert_eq!(p.total_messages, 5);
    assert_eq!(p.min_date, Some(1_700_000_000_000));
    assert_eq!(p.max_date, Some(1_700_000_400_000));

    assert_eq!(p.sessions[0].id, "h1");
    assert_eq!(p.sessions[0].message_count, 2);
    assert!(!p.sessions[0].archived);
    assert_eq!(p.sessions[0].model.as_deref(), Some("hermes-3"));

    assert_eq!(p.sessions[1].id, "h2");
    assert_eq!(p.sessions[1].message_count, 1);

    assert_eq!(p.sessions[2].id, "h3");
    assert_eq!(p.sessions[2].message_count, 2);
    assert!(p.sessions[2].archived);

    let after = std::fs::read(&src).unwrap();
    assert_eq!(before, after, "preview must not write to the source");

    let empty_db = dir.path().join("empty.db");
    let conn = rusqlite::Connection::open(&empty_db).unwrap();
    conn.execute_batch("CREATE TABLE other (x INTEGER);")
        .unwrap();
    drop(conn);
    assert!(matches!(preview(&empty_db), Err(ImportError::Schema(_))));
}

#[test]
fn import_selected_filters_archived_and_only_ids() {
    let dir = tmp();
    let src = fixture_multi(dir.path());

    // Excluding archived skips h3 without counting it as imported or skipped.
    let store_no_arch = Store::open(dir.path().join("no_arch.db")).unwrap();
    let r = import_selected(&src, "default", &store_no_arch, false, None).unwrap();
    assert_eq!(r.sessions_seen, 3);
    assert_eq!(r.sessions_imported, 2);
    assert_eq!(r.sessions_skipped, 0);
    assert_eq!(r.messages_imported, 3);
    assert!(r.failed.is_empty());
    assert!(store_no_arch
        .session("hermes-default-h1")
        .unwrap()
        .is_some());
    assert!(store_no_arch
        .session("hermes-default-h2")
        .unwrap()
        .is_some());
    assert!(store_no_arch
        .session("hermes-default-h3")
        .unwrap()
        .is_none());

    // Explicit only_ids imports just the picked sessions (including archived when allowed).
    let store_picked = Store::open(dir.path().join("picked.db")).unwrap();
    let ids = vec!["h2".to_string(), "h3".to_string()];
    let r = import_selected(&src, "default", &store_picked, true, Some(&ids)).unwrap();
    assert_eq!(r.sessions_imported, 2);
    assert_eq!(r.sessions_skipped, 0);
    assert_eq!(r.messages_imported, 3);
    assert!(store_picked.session("hermes-default-h1").unwrap().is_none());
    assert!(store_picked.session("hermes-default-h2").unwrap().is_some());
    assert!(
        store_picked
            .session("hermes-default-h3")
            .unwrap()
            .unwrap()
            .archived
    );

    // Combining include_archived = false with only_ids still excludes archived h3.
    let store_both = Store::open(dir.path().join("both.db")).unwrap();
    let ids = vec!["h1".to_string(), "h3".to_string()];
    let r = import_selected(&src, "default", &store_both, false, Some(&ids)).unwrap();
    assert_eq!(r.sessions_imported, 1);
    assert_eq!(r.messages_imported, 2);
    assert!(store_both.session("hermes-default-h1").unwrap().is_some());
    assert!(store_both.session("hermes-default-h3").unwrap().is_none());
}

#[test]
fn pinned_and_archived_survive_and_only_pinned_visible_archives_the_rest() {
    let dir = tmp();
    let src = fixture(dir.path());
    let conn = rusqlite::Connection::open(&src).unwrap();
    conn.execute_batch(
        "INSERT INTO sessions(id, started_at, title, archived, pinned)
            VALUES ('h2', 1700000100.0, 'Fixada', 0, 1),
                   ('h3', 1700000200.0, 'Arquivada', 1, 0);",
    )
    .unwrap();
    drop(conn);

    let p = preview(&src).unwrap();
    assert!(p.sessions.iter().any(|s| s.id == "h2" && s.pinned));

    let store = Store::open(dir.path().join("a.db")).unwrap();
    import_with(&src, "default", &store, &SessionOptions::default()).unwrap();
    let archived = |st: &Store, id: &str| st.session(id).unwrap().unwrap().archived;
    assert_eq!(store.pinned_sessions().unwrap(), ["hermes-default-h2"]);
    assert!(archived(&store, "hermes-default-h3"));
    assert!(!archived(&store, "hermes-default-h1"));
    // A pessoa desafixa no Zerith; a segunda rodada não refixa.
    store
        .set_session_pinned("hermes-default-h2", false)
        .unwrap();
    import_with(&src, "default", &store, &SessionOptions::default()).unwrap();
    assert!(store.pinned_sessions().unwrap().is_empty());

    let store = Store::open(dir.path().join("b.db")).unwrap();
    let opts = SessionOptions {
        only_pinned_visible: true,
        ..Default::default()
    };
    import_with(&src, "default", &store, &opts).unwrap();
    assert!(!archived(&store, "hermes-default-h2"));
    assert!(archived(&store, "hermes-default-h1"));
    assert!(archived(&store, "hermes-default-h3"));
}
