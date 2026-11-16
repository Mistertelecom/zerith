//! The shell's own logic, without a GPU: `App::new` builds the state container
//! but no window, so these run beside the rest of the workspace.

use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_sessions::{NewMessage, Role, SessionSource, Store};
use zerith_ui::{
    day_bucket, filter_rows, group_by_day, on_key, spec, Approval, Bucket, Decision, Load, Row,
    Screen, Shell, SourceFilter, Theme,
};

/// A shell over a database in `home`, with worker jobs run inline so each
/// transition has landed by the next line.
fn shell_at(home: &std::path::Path) -> Shell {
    let app = rune::App::new(spec());
    let mut s = Shell::open(home.join("zerith.db"), app.handle());
    s.inline = true;
    s
}

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir")
}

fn row(id: &str, title: &str, source: &str, at: i64, archived: bool) -> Row {
    Row {
        id: id.into(),
        title: title.into(),
        source: source.into(),
        updated_at: at,
        archived,
    }
}

fn approval(tool: &str) -> Approval {
    Approval {
        id: format!("a-{tool}"),
        title: "Executar".into(),
        tool: tool.into(),
        summary: "ls".into(),
    }
}

#[test]
fn an_empty_database_opens_on_chat_with_nothing_loaded() {
    let dir = tempdir();
    let shell = shell_at(dir.path());
    assert_eq!(shell.screen, Screen::Chat);
    assert!(shell.sessions.is_empty());
    assert!(shell.chat.lines.is_empty());
    assert!(!shell.chat.busy);
    // Other screens load lazily: nothing read yet.
    assert!(shell.memory.facts.is_idle());
    assert!(shell.skills.installed.is_idle());
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
    let roles: Vec<&str> = shell.chat.lines.iter().map(|l| l.role).collect();
    assert_eq!(roles, vec!["user", "assistant"]);
    assert_eq!(shell.chat.lines[1].content, "olá");
}

#[test]
fn navigation_switches_screens_and_loads_each_on_first_open() {
    let dir = tempdir();
    SqliteMemory::open(dir.path().join("zerith.db"))
        .expect("memory")
        .remember(
            &Scope::user("local"),
            FactKind::Fact,
            "usa Rust",
            Confidence::Confirmed,
            None,
        )
        .expect("remember");
    let mut shell = shell_at(dir.path());
    for screen in Screen::ALL {
        shell.navigate(screen);
        assert_eq!(shell.screen, screen);
    }
    assert_eq!(shell.memory.facts.ready().map(Vec::len), Some(1));
    assert!(shell.skills.installed.ready().is_some());
    assert!(shell.agents.list.ready().is_some());
    assert!(shell.settings.storage.ready().is_some());
    // New chat always returns to Chat.
    shell.new_chat();
    assert_eq!(shell.screen, Screen::Chat);
    assert!(shell.chat.selected.is_none());
}

#[test]
fn keyboard_shortcuts_switch_screens_start_chats_and_stop_turns() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    let mut tx = rune::Tx::default();
    let cmd = |c: char| rune::KeyEvent {
        key: rune::Key::Command(c),
        mods: rune::Mods {
            cmd: true,
            ctrl: true,
            ..Default::default()
        },
    };
    assert!(on_key(&mut shell, &cmd('5'), &mut tx));
    assert_eq!(shell.screen, Screen::Memory);
    assert!(on_key(&mut shell, &cmd('7'), &mut tx));
    assert_eq!(shell.screen, Screen::Settings);
    assert!(on_key(&mut shell, &cmd('k'), &mut tx));
    assert_eq!(shell.screen, Screen::History);
    assert!(on_key(&mut shell, &cmd('n'), &mut tx));
    assert_eq!(shell.screen, Screen::Chat);

    let esc = rune::KeyEvent {
        key: rune::Key::Escape,
        mods: rune::Mods::default(),
    };
    assert!(!on_key(&mut shell, &esc, &mut tx), "nothing to stop");
    let token = zerith_providers::Cancel::default();
    shell.chat.cancel = Some(token.clone());
    shell.chat.busy = true;
    assert!(on_key(&mut shell, &esc, &mut tx));
    assert!(token.is_cancelled());
}

#[test]
fn approval_card_transitions_once_session_and_deny() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());

    shell.request_approval(approval("shell"));
    assert!(shell.chat.pending_approval.is_some());
    shell.decide(Decision::Once);
    assert!(shell.chat.pending_approval.is_none());
    assert!(
        !shell.chat.allowed.contains("shell"),
        "once is not a standing grant"
    );

    shell.request_approval(approval("shell"));
    shell.decide(Decision::Session);
    assert!(shell.chat.allowed.contains("shell"));
    // Already allowed for this session: answered without a card.
    shell.request_approval(approval("shell"));
    assert!(shell.chat.pending_approval.is_none());

    shell.request_approval(approval("net"));
    shell.decide(Decision::Deny);
    assert!(shell.chat.pending_approval.is_none());
    assert!(!shell.chat.allowed.contains("net"));
    assert_eq!(shell.chat.status, "net negado");

    let got: Vec<Decision> = shell.chat.decisions.iter().map(|(_, d)| *d).collect();
    assert_eq!(
        got,
        vec![
            Decision::Once,
            Decision::Session,
            Decision::Session,
            Decision::Deny
        ]
    );

    // A new chat forgets the grants; deciding with no card does nothing.
    shell.new_chat();
    assert!(shell.chat.allowed.is_empty());
    shell.decide(Decision::Once);
    assert_eq!(shell.chat.decisions.len(), 4);

    // Stopping a turn denies the pending request.
    shell.request_approval(approval("fs"));
    shell.chat.cancel = Some(zerith_providers::Cancel::default());
    shell.stop();
    assert!(shell.chat.pending_approval.is_none());
}

#[test]
fn history_groups_by_local_day() {
    const DAY: i64 = 86_400_000;
    // 2026-11-16 12:00 UTC, on a UTC-3 clock: 09:00 local.
    let now = 1_794_830_400_000;
    let off = -3 * 3600;
    assert_eq!(day_bucket(now - 60_000, now, off), Bucket::Today);
    assert_eq!(day_bucket(now - DAY, now, off), Bucket::Yesterday);
    assert_eq!(day_bucket(now - 3 * DAY, now, off), Bucket::ThisWeek);
    assert_eq!(day_bucket(now - 30 * DAY, now, off), Bucket::Older);
    // 10 hours earlier is 23:00 the previous local day, not "today".
    assert_eq!(
        day_bucket(now - 10 * 3_600_000, now, off),
        Bucket::Yesterday
    );

    let rows = [
        row("a", "a", "native", now - 30 * DAY, false),
        row("b", "b", "native", now, false),
        row("c", "c", "native", now - DAY, false),
        row("d", "d", "native", now - 60_000, false),
    ];
    let refs: Vec<&Row> = rows.iter().collect();
    let groups = group_by_day(&refs, now, off);
    let shape: Vec<(Bucket, Vec<&str>)> = groups
        .iter()
        .map(|(b, v)| (*b, v.iter().map(|r| r.id.as_str()).collect()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (Bucket::Today, vec!["b", "d"]),
            (Bucket::Yesterday, vec!["c"]),
            (Bucket::Older, vec!["a"]),
        ]
    );
}

#[test]
fn history_filters_by_text_hits_source_and_archive() {
    let rows = vec![
        row("1", "Deploy do servidor", "native", 0, false),
        row("2", "Importada", "hermes", 0, false),
        row("3", "Velha", "native", 0, true),
    ];
    let ids = |v: Vec<&Row>| v.into_iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(filter_rows(&rows, "", None, SourceFilter::All, false)),
        ["1", "2"]
    );
    assert_eq!(
        ids(filter_rows(&rows, "deploy", None, SourceFilter::All, false)),
        ["1"]
    );
    assert_eq!(
        ids(filter_rows(&rows, "", None, SourceFilter::Hermes, false)),
        ["2"]
    );
    assert_eq!(
        ids(filter_rows(&rows, "", None, SourceFilter::All, true)),
        ["3"]
    );
    // A content hit matches even when the title does not.
    let hits = vec!["2".to_string()];
    assert_eq!(
        ids(filter_rows(
            &rows,
            "kubernetes",
            Some(&hits),
            SourceFilter::All,
            false
        )),
        ["2"]
    );
}

#[test]
fn history_search_archive_and_export_use_the_database() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let store = Store::open(&db).expect("store");
    let a = store
        .create_session("alpha", SessionSource::Native)
        .expect("s");
    let b = store
        .create_session("beta", SessionSource::Native)
        .expect("s");
    store
        .append(&b.id, &NewMessage::new(Role::User, "falar de kubernetes"))
        .expect("append");

    let mut shell = shell_at(dir.path());
    shell.navigate(Screen::History);
    shell.history.query = rune::TextEdit::new("kubernetes");
    zerith_ui::screens::history::search(&mut shell);
    match &shell.history.hits {
        Load::Ready((_, ids)) => assert_eq!(ids, &vec![b.id.clone()]),
        other => panic!("search did not land: {other:?}"),
    }

    zerith_ui::screens::history::export(&mut shell, b.id.clone());
    match &shell.history.notice {
        Some(Ok(m)) => assert!(m.starts_with("Exportado")),
        other => panic!("export failed: {other:?}"),
    }
    let exported = dir.path().join("exports").join(format!("{}.md", b.id));
    assert!(std::fs::read_to_string(exported)
        .expect("read")
        .contains("kubernetes"));

    zerith_ui::screens::history::archive(&mut shell, a.id.clone(), true);
    assert!(
        shell
            .sessions
            .iter()
            .find(|r| r.id == a.id)
            .expect("row")
            .archived
    );
    assert!(store.session(&a.id).expect("q").expect("row").archived);
    assert!(shell.visible_sessions().iter().all(|r| r.id != a.id));
}

#[test]
fn the_memory_screen_lists_edits_searches_and_forgets() {
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
    mem.remember(
        &scope,
        FactKind::Preference,
        "prefere tema escuro",
        Confidence::Confirmed,
        None,
    )
    .expect("remember");

    let mut shell = shell_at(dir.path());
    shell.navigate(Screen::Memory);
    assert_eq!(shell.memory.facts.ready().map(Vec::len), Some(2));
    assert_eq!(shell.memory.items.len(), 4, "two kind headers, two facts");

    shell.memory.query = rune::TextEdit::new("Rust");
    zerith_ui::screens::memory::search(&mut shell);
    assert_eq!(shell.memory.items.len(), 2);

    let id = shell.memory.facts.ready().expect("facts")[0].id;
    shell.memory.editing = Some((id, rune::TextEdit::new("usa Rust 1.99")));
    zerith_ui::screens::memory::save_edit(&mut shell);
    assert_eq!(mem.get(id).expect("get").text, "usa Rust 1.99");

    shell.forget(id);
    assert!(mem
        .list(&scope, 10)
        .expect("list")
        .iter()
        .all(|f| f.id != id));
}

#[test]
fn provider_form_validates_and_presets_9router() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    shell.providers.form.url = rune::TextEdit::new("ftp://x");
    assert!(shell.providers.from_form().is_err());
    zerith_ui::screens::providers::apply_preset(&mut shell);
    assert_eq!(shell.providers.form.url.text, "http://localhost:20128/v1");
    shell.providers.form.timeout = rune::TextEdit::new("5");
    assert!(shell.providers.from_form().is_err(), "timeout out of range");
    shell.providers.form.timeout = rune::TextEdit::new("30000");
    shell.providers.form.model = rune::TextEdit::new("m1");
    let cfg = shell.providers.from_form().expect("valid");
    assert_eq!(cfg.timeout_ms, 30_000);
    assert_eq!(cfg.default_model.as_deref(), Some("m1"));
    assert!(
        shell.providers.form.key.is_secure(),
        "the key field is masked"
    );
}

#[test]
fn theme_motion_and_profile_persist_in_settings_toml() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    // No settings file: the core default is System.
    assert_eq!(shell.theme, Theme::System);
    shell.set_theme(Theme::Light);
    shell.set_reduce_motion(true);
    drop(shell);
    let shell = shell_at(dir.path());
    assert_eq!(shell.theme, Theme::Light);
    assert!(shell.reduce_motion);
    let text = std::fs::read_to_string(dir.path().join("settings.toml")).expect("settings");
    assert!(text.contains("theme = \"light\""), "{text}");

    // A corrupt file is skipped by the loader, not fatal, and not overwritten.
    std::fs::write(dir.path().join("settings.toml"), "ui = [").expect("write");
    let mut shell = shell_at(dir.path());
    assert_eq!(shell.theme, Theme::System);
    shell.toggle_theme();
    assert_eq!(shell.theme, Theme::Light);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("settings.toml")).expect("read"),
        "ui = ["
    );
    assert_eq!(Theme::parse(Some("bogus")), Theme::Dark);
}

#[test]
fn memory_pause_is_the_database_switch() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    assert!(!shell.memory.paused);
    zerith_ui::screens::memory::toggle_pause(&mut shell);
    let mem = SqliteMemory::open(dir.path().join("zerith.db")).expect("memory");
    assert!(mem.is_paused().expect("paused"));
    // A new window reads the same switch.
    let shell = shell_at(dir.path());
    assert!(shell.memory.paused);
}

#[test]
fn stop_cancels_the_token_and_does_nothing_without_a_turn() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    shell.stop();
    assert!(!shell.chat.busy);
    let token = zerith_providers::Cancel::default();
    shell.chat.cancel = Some(token.clone());
    shell.chat.busy = true;
    shell.stop();
    assert!(token.is_cancelled());
    assert_eq!(shell.chat.status, "cancelado");
    assert!(shell.chat.cancel.is_none());
}

#[test]
fn collapsible_sections_toggle_and_reset_on_new_chat() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    shell.toggle_open(3);
    assert!(shell.chat.open.contains(&3));
    shell.toggle_open(3);
    assert!(shell.chat.open.is_empty());
    shell.toggle_open(1);
    shell.new_chat();
    assert!(shell.chat.open.is_empty());
}

#[test]
fn an_unfinished_plan_shows_up_in_the_window() {
    let dir = tempdir();
    let store = Store::open(dir.path().join("zerith.db")).expect("store");
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
fn an_approval_event_reaches_the_runtime_channel() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    let (tx, _gate) = zerith_runtime::approval_channel();
    shell.chat.approvals = Some(tx);
    shell.apply(zerith_runtime::RunEvent::ApprovalNeeded {
        id: 7,
        tool: "shell".into(),
        summary: "cargo test".into(),
    });
    let card = shell.chat.pending_approval.clone().expect("card shown");
    assert_eq!(card.id, "7");
    shell.decide(Decision::Once);
    assert!(shell.chat.pending_approval.is_none());
    // Tool rows land in the transcript as collapsible "tool" lines.
    shell.apply(zerith_runtime::RunEvent::ToolResult {
        id: "c1".into(),
        name: "read_file".into(),
        ok: true,
        output_preview: "fn main".into(),
    });
    assert!(shell.chat.lines.iter().any(|l| l.role == "tool"));
}

#[test]
fn agents_come_from_the_core_and_the_chat_can_pick_one() {
    let dir = tempdir();
    std::fs::create_dir_all(dir.path().join("agents")).expect("dir");
    std::fs::write(
        dir.path().join("agents/revisor.md"),
        "---\ndescription: Revisa diffs\nmodel: m1\n---\nAponte unwrap.",
    )
    .expect("write");
    let mut shell = shell_at(dir.path());
    shell.navigate(Screen::Agents);
    let list = shell.agents.list.ready().expect("loaded");
    let revisor = list.iter().find(|a| a.name == "revisor").expect("revisor");
    assert_eq!(revisor.description, "Revisa diffs");
    assert_eq!(revisor.model.as_deref(), Some("m1"));
    assert_eq!(revisor.layer, "user");

    // Opening the agent menu lists them; the default agent is no selection.
    zerith_ui::screens::chat::toggle_agents(&mut shell);
    assert!(shell.chat.agent_menu);
    assert!(shell.chat.agent.is_none());
}
