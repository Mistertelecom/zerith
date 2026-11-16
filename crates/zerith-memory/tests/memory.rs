use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};

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
            "senha antiga",
            Confidence::Confirmed,
            None,
        )
        .unwrap();
    m.forget(id).unwrap();

    assert!(m.recall(&s, "senha", 10).unwrap().is_empty());
    assert!(m.list(&s, 10).unwrap().is_empty());
    let stored = m.get(id).unwrap();
    assert_eq!(stored.text, "");

    // Reinserting the same text after a forget creates a new live row.
    let new_id = m
        .remember(
            &s,
            FactKind::Fact,
            "senha antiga",
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
