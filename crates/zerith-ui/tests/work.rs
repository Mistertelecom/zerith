//! Agent-work state transitions: checklist, subagents, tool rows, plan card,
//! questions, slash and mention popups, modes, context meter, rewind.
//! No GPU: `App::new` builds the state container without a window.

use zerith_runtime::RunEvent;
use zerith_security::Mode;
use zerith_sessions::{NewMessage, Role, SessionSource, Store};
use zerith_ui::agentwork::{
    self as aw, filter_files, filter_slash, trigger, ContextMeter, DiffKind, DiffSummary, Fill,
    Item, PermissionMode, Question, QuestionSet, SubState, Subagent, TaskItem, TaskState, ToolRow,
    ToolState, Trigger, TurnChanges, WorkEvent,
};
use zerith_ui::screens::chat;
use zerith_ui::{spec, Approval, Decision, Screen, Shell};

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

fn plan_approval() -> Approval {
    Approval {
        id: "9".into(),
        title: "Plano proposto".into(),
        tool: "exit_plan_mode".into(),
        summary: "Plano proposto:\n\n1. ler\n2. editar".into(),
    }
}

#[test]
fn the_checklist_is_replaced_whole_and_counts_progress() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    chat::apply_work(
        &mut s,
        WorkEvent::Tasks(vec![
            TaskItem::new("a", TaskState::Completed),
            TaskItem::new("b", TaskState::InProgress),
            TaskItem::new("c", TaskState::Pending),
        ]),
    );
    assert_eq!(aw::task_progress(&s.chat.tasks), (1, 3));
    chat::apply_work(
        &mut s,
        WorkEvent::Tasks(vec![TaskItem::new("a", TaskState::Completed)]),
    );
    assert_eq!(s.chat.tasks.len(), 1, "an update replaces, never appends");
    chat::toggle_tasks(&mut s);
    assert!(!s.chat.tasks_open);
    assert_eq!(TaskState::parse("in_progress"), TaskState::InProgress);
    s.new_chat();
    assert!(s.chat.tasks.is_empty(), "a new chat forgets the list");
}

#[test]
fn subagent_rows_stack_and_change_state_in_place() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    let sub = |id: &str, state| {
        WorkEvent::Subagent(Subagent {
            id: id.into(),
            name: format!("agente-{id}"),
            state,
            summary: "resumo".into(),
        })
    };
    chat::apply_work(&mut s, sub("a", SubState::Running));
    chat::apply_work(&mut s, sub("b", SubState::Running));
    chat::apply_work(&mut s, sub("a", SubState::Done));
    let rows: Vec<(String, SubState)> = s
        .chat
        .lines
        .iter()
        .filter_map(|l| match &l.item {
            Some(Item::Subagent(a)) => Some((a.id.clone(), a.state)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("a".into(), SubState::Done),
            ("b".into(), SubState::Running)
        ]
    );
    s.toggle_open(0);
    assert!(s.chat.open.contains(&0));
}

#[test]
fn tool_rows_show_one_liners_durations_and_diffs() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.apply(RunEvent::ToolCall {
        id: "t1".into(),
        name: "edit_file".into(),
        args: r#"{"path":"src/a.rs","old_string":"a\nb","new_string":"a\nc"}"#.into(),
    });
    let Some(Item::Tool(row)) = s.chat.lines.last().and_then(|l| l.item.clone()) else {
        panic!("no tool row");
    };
    assert_eq!(row.arg, "src/a.rs");
    assert_eq!(row.state, ToolState::Running);
    let d = row.diff.expect("an edit carries a diff");
    assert_eq!((d.adds, d.dels), (1, 1));
    s.apply(RunEvent::ToolResult {
        id: "t1".into(),
        name: "edit_file".into(),
        ok: false,
        output_preview: "recusado".into(),
    });
    let Some(Item::Tool(row)) = s.chat.lines.iter().find_map(|l| l.item.clone()) else {
        panic!("row vanished");
    };
    assert_eq!(row.state, ToolState::Failed);
    assert!(row.diff.is_none(), "a refused edit changed nothing");
    assert!(row.millis.is_some());
    assert_eq!(aw::format_duration(320), "320 ms");
    assert_eq!(aw::format_duration(1_400), "1,4 s");
    assert_eq!(
        aw::tool_arg(r#"{"command":"cargo test\nsecond"}"#),
        "cargo test"
    );
    assert!(aw::diff_of_call("read_file", "{}").is_none());
    // A turn that ends with a call still running cannot leave it spinning.
    s.apply(RunEvent::ToolCall {
        id: "t2".into(),
        name: "shell".into(),
        args: r#"{"command":"ls"}"#.into(),
    });
    s.apply(RunEvent::Done { stored: None });
    let running = s
        .chat
        .lines
        .iter()
        .any(|l| matches!(&l.item, Some(Item::Tool(t)) if t.state == ToolState::Running));
    assert!(!running);
}

#[test]
fn a_plan_approval_becomes_a_plan_card_that_edits_then_approves() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    let (tx, _gate) = zerith_runtime::approval_channel();
    s.chat.approvals = Some(tx);
    s.apply(RunEvent::ApprovalNeeded {
        id: 9,
        tool: "exit_plan_mode".into(),
        summary: "Plano proposto:\n\n1. ler\n2. editar".into(),
    });
    let a = s.chat.pending_approval.clone().expect("card");
    assert_eq!(a.title, "Plano proposto");
    assert!(aw::is_plan(&a.tool, &a.title));
    assert_eq!(chat::plan_body(&a.summary), "1. ler\n2. editar");

    chat::plan_edit(&mut s);
    let e = s.chat.plan_edit.as_mut().expect("editor opens");
    assert_eq!(e.text, "1. ler\n2. editar");
    e.replace_text("1. ler\n2. editar\n3. testar");
    chat::plan_approve(&mut s);
    assert!(s.chat.pending_approval.is_none());
    assert!(s.chat.plan_edit.is_none());
    assert_eq!(s.chat.plan_edits.len(), 1);
    assert!(s.chat.plan_edits[0].1.ends_with("3. testar"));
    assert_eq!(s.chat.decisions.last().map(|d| d.1), Some(Decision::Once));

    // Approving without editing records no edit.
    s.request_approval(plan_approval());
    chat::plan_edit(&mut s);
    chat::plan_approve(&mut s);
    assert_eq!(s.chat.plan_edits.len(), 1);

    // "Continuar planejando" refuses the plan.
    s.request_approval(plan_approval());
    chat::plan_continue(&mut s);
    assert_eq!(s.chat.decisions.last().map(|d| d.1), Some(Decision::Deny));
    assert!(s.chat.pending_approval.is_none());
}

#[test]
fn a_question_card_needs_every_answer_and_hands_them_over() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    chat::apply_work(
        &mut s,
        WorkEvent::Question(QuestionSet {
            id: "q".into(),
            questions: vec![
                Question {
                    header: String::new(),
                    text: "um?".into(),
                    options: vec!["x".into(), "y".into()],
                    multi: false,
                },
                Question {
                    header: String::new(),
                    text: "varios?".into(),
                    options: vec!["a".into(), "b".into(), "c".into()],
                    multi: true,
                },
            ],
        }),
    );
    assert!(!s.chat.question.as_ref().expect("card").ready());
    chat::answer_question(&mut s);
    assert!(
        s.chat.question.is_some(),
        "submit is refused while unanswered"
    );

    chat::question_pick(&mut s, 0, 0);
    chat::question_pick(&mut s, 0, 1);
    chat::question_pick(&mut s, 1, 2);
    chat::question_pick(&mut s, 1, 0);
    chat::question_pick(&mut s, 1, 2);
    chat::question_pick(&mut s, 1, 9);
    assert!(s.chat.question.as_ref().expect("card").ready());
    chat::answer_question(&mut s);
    assert!(s.chat.question.is_none());
    assert_eq!(
        s.chat.answers,
        vec![(
            "q".to_string(),
            vec![vec!["y".to_string()], vec!["a".to_string()]]
        )],
        "radio keeps the last pick; the checkbox toggled c off"
    );
}

#[test]
fn slash_and_mention_triggers_filter_and_replace_the_token() {
    let names = |q: &str| filter_slash(q).iter().map(|c| c.name).collect::<Vec<_>>();
    assert_eq!(names("").len(), 11);
    assert_eq!(names("c"), ["compact", "context", "clear"]);
    assert_eq!(
        names("mo"),
        ["model", "mode", "memory"],
        "prefix first, then contained"
    );
    assert_eq!(
        names("ear"),
        ["clear"],
        "a contained match follows prefixes"
    );
    assert_eq!(
        names("re"),
        ["resume", "rewind"],
        "rewind matches prefix after resume"
    );
    assert!(names("zzz").is_empty());

    let a = trigger("/co", 3).expect("slash");
    assert_eq!(
        (a.trigger, a.query.as_str(), a.start),
        (Trigger::Slash, "co", 0)
    );
    assert!(trigger("oi /co", 6).is_none(), "a command starts the draft");
    assert!(trigger("/co mais", 8).is_none(), "a space ends the command");
    let f = trigger("veja @sr", 8).expect("mention");
    assert_eq!(
        (f.trigger, f.query.as_str(), f.start),
        (Trigger::File, "sr", 5)
    );
    assert!(trigger("a@b", 3).is_none(), "an address is not a mention");

    let files: Vec<String> = ["src/lib.rs", "src/ui.rs", "README.md", "docs/lib.md"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        filter_files(&files, "lib", 10),
        ["src/lib.rs", "docs/lib.md"]
    );

    // Choosing a file replaces just the token.
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.chat.draft = rune::TextEdit::new("veja @li e depois");
    s.chat.draft.set_caret(8);
    let a = chat::active(&s).expect("active");
    chat::insert_file(&mut s, &a, "src/lib.rs");
    assert_eq!(s.chat.draft.text, "veja @src/lib.rs  e depois");
}

#[test]
fn slash_commands_run_and_the_unwired_ones_say_so() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.chat.draft = rune::TextEdit::new("/plan");
    chat::run_command(&mut s, "plan");
    assert_eq!(s.chat.permission_mode(), PermissionMode::Plan);
    assert!(s.chat.draft.text.is_empty());
    chat::run_command(&mut s, "skills");
    assert_eq!(s.screen, Screen::Skills);
    s.navigate(Screen::Chat);
    chat::run_command(&mut s, "compact");
    assert!(
        s.chat.status.contains("Nada para compactar"),
        "{}",
        s.chat.status
    );
    // Esc closes the popup for this text; typing reopens it.
    s.chat.draft = rune::TextEdit::new("/c");
    s.chat.draft.set_caret(2);
    assert!(chat::active(&s).is_some());
    chat::popup_close(&mut s);
    assert!(chat::active(&s).is_none());
    s.chat.draft = rune::TextEdit::new("/co");
    s.chat.draft.set_caret(3);
    assert!(chat::active(&s).is_some());
}

#[test]
fn the_mode_cycles_like_claude_code_and_drives_the_shared_handle() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    chat::set_mode(&mut s, PermissionMode::Default);
    let handle = s.chat.mode.clone();
    chat::cycle_mode(&mut s);
    assert_eq!(handle.get(), Mode::AcceptEdits);
    chat::cycle_mode(&mut s);
    assert_eq!(handle.get(), Mode::Plan);
    chat::cycle_mode(&mut s);
    assert_eq!(handle.get(), Mode::Default);
    // The modes that skip questions are chosen, never cycled into, and the
    // cycle leaves them for the default.
    chat::set_mode(&mut s, PermissionMode::Bypass);
    chat::cycle_mode(&mut s);
    assert_eq!(handle.get(), Mode::Default);
    let labels: Vec<&str> = PermissionMode::ALL.iter().map(|m| m.label()).collect();
    assert_eq!(
        labels,
        [
            "Padrão",
            "Aceitar edições",
            "Plano",
            "Sem perguntas",
            "Livre"
        ]
    );
    for m in PermissionMode::ALL {
        assert_eq!(PermissionMode::from_security(m.to_security()), m);
    }
}

#[test]
fn the_context_meter_turns_amber_past_eighty_percent() {
    let m = |used, limit| ContextMeter { used, limit };
    assert_eq!(m(50_000, Some(128_000)).fill(), Fill::Ok);
    assert_eq!(
        m(102_400, Some(128_000)).fill(),
        Fill::Ok,
        "80% is not past it"
    );
    assert_eq!(m(102_500, Some(128_000)).fill(), Fill::Warn);
    assert_eq!(m(125_000, Some(128_000)).fill(), Fill::Full);
    assert_eq!(m(500, None).fill(), Fill::Ok);
    assert_eq!(m(500, None).fraction(), None, "no limit, no bar");
    assert_eq!(m(300_000, Some(128_000)).fraction(), Some(1.0));
    assert_eq!(m(104_300, Some(128_000)).label(), "104k / 128k");
    assert_eq!(aw::compact_count(1_234), "1,2k");

    // Cost shows only when the price is known.
    let usage = (1_000_000, 100_000);
    assert_eq!(aw::turn_cost(usage, None), None);
    let price = aw::Price {
        input_per_m: 3.0,
        output_per_m: 15.0,
    };
    let c = aw::turn_cost(usage, Some(price)).expect("priced");
    assert!((c - 4.5).abs() < 1e-9);
    assert_eq!(aw::format_cost(c), "US$ 4,50");

    let dir = tempdir();
    let mut s = shell_at(dir.path());
    assert!(s.chat.meter(Some(1000)).is_none(), "no turn, no meter");
    s.apply(RunEvent::Usage {
        input: 900,
        output: 50,
    });
    assert_eq!(s.chat.meter(Some(1000)).expect("meter").fill(), Fill::Warn);
}

#[test]
fn rewind_reports_when_there_is_no_checkpoint() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let store = Store::open(&db).expect("store");
    let sess = store
        .create_session("r", SessionSource::Native)
        .expect("session");
    store
        .append(&sess.id, &NewMessage::new(Role::User, "oi"))
        .expect("append");
    let mut s = shell_at(dir.path());
    assert!(s.chat.rewind, "the checkpoints module exists");
    s.chat.selected = Some(sess.id.clone());
    chat::rewind(&mut s);
    assert!(
        s.chat.status.contains("nenhum ponto"),
        "no checkpoint yet: {}",
        s.chat.status
    );
}

#[test]
fn compaction_events_leave_a_quiet_note() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    chat::apply_work(&mut s, WorkEvent::Compaction { running: true });
    assert!(s.chat.compacting);
    chat::apply_work(&mut s, WorkEvent::Compaction { running: false });
    assert!(!s.chat.compacting);
    assert!(matches!(
        s.chat.lines.last().and_then(|l| l.item.as_ref()),
        Some(Item::Note(_))
    ));
}

#[test]
fn the_cards_build_in_both_themes_without_a_gpu() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.request_approval(plan_approval());
    chat::plan_edit(&mut s);
    chat::apply_work(
        &mut s,
        WorkEvent::Tasks(vec![TaskItem::new("a", TaskState::InProgress)]),
    );
    s.chat.draft = rune::TextEdit::new("/c");
    s.chat.draft.focused = true;
    s.chat.mode_menu = true;
    for dark in [true, false] {
        let mut rt = rune::Runtime::new(
            Shell::open(
                dir.path().join("zerith.db"),
                rune::App::new(spec()).handle(),
            ),
            zerith_ui::view,
            (1100.0, 720.0),
            rune::Env::new(dark, 1.0),
        );
        rt.frame(0.0);
    }
}

#[test]
fn turn_changes_counts_unique_files_and_ignores_failed_or_running_edits() {
    let mk = |id: &str, arg: &str, state: ToolState, adds: usize, dels: usize| ToolRow {
        id: id.into(),
        name: "edit_file".into(),
        arg: arg.into(),
        state,
        millis: Some(10),
        started: None,
        preview: String::new(),
        diff: Some(DiffSummary {
            rows: Vec::new(),
            more: 0,
            adds,
            dels,
        }),
    };
    let mut read = mk("r", "src/e.rs", ToolState::Ok, 0, 0);
    read.name = "read_file".into();
    read.diff = None;

    let tools = vec![
        mk("1", "src/a.rs", ToolState::Ok, 2, 1),
        mk("2", "src/a.rs", ToolState::Ok, 3, 2),
        mk("3", "src/b.rs", ToolState::Ok, 1, 0),
        mk("4", "src/c.rs", ToolState::Failed, 5, 4),
        mk("5", "src/d.rs", ToolState::Running, 2, 1),
        read,
    ];
    assert_eq!(
        aw::turn_changes(&tools),
        TurnChanges {
            files: 2,
            adds: 6,
            dels: 3,
        }
    );
    assert_eq!(aw::turn_changes(std::iter::empty()), TurnChanges::default());
}

#[test]
fn diff_of_call_populates_one_based_line_numbers() {
    let d = aw::diff_of_call(
        "edit_file",
        r#"{"path":"src/a.rs","old_string":"1\n2\n3\n4","new_string":"1\n2\n3\n40"}"#,
    )
    .expect("edit diff");
    let rows: Vec<(DiffKind, Option<usize>, &str)> = d
        .rows
        .iter()
        .map(|r| (r.kind, r.line_no, r.text.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (DiffKind::Ctx, Some(3), "3"),
            (DiffKind::Del, Some(4), "4"),
            (DiffKind::Add, Some(4), "40"),
        ]
    );

    let w = aw::diff_of_call(
        "write_file",
        r#"{"path":"src/new.rs","content":"alpha\nbeta"}"#,
    )
    .expect("write diff");
    let write_rows: Vec<(DiffKind, Option<usize>, &str)> = w
        .rows
        .iter()
        .map(|r| (r.kind, r.line_no, r.text.as_str()))
        .collect();
    assert_eq!(
        write_rows,
        vec![
            (DiffKind::Add, Some(1), "alpha"),
            (DiffKind::Add, Some(2), "beta"),
        ]
    );
}

#[test]
fn mode_and_rewind_slash_commands_execute() {
    let dir = tempdir();
    let db = dir.path().join("zerith.db");
    let store = Store::open(&db).expect("store");
    let sess = store
        .create_session("r", SessionSource::Native)
        .expect("session");
    store
        .append(&sess.id, &NewMessage::new(Role::User, "oi"))
        .expect("append");

    let mut s = shell_at(dir.path());
    chat::set_mode(&mut s, PermissionMode::Default);
    s.chat.draft = rune::TextEdit::new("/mode");
    chat::run_command(&mut s, "mode");
    assert_eq!(s.chat.permission_mode(), PermissionMode::AcceptEdits);
    assert!(s.chat.draft.text.is_empty());

    chat::run_command(&mut s, "mode");
    assert_eq!(s.chat.permission_mode(), PermissionMode::Plan);
    chat::run_command(&mut s, "mode");
    assert_eq!(s.chat.permission_mode(), PermissionMode::Default);

    s.chat.selected = Some(sess.id);
    s.chat.draft = rune::TextEdit::new("/rewind");
    chat::run_command(&mut s, "rewind");
    assert!(s.chat.draft.text.is_empty());
    assert!(
        s.chat.status.contains("nenhum ponto"),
        "no checkpoint yet: {}",
        s.chat.status
    );
}
