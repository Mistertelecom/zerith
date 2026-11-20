//! Checkpoints and rewind against real temp directories.

use std::path::{Path, PathBuf};

use zerith_sessions::checkpoints::{
    list, next_turn, purge_superseded, restore, Checkpoint, CheckpointError, Mode, KEEP_TURNS,
    MAX_FILE_BYTES,
};
use zerith_sessions::{NewMessage, Role, SessionSource, Store};

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    store: Store,
    session: String,
}

fn env() -> Env {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let root = dir.path().join("data");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session("t", SessionSource::Native).unwrap().id;
    Env {
        root,
        work,
        store,
        session,
        _dir: dir,
    }
}

fn say(e: &Env, text: &str) {
    e.store
        .append(&e.session, &NewMessage::new(Role::User, text))
        .unwrap();
}

#[test]
fn restore_code_brings_back_edits_and_removes_created_files() {
    let e = env();
    let (a, b) = (e.work.join("a.txt"), e.work.join("b.txt"));
    std::fs::write(&a, "original").unwrap();
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    cp.record_file(&a).unwrap();
    std::fs::write(&a, "editado").unwrap();
    // A second record in the same turn must not overwrite the first copy.
    cp.record_file(&a).unwrap();
    std::fs::write(&a, "editado de novo").unwrap();
    cp.record_file(&b).unwrap();
    std::fs::write(&b, "novo").unwrap();
    assert_eq!(cp.recorded(), 2);

    let r = restore(&e.store, &e.root, &e.session, 1, Mode::Code).unwrap();
    assert_eq!((r.files_restored, r.files_removed), (1, 1));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
    assert!(!b.exists());
    // Restoring twice is a no-op: the saved copies were consumed.
    std::fs::write(&a, "trabalho novo").unwrap();
    let again = restore(&e.store, &e.root, &e.session, 1, Mode::Code).unwrap();
    assert_eq!(again.files_restored, 0);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "trabalho novo");
}

#[test]
fn the_earliest_turn_wins_when_rewinding_across_turns() {
    let e = env();
    let a = e.work.join("a.txt");
    std::fs::write(&a, "v0").unwrap();
    let t1 = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    t1.record_file(&a).unwrap();
    std::fs::write(&a, "v1").unwrap();
    let t2 = Checkpoint::begin(&e.store, &e.root, &e.session, 2).unwrap();
    t2.record_file(&a).unwrap();
    std::fs::write(&a, "v2").unwrap();

    // Rewinding to turn 2 undoes only turn 2.
    restore(&e.store, &e.root, &e.session, 2, Mode::Code).unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "v1");
    // Turn 1 is still restorable (turn 2 is already consumed).
    restore(&e.store, &e.root, &e.session, 1, Mode::Code).unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "v0");
}

#[test]
fn conversation_rewind_is_soft_and_purge_is_explicit() {
    let e = env();
    say(&e, "antes");
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    say(&e, "pedido do turno 1");
    e.store
        .append(&e.session, &NewMessage::new(Role::Assistant, "resposta"))
        .unwrap();
    drop(cp);

    let r = restore(&e.store, &e.root, &e.session, 1, Mode::Conversation).unwrap();
    assert_eq!(r.messages_archived, 2);
    let live = e.store.messages(&e.session, 10, 0).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].content, "antes");
    // Still in the file, hidden from readers and from search.
    let raw: i64 = e
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, 3);
    assert!(e.store.search_messages("resposta", 5).unwrap().is_empty());
    // The checkpoint of the rewound turn is gone; the next turn reuses 1.
    assert_eq!(next_turn(&e.store, &e.session).unwrap(), 1);

    assert_eq!(purge_superseded(&e.store, &e.session).unwrap(), 2);
    let raw: i64 = e
        .store
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, 1);
}

#[test]
fn both_restores_files_and_messages() {
    let e = env();
    let a = e.work.join("a.txt");
    std::fs::write(&a, "antes").unwrap();
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    say(&e, "edite a.txt");
    cp.record_file(&a).unwrap();
    std::fs::write(&a, "depois").unwrap();
    let r = restore(&e.store, &e.root, &e.session, 1, Mode::Both).unwrap();
    assert_eq!((r.files_restored, r.messages_archived), (1, 1));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "antes");
    assert_eq!(e.store.message_count(&e.session).unwrap(), 0);
}

#[test]
fn caps_fail_loudly_and_do_not_touch_the_manifest() {
    let e = env();
    let big = e.work.join("big.bin");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    let err = cp.record_file(&big).unwrap_err();
    assert!(matches!(err, CheckpointError::TooLarge { .. }), "{err}");
    assert!(err.to_string().contains("a edição não foi feita"));
    assert_eq!(cp.recorded(), 0);
    // A directory is refused rather than silently skipped.
    assert!(matches!(
        cp.record_file(&e.work).unwrap_err(),
        CheckpointError::NotAFile(_)
    ));
}

#[test]
fn only_the_last_hundred_turns_are_kept() {
    let e = env();
    for t in 1..=(KEEP_TURNS + 5) {
        Checkpoint::begin(&e.store, &e.root, &e.session, t).unwrap();
    }
    let all = list(&e.store, &e.root, &e.session).unwrap();
    assert_eq!(all.len() as i64, KEEP_TURNS);
    assert_eq!(all[0].turn, 6);
    assert!(!turn_dir(&e.root, &e.session, 5).exists());
    assert!(turn_dir(&e.root, &e.session, 6).exists());
    assert_eq!(next_turn(&e.store, &e.session).unwrap(), KEEP_TURNS + 5 + 1);
}

#[test]
fn restoring_a_turn_that_never_existed_says_so() {
    let e = env();
    let err = restore(&e.store, &e.root, &e.session, 9, Mode::Both).unwrap_err();
    assert!(matches!(err, CheckpointError::NoCheckpoint { turn: 9, .. }));
}

#[test]
fn list_reports_file_counts() {
    let e = env();
    let a = e.work.join("a.txt");
    std::fs::write(&a, "x").unwrap();
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    cp.record_file(&a).unwrap();
    let rows = list(&e.store, &e.root, &e.session).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].files, 1);
    assert!(!rows[0].code_restored);
}

fn turn_dir(root: &Path, session: &str, turn: i64) -> PathBuf {
    root.join("checkpoints")
        .join(session)
        .join(turn.to_string())
}

#[test]
fn preview_lists_what_a_restore_would_touch_and_changes_nothing() {
    let e = env();
    let (a, b) = (e.work.join("a.txt"), e.work.join("b.txt"));
    std::fs::write(&a, "x").unwrap();
    let cp = Checkpoint::begin(&e.store, &e.root, &e.session, 1).unwrap();
    say(&e, "pedido");
    cp.record_file(&a).unwrap();
    cp.record_file(&b).unwrap();
    std::fs::write(&a, "y").unwrap();
    std::fs::write(&b, "novo").unwrap();

    let p = zerith_sessions::checkpoints::preview(&e.store, &e.root, &e.session, 1).unwrap();
    assert_eq!(p.messages_after, 1);
    assert_eq!(p.files.len(), 2);
    assert!(p
        .files
        .iter()
        .any(|(path, existed)| path.ends_with("b.txt") && !existed));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "y");
    assert!(b.exists());
    assert_eq!(e.store.message_count(&e.session).unwrap(), 1);
}

/// A clean folder under `target/test-scratch` (the OS temp dir has a
/// restricted ACL on some machines).
fn scratch(tag: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch")
        .join(format!("rewind-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn code_restore_undoes_what_the_shell_changed_in_a_folder_without_git() {
    let dir = scratch("shell");
    let (root, work) = (dir.join("data"), dir.join("work"));
    std::fs::create_dir_all(work.join("sub")).unwrap();
    std::fs::write(work.join("a.txt"), "a original").unwrap();
    std::fs::write(work.join("sub/b.txt"), "b original").unwrap();
    std::fs::write(work.join(".gitignore"), "build/\n").unwrap();
    let st = Store::open_in_memory().unwrap();
    let s = st.create_session("t", SessionSource::Native).unwrap().id;

    let cp = Checkpoint::begin(&st, &root, &s, 1).unwrap();
    cp.snapshot(&work);
    // What a shell command does: rm, sed -i, echo > new, a build.
    std::fs::remove_file(work.join("a.txt")).unwrap();
    std::fs::write(work.join("sub/b.txt"), "b editado").unwrap();
    std::fs::write(work.join("c.txt"), "novo").unwrap();
    std::fs::create_dir_all(work.join("build")).unwrap();
    std::fs::write(work.join("build/x"), "artefato").unwrap();
    // Later in the same turn a file tool recreates a.txt: its copy says
    // "did not exist", the snapshot must still win.
    cp.snapshot(&work);
    cp.record_file(&work.join("a.txt")).unwrap();
    std::fs::write(work.join("a.txt"), "a reescrito").unwrap();

    let p = zerith_sessions::checkpoints::preview(&st, &root, &s, 1).unwrap();
    let shown = |name: &str, existed: bool| {
        p.files
            .iter()
            .any(|(f, e)| f.replace('\\', "/").ends_with(name) && *e == existed)
    };
    assert!(shown("/c.txt", false), "{:?}", p.files);
    assert!(shown("sub/b.txt", true), "{:?}", p.files);
    assert!(shown("/a.txt", true), "the snapshot wins: {:?}", p.files);
    assert!(!p.files.iter().any(|(f, _)| f.contains("build")));
    assert!(p.notes.is_empty(), "{:?}", p.notes);

    let r = restore(&st, &root, &s, 1, Mode::Code).unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(read(&work.join("a.txt")), "a original");
    assert_eq!(read(&work.join("sub/b.txt")), "b original");
    assert!(!work.join("c.txt").exists());
    assert_eq!(
        read(&work.join("build/x")),
        "artefato",
        "ignored: left alone"
    );
    assert!(
        r.notes.iter().any(|n| n.contains("checkout")),
        "{:?}",
        r.notes
    );
    // Consumed like the copies: a second restore changes nothing.
    std::fs::write(work.join("c.txt"), "trabalho novo").unwrap();
    restore(&st, &root, &s, 1, Mode::Code).unwrap();
    assert_eq!(read(&work.join("c.txt")), "trabalho novo");
}

#[test]
fn the_users_repository_is_untouched_and_the_earliest_turn_wins() {
    let dir = scratch("repo");
    let (root, work) = (dir.join("data"), dir.join("work"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.txt"), "v0").unwrap();
    git(&work, &["init", "-q"]);
    git(&work, &["add", "-A"]);
    let index = std::fs::read(work.join(".git/index")).unwrap();
    let st = Store::open_in_memory().unwrap();
    let s = st.create_session("t", SessionSource::Native).unwrap().id;

    let t1 = Checkpoint::begin(&st, &root, &s, 1).unwrap();
    t1.snapshot(&work);
    std::fs::write(work.join("a.txt"), "v1").unwrap();
    let t2 = Checkpoint::begin(&st, &root, &s, 2).unwrap();
    t2.snapshot(&work);
    std::fs::write(work.join("a.txt"), "v2").unwrap();
    std::fs::write(work.join("novo.txt"), "t2").unwrap();

    restore(&st, &root, &s, 2, Mode::Code).unwrap();
    assert_eq!(read(&work.join("a.txt")), "v1");
    assert!(!work.join("novo.txt").exists());
    restore(&st, &root, &s, 1, Mode::Code).unwrap();
    assert_eq!(read(&work.join("a.txt")), "v0");
    assert_eq!(std::fs::read(work.join(".git/index")).unwrap(), index);
}

#[test]
fn the_home_folder_is_never_snapshotted_and_restore_says_so() {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return;
    };
    let dir = scratch("home");
    let st = Store::open_in_memory().unwrap();
    let s = st.create_session("t", SessionSource::Native).unwrap().id;
    let cp = Checkpoint::begin(&st, &dir, &s, 1).unwrap();
    cp.snapshot(Path::new(&home));
    let failed = turn_dir(&dir, &s, 1).join("shadow.failed");
    assert!(read(&failed).contains("pasta pessoal"));
    assert!(!dir.join("checkpoints/.shadow").exists(), "git never ran");
    let r = restore(&st, &dir, &s, 1, Mode::Code).unwrap();
    assert!(
        r.notes.iter().any(|n| n.contains("sem snapshot")),
        "{:?}",
        r.notes
    );
}
