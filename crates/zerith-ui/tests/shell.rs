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
    assert!(shell.providers.usage.ready().is_some());
    assert!(shell.settings.storage.ready().is_some());
    assert!(shell.settings.savings.ready().is_some());
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
    assert_eq!(shell.screen, Screen::Media);
    assert!(on_key(&mut shell, &cmd('6'), &mut tx));
    assert_eq!(shell.screen, Screen::Memory);
    assert!(on_key(&mut shell, &cmd('8'), &mut tx));
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
fn provider_form_validates_and_presets_native() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    shell.providers.form.url = rune::TextEdit::new("ftp://x");
    assert!(shell.providers.from_form().is_err());
    zerith_ui::screens::providers::apply_preset(&mut shell);
    assert_eq!(shell.providers.form.name.text, "anthropic");
    assert_eq!(
        shell.providers.form.url.text,
        "https://api.anthropic.com/v1"
    );
    assert_eq!(
        shell.providers.form.protocol,
        zerith_providers::Protocol::Anthropic
    );
    zerith_ui::screens::providers::apply_kind_preset(
        &mut shell,
        zerith_providers::ProviderKind::DeepSeek,
    );
    assert_eq!(shell.providers.form.name.text, "deepseek");
    assert_eq!(shell.providers.form.url.text, "https://api.deepseek.com/v1");
    assert_eq!(
        shell.providers.form.protocol,
        zerith_providers::Protocol::OpenAi
    );
    shell.providers.form.timeout = rune::TextEdit::new("5");
    assert!(shell.providers.from_form().is_err(), "timeout out of range");
    shell.providers.form.timeout = rune::TextEdit::new("30000");
    shell.providers.form.model = rune::TextEdit::new("m1");
    let cfg = shell.providers.from_form().expect("valid");
    assert_eq!(cfg.timeout_ms, 30_000);
    assert_eq!(cfg.default_model.as_deref(), Some("m1"));
    assert_eq!(cfg.protocol, zerith_providers::Protocol::OpenAi);
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
    // Tool results land in the transcript as tool rows.
    shell.apply(zerith_runtime::RunEvent::ToolResult {
        id: "c1".into(),
        name: "read_file".into(),
        ok: true,
        output_preview: "fn main".into(),
    });
    assert!(shell
        .chat
        .lines
        .iter()
        .any(|l| matches!(&l.item, Some(zerith_ui::agentwork::Item::Tool(_)))));
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

#[test]
fn the_profile_sets_real_limits_and_persists() {
    let dir = tempdir();
    let mut shell = shell_at(dir.path());
    assert_eq!(shell.settings.profile, zerith_ui::Profile::Balanced);
    shell.set_profile(zerith_ui::Profile::Economy);
    let loaded = zerith_core::Settings::load(&zerith_core::Paths::with_root(dir.path()), None);
    assert_eq!(
        loaded.settings.profile,
        zerith_core::settings::Profile::Economy
    );
    assert_eq!(loaded.settings.profile.limits().history_window, 20);
    assert_eq!(
        shell_at(dir.path()).settings.profile,
        zerith_ui::Profile::Economy
    );
}

#[test]
fn hermes_import_panel_previews_and_imports_selected_sessions() {
    let dir = tempdir();
    let hermes_db = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&hermes_db).expect("open hermes db");
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
            VALUES ('h1', 'hermes-3', 1700000000.0, 1700000050.0, 'Sessão ativa', 'default', 0),
                   ('h2', 'hermes-3', 1700000100.0, 1700000200.0, 'Sessão arquivada', 'default', 1);
         INSERT INTO messages(session_id, role, content, timestamp, active)
            VALUES ('h1', 'user', 'oi', 1700000001.0, 1),
                   ('h1', 'assistant', 'olá', 1700000002.0, 1),
                   ('h2', 'user', 'antiga', 1700000150.0, 1);",
    )
    .expect("seed hermes db");
    drop(conn);

    let mut shell = shell_at(dir.path());
    shell.navigate(Screen::History);
    assert!(shell.history.import_include_archived);
    zerith_ui::screens::history::toggle_import(&mut shell);
    assert!(shell.history.import_open);

    shell.history.import_path = rune::TextEdit::new(hermes_db.display().to_string());
    zerith_ui::screens::history::preview_custom_path(&mut shell);
    match &shell.history.import_preview {
        Load::Ready(sources) => {
            assert_eq!(sources.len(), 1);
            assert_eq!(sources[0].sessions.len(), 2);
            assert_eq!(sources[0].total_messages, 3);
        }
        other => panic!("preview did not land: {other:?}"),
    }
    assert_eq!(shell.history.import_picked.len(), 2);

    zerith_ui::screens::history::toggle_import_session(&mut shell, "h2");
    assert_eq!(shell.history.import_picked.len(), 1);
    assert!(shell.history.import_picked.contains("h1"));

    zerith_ui::screens::history::run_import(&mut shell);
    assert!(!shell.history.import_running);
    match &shell.history.import_report {
        Some(Ok(report)) => {
            assert_eq!(report.sessions_imported, 1);
            assert_eq!(report.messages_imported, 2);
        }
        other => panic!("import did not succeed: {other:?}"),
    }
    assert!(shell
        .sessions
        .iter()
        .any(|r| r.source == "hermes" && r.title == "Sessão ativa"));
    assert!(!shell.sessions.iter().any(|r| r.title == "Sessão arquivada"));
}

#[test]
fn token_saver_and_multi_model_routing_persist_and_load_usage() {
    use zerith_core::{CavemanLevel, Paths, PonytailLevel, Settings};
    use zerith_runtime::{RunEvent, TurnReport};
    use zerith_sessions::NewUsageRecord;
    use zerith_ui::screens::{chat, providers, settings};

    let dir = tempdir();
    let mut shell = shell_at(dir.path());

    // 1. Token Saver defaults and persistence in settings.toml.
    assert_eq!(shell.settings.token_saver.ponytail, PonytailLevel::Off);
    assert_eq!(shell.settings.token_saver.caveman, CavemanLevel::Off);
    assert!(shell.settings.token_saver.rtk_enabled);
    settings::set_ponytail(&mut shell, PonytailLevel::Full);
    settings::set_caveman(&mut shell, CavemanLevel::Lite);
    settings::set_rtk_enabled(&mut shell, false);

    // 2. Gateway accounts (multi-account for the same provider) & multi-model routing.
    providers::new_account(&mut shell);
    shell.providers.account_form.id = rune::TextEdit::new("ant-1");
    shell.providers.account_form.provider = rune::TextEdit::new("anthropic");
    shell.providers.account_form.priority = rune::TextEdit::new("1");
    shell.providers.account_form.quota_tokens = rune::TextEdit::new("100000");
    shell.providers.account_form.window_hours = rune::TextEdit::new("5");
    shell.providers.account_form.default_model = rune::TextEdit::new("claude-sonnet-4");
    providers::save_account(&mut shell);
    assert!(matches!(shell.providers.account_notice, Some(Ok(_))));

    providers::new_account(&mut shell);
    shell.providers.account_form.id = rune::TextEdit::new("ant-2");
    shell.providers.account_form.provider = rune::TextEdit::new("anthropic");
    shell.providers.account_form.priority = rune::TextEdit::new("2");
    shell.providers.account_form.quota_tokens = rune::TextEdit::new("200000");
    providers::save_account(&mut shell);
    providers::toggle_account_enabled(&mut shell, "ant-2".into());

    shell.providers.routing_form.main_model = rune::TextEdit::new("anthropic/claude-opus-5-5");
    shell.providers.routing_form.subagent_model =
        rune::TextEdit::new("deepseek/deepseek-4.1-flash");
    providers::save_routing(&mut shell);
    assert!(matches!(shell.providers.routing_notice, Some(Ok(_))));
    drop(shell);

    // Reopen shell from disk: all settings, accounts and routing reload cleanly.
    let mut reopened = shell_at(dir.path());
    assert_eq!(reopened.settings.token_saver.ponytail, PonytailLevel::Full);
    assert_eq!(reopened.settings.token_saver.caveman, CavemanLevel::Lite);
    assert!(!reopened.settings.token_saver.rtk_enabled);

    let disk = Settings::load(&Paths::with_root(dir.path()), None).settings;
    assert_eq!(disk.token_saver, reopened.settings.token_saver);
    assert_eq!(
        disk.routing.main_model.as_deref(),
        Some("anthropic/claude-opus-5-5")
    );
    assert_eq!(
        disk.routing.subagent_model.as_deref(),
        Some("deepseek/deepseek-4.1-flash")
    );

    assert_eq!(reopened.providers.accounts.len(), 2);
    assert_eq!(reopened.providers.accounts[0].id, "ant-1");
    assert!(reopened.providers.accounts[0].enabled);
    assert_eq!(reopened.providers.accounts[0].quota_tokens, Some(100_000));
    assert_eq!(reopened.providers.accounts[1].id, "ant-2");
    assert!(!reopened.providers.accounts[1].enabled);
    assert_eq!(
        reopened.providers.routing.main_model.as_deref(),
        Some("anthropic/claude-opus-5-5")
    );
    assert_eq!(
        reopened.providers.routing.subagent_model.as_deref(),
        Some("deepseek/deepseek-4.1-flash")
    );
    assert!(reopened
        .providers
        .list
        .iter()
        .any(|p| p.name == "anthropic"));
    if std::env::var("ZERITH_PROVIDER")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_none()
    {
        assert_eq!(zerith_ui::active_provider(&reopened).name, "anthropic");
    }
    if std::env::var("ZERITH_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_none()
    {
        assert_eq!(reopened.chat.model, "anthropic/claude-opus-5-5");
    }

    // 3. Usage summary, Token Saver savings breakdown, and cooldown clearing.
    let store = Store::open(dir.path().join("zerith.db")).expect("store");
    store
        .record_usage(&NewUsageRecord {
            session_id: None,
            account_id: "ant-1".into(),
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
            role_kind: "main".into(),
            prompt_tokens: 800,
            completion_tokens: 200,
            saved_rtk_tokens: 120,
            saved_ponytail_tokens: 50,
            saved_caveman_tokens: 30,
            saved_tokens: 200,
            status: "ok".into(),
            latency_ms: 420,
        })
        .expect("record usage");
    let future_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + 120_000;
    store
        .set_account_cooldown("ant-1", future_ms, Some("rate limited"))
        .expect("set cooldown");
    drop(store);

    reopened.navigate(Screen::Settings);
    let savings = reopened.settings.savings.ready().expect("savings loaded");
    assert_eq!(savings.total_saved_rtk_tokens, 120);
    assert_eq!(savings.total_saved_ponytail_tokens, 50);
    assert_eq!(savings.total_saved_caveman_tokens, 30);
    assert_eq!(savings.total_saved_tokens, 200);

    reopened.navigate(Screen::Providers);
    let usage = reopened.providers.usage.ready().expect("usage loaded");
    assert_eq!(usage.active_account.as_deref(), Some("ant-1"));
    assert_eq!(usage.active_model.as_deref(), Some("claude-opus-5-5"));
    assert!(
        usage
            .accounts
            .iter()
            .find(|a| a.account_id == "ant-1")
            .expect("ant-1 stats")
            .cooldown_until
            > 0
    );

    providers::clear_cooldown(&mut reopened, "ant-1".into());
    let usage_after = reopened.providers.usage.ready().expect("usage refreshed");
    assert_eq!(
        usage_after
            .accounts
            .iter()
            .find(|a| a.account_id == "ant-1")
            .expect("ant-1 stats")
            .cooldown_until,
        0
    );

    // 4. Live route and TokenSaved events in ChatState + report completion + UI frame build.
    reopened.apply(RunEvent::Route {
        account_id: "ant-1".into(),
        provider: "anthropic".into(),
        model: "claude-opus-5-5".into(),
        attempts: 1,
    });
    assert_eq!(
        reopened.chat.active_route,
        Some(("ant-1".into(), "claude-opus-5-5".into()))
    );
    reopened.apply(RunEvent::TokenSaved {
        tool: "shell".into(),
        before_bytes: 2_400,
        after_bytes: 400,
        filter: Some("build-output".into()),
    });
    assert_eq!(reopened.chat.saved_tokens, 500);

    chat::events::apply_report(
        &mut reopened,
        &TurnReport {
            prompt_tokens: 900,
            completion_tokens: 100,
            saved_tokens: 650,
            account_id: Some("ant-2".into()),
            model_used: Some("claude-sonnet-4".into()),
            ..TurnReport::default()
        },
    );
    assert_eq!(reopened.chat.saved_tokens, 650);
    assert_eq!(
        reopened.chat.active_route,
        Some(("ant-2".into(), "claude-sonnet-4".into()))
    );

    for screen in [Screen::Providers, Screen::Settings, Screen::Chat] {
        for dark in [true, false] {
            let mut sh = shell_at(dir.path());
            sh.navigate(screen);
            sh.chat.active_route = Some(("ant-1".into(), "claude-opus-5-5".into()));
            sh.chat.saved_tokens = 650;
            let mut rt = rune::Runtime::new(
                sh,
                zerith_ui::view,
                (1100.0, 720.0),
                rune::Env::new(dark, 1.0),
            );
            rt.frame(0.0);
        }
    }

    reopened.new_chat();
    assert!(reopened.chat.active_route.is_none());
    assert_eq!(reopened.chat.saved_tokens, 0);
}
