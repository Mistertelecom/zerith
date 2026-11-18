use zerith_sessions::{NewMessage, Role, Session, SessionSource, Store};

#[test]
fn schema_is_current_and_integrity_holds() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(
        s.schema_version().unwrap(),
        zerith_sessions::migrations::CURRENT_VERSION
    );
    assert!(s.integrity_ok().unwrap());
}

#[test]
fn messages_survive_reopening_the_file() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("zerith.db");
    let id = {
        let s = Store::open(&db).unwrap();
        let sess = s.create_session("primeira", SessionSource::Native).unwrap();
        s.append(&sess.id, &NewMessage::new(Role::User, "oi"))
            .unwrap();
        s.append(&sess.id, &NewMessage::new(Role::Assistant, "ola"))
            .unwrap();
        sess.id
    };
    let s = Store::open(&db).unwrap();
    let msgs = s.messages(&id, 50, 0).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].content, "oi");
    assert_eq!(msgs[1].role, Role::Assistant);
    assert_eq!(s.message_count(&id).unwrap(), 2);
}

#[test]
fn reimporting_the_same_source_row_does_not_duplicate() {
    let s = Store::open_in_memory().unwrap();
    // Importers build the row themselves: a session that never existed here
    // has no local id yet.
    let sess = Session {
        id: "hermes-row-1".into(),
        title: "x".into(),
        source: SessionSource::Hermes,
        source_id: Some("hermes-abc".into()),
        source_profile: Some("default".into()),
        model: None,
        provider: None,
        created_at: 1,
        updated_at: 1,
        archived: false,
    };

    let (id1, created1) = s.upsert_imported(&sess).unwrap();
    let (id2, created2) = s.upsert_imported(&sess).unwrap();

    assert!(created1);
    assert!(!created2);
    assert_eq!(id1, id2);
    assert_eq!(s.list_sessions(10).unwrap().len(), 1);
}

#[test]
fn backup_produces_a_readable_copy() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let s = Store::open(dir.path().join("a.db")).unwrap();
    let sess = s.create_session("backup", SessionSource::Native).unwrap();
    s.append(&sess.id, &NewMessage::new(Role::User, "persistir"))
        .unwrap();

    let copy = dir.path().join("b.db");
    s.backup_to(&copy).unwrap();

    let restored = Store::open(&copy).unwrap();
    assert_eq!(restored.message_count(&sess.id).unwrap(), 1);
}

/// v8: each session keeps its own folder; the newest folders come first and a
/// file migrated from v7 (no column yet) gains it with every row NULL.
#[test]
fn a_session_remembers_its_folder() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("zerith.db");
    {
        // A v7 file: everything but the v8 migration applied.
        let s = Store::open(&db).unwrap();
        s.conn()
            .execute_batch(
                "ALTER TABLE sessions DROP COLUMN workspace; DELETE FROM schema_migrations WHERE version = 8;",
            )
            .unwrap();
        s.create_session("antiga", SessionSource::Native).unwrap();
    }
    let s = Store::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), 8);
    let old = s.list_sessions(10).unwrap().remove(0);
    assert_eq!(s.workspace(&old.id).unwrap(), None);

    let a = s.create_session("a", SessionSource::Native).unwrap();
    s.set_workspace(&a.id, "C:/proj/a").unwrap();
    s.set_workspace(&old.id, "C:/proj/b").unwrap();
    assert_eq!(s.workspace(&a.id).unwrap().as_deref(), Some("C:/proj/a"));
    s.append(&a.id, &NewMessage::new(Role::User, "oi")).unwrap();
    assert_eq!(
        s.recent_workspaces(10).unwrap(),
        vec!["C:/proj/a", "C:/proj/b"]
    );
    assert_eq!(s.workspaces().unwrap().len(), 2);
    assert!(s.set_workspace("nope", "C:/x").is_err());
}
