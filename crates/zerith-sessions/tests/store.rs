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
    assert_eq!(
        s.schema_version().unwrap(),
        zerith_sessions::migrations::CURRENT_VERSION
    );
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

/// The replay window counts user messages: tool rows ride free, synthetic
/// user rows do not count, and the newest summary is kept even when older
/// than the window.
#[test]
fn history_rows_count_user_messages_and_keep_the_summary() {
    let s = Store::open_in_memory().unwrap();
    let sid = s.create_session("longa", SessionSource::Native).unwrap().id;
    let put = |role, text: &str, raw: Option<serde_json::Value>| {
        s.append(
            &sid,
            &NewMessage {
                raw,
                ..NewMessage::new(role, text)
            },
        )
        .unwrap()
    };
    let first = put(Role::User, "primeira pergunta", None);
    put(Role::Assistant, "resposta 0", None);
    let summary = put(
        Role::User,
        "[resumo]",
        Some(
            serde_json::json!({"summary": {"start_id": first, "end_id": first + 1, "supersedes": []}}),
        ),
    );
    for i in 1..=3 {
        put(Role::User, &format!("pergunta {i}"), None);
        for _ in 0..30 {
            put(Role::Assistant, "chamada", None);
            put(Role::Tool, "resultado", None);
        }
        put(
            Role::User,
            "ctx",
            Some(serde_json::json!({"tool_context": true})),
        );
        put(Role::Assistant, &format!("resposta {i}"), None);
    }
    let texts = |w| -> Vec<String> {
        s.history_rows(&sid, w)
            .unwrap()
            .into_iter()
            .map(|m| m.content)
            .collect()
    };

    // Window 2: the summary, then from "pergunta 2" on, all 60 tool rows in.
    let two = texts(2);
    assert_eq!(two[0], "[resumo]");
    assert_eq!(two[1], "pergunta 2");
    assert_eq!(two.len(), 1 + 2 * (1 + 60 + 2));
    assert_eq!(two.last().unwrap(), "resposta 3");
    // Window 3 starts at "pergunta 1": "ctx" rows did not eat a slot.
    assert_eq!(texts(3)[1], "pergunta 1");
    // A window larger than the session: everything, the summary once.
    let all = texts(40);
    assert_eq!(all.len(), s.message_count(&sid).unwrap() as usize);
    assert_eq!(all.iter().filter(|t| *t == "[resumo]").count(), 1);
    assert_eq!(all[0], "primeira pergunta");
    // What the window shows of a long chat: the newest rows, oldest first.
    let shown: Vec<String> = s
        .recent_messages(&sid, 2)
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(shown, ["ctx", "resposta 3"]);
    // A superseded summary is not replayed.
    s.conn()
        .execute(
            "UPDATE messages SET superseded = 1 WHERE id = ?1",
            [summary],
        )
        .unwrap();
    assert_eq!(texts(1)[0], "pergunta 3");
}
