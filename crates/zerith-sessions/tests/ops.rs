use zerith_sessions::{NewMessage, Role, SessionFilter, SessionSource, Store};

fn seeded() -> (Store, String, Vec<i64>) {
    let s = Store::open_in_memory().unwrap();
    let sess = s
        .create_session("Deploy de sexta", SessionSource::Native)
        .unwrap();
    let ids = [
        "como faço o deploy?",
        "use o script de publicação",
        "obrigado",
    ]
    .iter()
    .enumerate()
    .map(|(i, t)| {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        s.append(&sess.id, &NewMessage::new(role, *t)).unwrap()
    })
    .collect();
    (s, sess.id, ids)
}

#[test]
fn search_finds_message_content_without_accents() {
    let (s, id, ids) = seeded();
    let hits = s.search_messages("publicacao", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, id);
    assert_eq!(hits[0].message_id, ids[1]);
    assert!(
        hits[0].snippet.contains("[publicação]"),
        "{}",
        hits[0].snippet
    );
    // FTS syntax in user input is not an error.
    assert!(s.search_messages("\"a:b", 10).is_ok());
    assert!(s.search_messages("  ", 10).unwrap().is_empty());
}

#[test]
fn search_sessions_matches_title_or_content() {
    let (s, id, _) = seeded();
    s.create_session("outra", SessionSource::Native).unwrap();
    assert_eq!(s.search_sessions("sexta", 10).unwrap()[0].id, id);
    assert_eq!(s.search_sessions("script", 10).unwrap()[0].id, id);
    assert!(s.search_sessions("inexistente", 10).unwrap().is_empty());
    // A literal `%` does not match everything.
    assert!(s.search_sessions("%", 10).unwrap().is_empty());
}

#[test]
fn migration_v3_backfills_and_is_idempotent() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("z.db");
    let id = {
        let s = Store::open(&db).unwrap();
        let sess = s.create_session("x", SessionSource::Native).unwrap();
        // Simulate a v2 database: no index, rows written without triggers.
        s.conn()
            .execute_batch(
                "DROP TRIGGER messages_ai; DROP TRIGGER messages_ad; DROP TRIGGER messages_au;
                 DROP TABLE messages_fts; DROP TABLE settings; DROP TABLE events;
                 DELETE FROM schema_migrations WHERE version IN (3, 4);",
            )
            .unwrap();
        s.append(&sess.id, &NewMessage::new(Role::User, "girafa antiga"))
            .unwrap();
        sess.id
    };
    let s = Store::open(&db).unwrap();
    assert_eq!(
        s.schema_version().unwrap(),
        zerith_sessions::migrations::CURRENT_VERSION
    );
    assert_eq!(s.search_messages("girafa", 5).unwrap()[0].session_id, id);
    drop(s);
    // Reopening runs nothing twice.
    let s = Store::open(&db).unwrap();
    assert_eq!(s.search_messages("girafa", 5).unwrap().len(), 1);
}

#[test]
fn fork_copies_messages_up_to_a_point() {
    let (s, id, ids) = seeded();
    s.set_session_model(&id, Some("m1"), Some("ollama"))
        .unwrap();
    let f = s.fork_session(&id, Some(ids[1]), "ramo").unwrap();
    assert_ne!(f.id, id);
    assert_eq!(f.model.as_deref(), Some("m1"));
    let msgs = s.messages(&f.id, 10, 0).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].content, "use o script de publicação");
    assert_eq!(s.message_count(&id).unwrap(), 3);
    assert_eq!(s.fork_session(&id, None, "tudo").unwrap().title, "tudo");
    assert!(s.fork_session("nope", None, "x").is_err());
}

#[test]
fn fork_keeps_the_folder_and_leaves_the_original_alone() {
    let (s, id, ids) = seeded();
    s.set_workspace(&id, "C:/proj").unwrap();
    let f = s.fork_session(&id, Some(ids[0]), "editada").unwrap();
    assert_eq!(s.workspace(&f.id).unwrap().as_deref(), Some("C:/proj"));
    let msgs = s.messages(&f.id, 10, 0).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, s.messages(&id, 10, 0).unwrap()[0].content);
    assert_eq!(s.message_count(&id).unwrap(), 3);
    assert_eq!(s.workspace(&id).unwrap().as_deref(), Some("C:/proj"));
}

#[test]
fn export_jsonl_and_markdown() {
    let (s, id, _) = seeded();
    let mut buf = Vec::new();
    s.export_session_jsonl(&id, &mut buf).unwrap();
    let lines: Vec<serde_json::Value> = String::from_utf8(buf)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[0]["title"], "Deploy de sexta");
    assert_eq!(lines[2]["type"], "message");
    assert_eq!(lines[2]["role"], "assistant");

    let mut md = Vec::new();
    s.export_session_markdown(&id, &mut md).unwrap();
    let md = String::from_utf8(md).unwrap();
    assert!(md.starts_with("# Deploy de sexta"));
    assert!(md.contains("## user\n\ncomo faço o deploy?"));
}

#[test]
fn archive_and_filters() {
    let (s, id, _) = seeded();
    let other = s.create_session("b", SessionSource::Native).unwrap();
    s.set_archived(&id, true).unwrap();
    // `list_sessions` keeps listing everything, as before.
    assert_eq!(s.list_sessions(10).unwrap().len(), 2);
    let live = s
        .list_sessions_filtered(&SessionFilter {
            archived: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, other.id);
    let hermes = s
        .list_sessions_filtered(&SessionFilter {
            source: Some(SessionSource::Hermes),
            ..Default::default()
        })
        .unwrap();
    assert!(hermes.is_empty());
    let sub = s
        .create_session("subagente: x", SessionSource::Subagent)
        .unwrap();
    let sidebar = s
        .list_sessions_filtered(&SessionFilter {
            archived: Some(false),
            exclude_source: Some(SessionSource::Subagent),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(sidebar.len(), 1, "no archived, no subagent");
    assert_eq!(sidebar[0].id, other.id);
    assert!(s.session(&sub.id).unwrap().is_some());
    let q = s
        .list_sessions_filtered(&SessionFilter {
            archived: Some(true),
            query: Some("deploy".into()),
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(q[0].id, id);
    assert!(s.session(&id).unwrap().unwrap().archived);
    assert!(s.set_archived("nope", true).is_err());
}

#[test]
fn settings_round_trip() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.setting("k").unwrap(), None);
    s.set_setting("k", "1").unwrap();
    s.set_setting("k", "2").unwrap();
    assert_eq!(s.setting("k").unwrap().as_deref(), Some("2"));
    s.delete_setting("k").unwrap();
    assert_eq!(s.setting("k").unwrap(), None);
}

#[test]
fn sessions_resolve_by_id_title_or_unique_prefix_and_rename_keeps_order() {
    use zerith_sessions::names::ResolveError;
    let s = Store::open_in_memory().unwrap();
    let a = s
        .create_session("Refatorar API", SessionSource::Native)
        .unwrap();
    let b = s
        .create_session("refatorar api", SessionSource::Native)
        .unwrap();
    let c = s.create_session("outra", SessionSource::Native).unwrap();
    assert_eq!(s.resolve_session(&a.id).unwrap().id, a.id);
    assert_eq!(s.resolve_session("OUTRA").unwrap().id, c.id);
    assert_eq!(s.resolve_session(&c.id[..8]).unwrap().id, c.id);
    // Two sessions share a title: say so, do not pick one.
    match s.resolve_session("refatorar api") {
        Err(ResolveError::Ambiguous { ids, .. }) => assert_eq!(ids.len(), 2),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        s.resolve_session("zzzz"),
        Err(ResolveError::NotFound(_))
    ));
    // `%` is data, not a wildcard.
    assert!(matches!(
        s.resolve_session("%%%%"),
        Err(ResolveError::NotFound(_))
    ));

    let before = s.session(&b.id).unwrap().unwrap().updated_at;
    s.rename_session(&b.id, "  novo nome ").unwrap();
    let after = s.session(&b.id).unwrap().unwrap();
    assert_eq!(after.title, "novo nome");
    assert_eq!(after.updated_at, before);
    assert!(s.rename_session(&b.id, "   ").is_err());
    assert!(s.rename_session("nao-existe", "x").is_err());
    assert!(s.latest_session().unwrap().is_some());
}

#[test]
fn a_rewound_message_does_not_match_the_session_list_search() {
    let (s, id, ids) = seeded();
    s.conn()
        .execute("UPDATE messages SET superseded = 1 WHERE id = ?1", [ids[1]])
        .unwrap();
    let q = |text: &str| {
        s.list_sessions_filtered(&SessionFilter {
            query: Some(text.into()),
            ..Default::default()
        })
        .unwrap()
    };
    assert!(q("publicacao").is_empty(), "superseded row must not match");
    assert_eq!(q("obrigado")[0].id, id);
}
