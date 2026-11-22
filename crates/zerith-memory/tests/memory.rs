use zerith_memory::{Confidence, FactKind, MemoryBackend, MemoryError, Scope, SqliteMemory};

fn mem() -> SqliteMemory {
    SqliteMemory::open_in_memory().unwrap()
}

#[test]
fn recall_finds_an_accented_word_written_without_accents() {
    let m = mem();
    let s = Scope::user("joao");
    m.remember(
        &s,
        FactKind::Preference,
        "gosta de cafe sem acucar",
        Confidence::Confirmed,
        None,
    )
    .unwrap();

    let hits = m.recall(&s, "acucar", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].fact.kind, FactKind::Preference);
}

#[test]
fn the_same_sentence_twice_is_one_memory() {
    let m = mem();
    let s = Scope::user("joao");
    let a = m
        .remember(
            &s,
            FactKind::Fact,
            "mora em Sao Paulo",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    let b = m
        .remember(
            &s,
            FactKind::Fact,
            "mora em Sao Paulo",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(m.list(&s, 10).unwrap().len(), 1);
}

#[test]
fn scopes_do_not_leak_into_each_other() {
    let m = mem();
    let joao = Scope::user("joao");
    let maria = Scope::user("maria");
    m.remember(
        &joao,
        FactKind::Fact,
        "prefere rust",
        Confidence::Confirmed,
        None,
    )
    .unwrap();

    assert!(m.recall(&maria, "rust", 10).unwrap().is_empty());
    assert_eq!(m.recall(&joao, "rust", 10).unwrap().len(), 1);
}

#[test]
fn inferred_is_stored_and_read_back_as_inferred() {
    let m = mem();
    let s = Scope::project("zerith");
    let id = m
        .remember(
            &s,
            FactKind::Fact,
            "usa SQLite",
            Confidence::Inferred,
            Some("agent"),
        )
        .unwrap();
    assert_eq!(m.get(id).unwrap().confidence, Confidence::Inferred);
}

#[test]
fn forget_removes_text_and_keeps_it_forgotten_after_reinsert() {
    let m = mem();
    let s = Scope::user("joao");
    let id = m
        .remember(
            &s,
            FactKind::Fact,
            "endereco antigo",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    m.forget(id).unwrap();

    assert!(m.recall(&s, "endereco", 10).unwrap().is_empty());
    assert!(m.list(&s, 10).unwrap().is_empty());
    let stored = m.get(id).unwrap();
    assert_eq!(stored.text, "");

    // Reinserting the same text after a forget creates a new live row.
    let new_id = m
        .remember(
            &s,
            FactKind::Fact,
            "endereco antigo",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    assert_ne!(id, new_id);
    assert_eq!(m.list(&s, 10).unwrap().len(), 1);
}

#[test]
fn supersede_keeps_history_and_marks_the_old_fact() {
    let m = mem();
    let s = Scope::user("joao");
    let old = m
        .remember(
            &s,
            FactKind::Preference,
            "usa vim",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    let new = m
        .remember(
            &s,
            FactKind::Preference,
            "usa neovim",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    m.supersede(old, new).unwrap();

    let rows = m.export(&s).unwrap();
    assert_eq!(rows.len(), 2);
    let old_row = rows.iter().find(|f| f.id == old).unwrap();
    assert_eq!(old_row.superseded_by, Some(new));
}

#[test]
fn a_query_with_fts_syntax_characters_returns_empty_instead_of_failing() {
    let m = mem();
    let s = Scope::user("joao");
    m.remember(
        &s,
        FactKind::Fact,
        "testa parser",
        Confidence::Confirmed,
        None,
    )
    .unwrap();
    assert!(m.recall(&s, "\"*:()", 10).unwrap().is_empty());
}

#[test]
fn memory_survives_reopening_the_file() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("zerith.db");
    let s = Scope::user("joao");
    {
        let m = SqliteMemory::open(&db).unwrap();
        m.remember(
            &s,
            FactKind::Decision,
            "adotamos rune para a UI",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    }
    let m = SqliteMemory::open(&db).unwrap();
    assert_eq!(m.recall(&s, "rune", 10).unwrap().len(), 1);

    // Close before the tempdir is removed: Windows refuses to unlink a file a
    // live SQLite handle still holds.
    drop(m);
}

#[test]
fn default_scope_uses_zerith_user_else_local() {
    // Single test touches the env var, so no race with other tests.
    std::env::remove_var("ZERITH_USER");
    assert_eq!(zerith_memory::default_scope(), Scope::user("local"));
    std::env::set_var("ZERITH_USER", "ana");
    assert_eq!(zerith_memory::default_scope(), Scope::user("ana"));
    std::env::remove_var("ZERITH_USER");
}

#[test]
fn recall_multi_merges_user_and_project_scopes() {
    let m = mem();
    let u = Scope::user("joao");
    let p = Scope::project("zerith");
    let other = Scope::project("outro");
    m.remember(
        &u,
        FactKind::Preference,
        "prefiro deploy pequeno",
        Confidence::Confirmed,
        None,
    )
    .unwrap();
    m.remember(
        &p,
        FactKind::Decision,
        "deploy via script",
        Confidence::Confirmed,
        None,
    )
    .unwrap();
    m.remember(
        &other,
        FactKind::Fact,
        "deploy secreto",
        Confidence::Confirmed,
        None,
    )
    .unwrap();
    let hits = m.recall_multi(&[u, p], "deploy", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.windows(2).all(|w| w[0].score <= w[1].score));
    assert!(hits.iter().all(|h| h.fact.scope != "project:outro"));
    assert_eq!(
        m.recall_multi(&[Scope::user("joao")], "deploy", 1)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pause_and_session_exclusion_persist() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("m.db");
    {
        let m = SqliteMemory::open(&db).unwrap();
        assert!(m.is_extraction_allowed("s1").unwrap());
        m.exclude_session("s1").unwrap();
        assert!(!m.is_extraction_allowed("s1").unwrap());
        assert!(m.is_extraction_allowed("s2").unwrap());
        m.set_paused(true).unwrap();
    }
    let m = SqliteMemory::open(&db).unwrap();
    assert!(m.is_paused().unwrap());
    assert!(!m.is_extraction_allowed("s2").unwrap());
    assert_eq!(
        zerith_memory::extract_allowed(&m, "s2", "lembre que x"),
        None
    );
    m.set_paused(false).unwrap();
    assert!(zerith_memory::extract_allowed(&m, "s2", "lembre que x").is_some());
    assert_eq!(
        zerith_memory::extract_allowed(&m, "s1", "lembre que x"),
        None
    );
    m.include_session("s1").unwrap();
    assert!(m.is_extraction_allowed("s1").unwrap());
}

#[test]
fn facts_round_trip_through_jsonl() {
    let a = mem();
    let u = Scope::user("joao");
    a.remember(
        &u,
        FactKind::Instruction,
        "sempre testes",
        Confidence::Confirmed,
        Some("manual"),
    )
    .unwrap();
    a.remember(
        &Scope::project("z"),
        FactKind::Fact,
        "usa sqlite",
        Confidence::Inferred,
        None,
    )
    .unwrap();
    let mut buf = Vec::new();
    assert_eq!(a.export_jsonl(None, &mut buf).unwrap(), 2);

    let b = mem();
    assert_eq!(b.import_jsonl(&buf[..]).unwrap(), 2);
    // Importing again dedupes instead of duplicating.
    b.import_jsonl(&buf[..]).unwrap();
    let mine = b.export(&u).unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, FactKind::Instruction);
    assert_eq!(mine[0].source.as_deref(), Some("manual"));
    let proj = b.export(&Scope::project("z")).unwrap();
    assert_eq!(proj[0].confidence, Confidence::Inferred);

    let mut one = Vec::new();
    assert_eq!(a.export_jsonl(Some(&u), &mut one).unwrap(), 1);
    assert!(b.import_jsonl(&b"{bad"[..]).is_err());
}

#[test]
fn work_facts_stay_in_their_project_and_preferences_follow_the_user() {
    let m = mem();
    let base =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    let (a, b) = (base.join("cliente-a"), base.join("cliente-b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (sa, sb) = (Scope::workspace(&a), Scope::workspace(&b));
    let c = Confidence::Confirmed;
    m.remember(&sa, FactKind::Project, "roteador core em 10.0.0.1", c, None)
        .unwrap();
    m.remember(&sa, FactKind::Preference, "responder em portugues", c, None)
        .unwrap();
    // A fact saved before projects existed (plain user scope).
    m.remember(
        &zerith_memory::default_scope(),
        FactKind::Fact,
        "roteador antigo legado",
        c,
        None,
    )
    .unwrap();

    let texts = |s: &Scope| -> Vec<String> {
        m.recall(s, "roteador portugues", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.fact.text)
            .collect()
    };
    let in_a = texts(&sa);
    let in_b = texts(&sb);
    assert!(in_a.iter().any(|t| t.contains("10.0.0.1")), "{in_a:?}");
    assert!(
        !in_b.iter().any(|t| t.contains("10.0.0.1")),
        "leaked to B: {in_b:?}"
    );
    assert!(
        in_b.iter().any(|t| t.contains("portugues")),
        "preference follows the user: {in_b:?}"
    );
    assert!(
        in_b.iter().any(|t| t.contains("legado")),
        "old facts stay visible: {in_b:?}"
    );
    assert_eq!(
        m.list(&Scope::all(), 10).unwrap().len(),
        3,
        "the memory screen sees all"
    );
    assert!(m
        .remember(&Scope::all(), FactKind::Fact, "x", c, None)
        .is_err());
}

#[test]
fn a_password_is_never_remembered() {
    let m = mem();
    let s = Scope::user("joao");
    let c = Confidence::Confirmed;
    assert!(m
        .remember(
            &s,
            FactKind::Fact,
            "A senha do roteador do cliente Alfa é Admin@123",
            c,
            None
        )
        .is_err());
    assert!(m
        .remember(&s, FactKind::Fact, "community SNMP AlphanNet-86", c, None)
        .is_err());
    assert!(m
        .remember(
            &s,
            FactKind::Fact,
            "OLT do cliente Alfa em 10.27.30.2",
            c,
            None
        )
        .is_ok());
}

#[test]
fn update_rejects_empty_and_secrets_and_recall_list_skip_superseded() {
    let m = mem();
    let s = Scope::user("joao");
    let c = Confidence::Confirmed;
    let id1 = m
        .remember(&s, FactKind::Fact, "roteador antigo em 10.0.0.1", c, None)
        .unwrap();
    let id2 = m
        .remember(&s, FactKind::Fact, "roteador novo em 10.0.0.2", c, None)
        .unwrap();

    assert!(matches!(m.update(id2, "   "), Err(MemoryError::Empty)));
    assert!(matches!(
        m.update(id2, "a senha do roteador é Admin@123"),
        Err(MemoryError::Secret)
    ));
    assert_eq!(m.get(id2).unwrap().text, "roteador novo em 10.0.0.2");

    m.supersede(id1, id2).unwrap();
    let listed = m.list(&s, 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id2);

    let hits = m.recall(&s, "roteador", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].fact.id, id2);
}
