//! `zerith-ui` — opens the native window.
//!
//! `--tree` and `--snap <file.png>` are Rune's headless modes, so the same
//! binary proves the UI renders without a display. `--scene <name>` opens a
//! screen directly (`--list-scenes`), for snapshots of every screen.

use zerith_core::Paths;
use zerith_ui::agentwork::{
    Item, Question, QuestionSet, SubState, Subagent, TaskItem, TaskState, ToolRow, ToolState,
    WorkEvent,
};
use zerith_ui::{spec, Line, Screen, Shell, Theme};

fn go(s: &mut Shell, to: Screen) {
    // Scenes follow `--theme` so one scene snaps in both appearances.
    s.theme = Theme::System;
    // Headless snapshots draw one frame: load inline so it shows data, not a spinner.
    s.inline = true;
    s.navigate(to);
    s.inline = false;
}

const PLAN: &str = "## Migrar o cliente HTTP

1. Ler `crates/zerith-providers/src/openai.rs` e mapear os pontos de uso
2. Trocar o cliente e manter a API pública
3. Atualizar os testes e rodar `cargo test -p zerith-providers`

**Risco:** a mudança toca o fluxo de streaming.";

/// A conversation in the middle of agent work, for the work scenes.
fn demo_work(s: &mut Shell) {
    s.theme = Theme::System;
    s.chat.model = "claude-sonnet".into();
    s.chat.usage = Some((104_000, 2_300));
    s.chat.rewind = true;
    let tool = |id: &str, name: &str, arg: &str, state, ms, diff| {
        Line::item(Item::Tool(ToolRow {
            id: id.into(),
            name: name.into(),
            arg: arg.into(),
            state,
            millis: ms,
            started: None,
            preview: String::new(),
            diff,
        }))
    };
    s.chat.lines = vec![
        Line::new("user", "Migre o cliente HTTP do provedor OpenAI e mantenha os testes passando."),
        Line::new(
            "assistant",
            "Vou ler o cliente atual, trocar a implementação e rodar os testes.",
        ),
        tool("1", "read_file", "crates/zerith-providers/src/openai.rs", ToolState::Ok, Some(42), None),
        tool(
            "2",
            "edit_file",
            "crates/zerith-providers/src/openai.rs",
            ToolState::Ok,
            Some(310),
            zerith_ui::agentwork::diff_of_call(
                "edit_file",
                r#"{"path":"openai.rs","old_string":"let client = reqwest::Client::new();\nlet url = base;\nsend(client)","new_string":"let client = http::client(timeout);\nlet url = join(base, path);\nsend(client)"}"#,
            ),
        ),
        tool("3", "shell", "cargo test -p zerith-providers", ToolState::Running, None, None),
        Line::item(Item::Subagent(Subagent {
            id: "a".into(),
            name: "revisor".into(),
            state: SubState::Running,
            summary: "Revisando o diff em busca de unwrap e erros ignorados".into(),
        })),
        Line::item(Item::Subagent(Subagent {
            id: "b".into(),
            name: "explorador".into(),
            state: SubState::Done,
            summary: "Mapeou 4 usos do cliente\nOs usos estão em openai.rs, anthropic.rs, stream.rs e config.rs.".into(),
        })),
    ];
    s.chat.open.insert(6);
    zerith_ui::screens::chat::apply_work(
        s,
        WorkEvent::Tasks(vec![
            TaskItem::new("Ler o cliente atual", TaskState::Completed),
            TaskItem {
                text: "Trocar a implementação".into(),
                active: "Trocando a implementação".into(),
                state: TaskState::InProgress,
            },
            TaskItem::new("Rodar os testes", TaskState::Pending),
        ]),
    );
}

type Scene = (&'static str, fn(&mut Shell));

const SCENES: &[Scene] = &[
    ("chat", |s| go(s, Screen::Chat)),
    ("new-chat", |s| {
        s.new_chat();
        s.theme = Theme::System;
    }),
    ("approval", |s| {
        s.theme = Theme::System;
        s.request_approval(zerith_ui::Approval {
            id: "demo".into(),
            title: "Executar comando no terminal".into(),
            tool: "shell".into(),
            summary: "cargo test -p zerith-ui --quiet  (diretório: projeto atual)".into(),
        })
    }),
    ("work", |s| {
        demo_work(s);
    }),
    ("plan", |s| {
        demo_work(s);
        s.chat.tasks.clear();
        s.request_approval(zerith_ui::Approval {
            id: "7".into(),
            title: "Plano proposto".into(),
            tool: "exit_plan_mode".into(),
            summary: PLAN.into(),
        });
    }),
    ("plan-edit", |s| {
        demo_work(s);
        s.chat.tasks.clear();
        s.request_approval(zerith_ui::Approval {
            id: "7".into(),
            title: "Plano proposto".into(),
            tool: "exit_plan_mode".into(),
            summary: PLAN.into(),
        });
        zerith_ui::screens::chat::plan_edit(s);
    }),
    ("question", |s| {
        demo_work(s);
        s.chat.tasks.clear();
        zerith_ui::screens::chat::apply_work(
            s,
            WorkEvent::Question(QuestionSet {
                id: "q1".into(),
                questions: vec![
                    Question {
                        header: "Banco".into(),
                        text: "Qual banco de dados devo usar nos testes?".into(),
                        options: vec!["SQLite em memória".into(), "Arquivo temporário".into()],
                        multi: false,
                    },
                    Question {
                        header: "Extras".into(),
                        text: "O que mais devo rodar?".into(),
                        options: vec!["clippy".into(), "fmt".into(), "doc".into()],
                        multi: true,
                    },
                ],
            }),
        );
        zerith_ui::screens::chat::question_pick(s, 0, 0);
        zerith_ui::screens::chat::question_pick(s, 1, 0);
        zerith_ui::screens::chat::question_pick(s, 1, 2);
    }),
    ("slash", |s| {
        demo_work(s);
        s.chat.draft = rune::TextEdit::new("/c");
        s.chat.draft.focused = true;
    }),
    ("mention", |s| {
        demo_work(s);
        s.chat.files = zerith_ui::Load::Ready(vec![
            "crates/zerith-ui/src/lib.rs".into(),
            "crates/zerith-ui/src/ui.rs".into(),
            "crates/zerith-runtime/src/lib.rs".into(),
            "README.md".into(),
        ]);
        s.chat.draft = rune::TextEdit::new("veja @lib");
        s.chat.draft.focused = true;
    }),
    ("mode-menu", |s| {
        demo_work(s);
        s.chat.mode_menu = true;
    }),
    ("history", |s| go(s, Screen::History)),
    ("agents", |s| go(s, Screen::Agents)),
    ("skills", |s| go(s, Screen::Skills)),
    ("memory", |s| go(s, Screen::Memory)),
    ("providers", |s| go(s, Screen::Providers)),
    ("settings", |s| go(s, Screen::Settings)),
    ("catalog", |s| {
        go(s, Screen::Skills);
        s.inline = true;
        zerith_ui::screens::skills::set_tab(s, zerith_ui::screens::skills::Tab::Catalog);
        s.inline = false;
    }),
    ("memory-edit", |s| {
        go(s, Screen::Memory);
        if let Some(f) = s.memory.facts.ready().and_then(|v| v.first()) {
            let mut e = rune::TextEdit::new(f.text.clone());
            e.focused = true;
            s.memory.editing = Some((f.id, e));
        }
    }),
    ("model-menu", |s| {
        s.theme = Theme::System;
        s.chat.models = zerith_ui::Load::Ready(vec![
            "gpt-4o-mini".into(),
            "claude-sonnet".into(),
            "qwen2.5-coder".into(),
        ]);
        s.chat.model = "gpt-4o-mini".into();
        s.chat.model_menu = true;
    }),
];

fn main() {
    let mut spec = spec();
    spec.scenes = SCENES;
    let app = rune::App::new(spec);
    let me = app.handle();
    let paths = Paths::resolve();
    if let Err(e) = paths.ensure() {
        eprintln!("zerith-ui: cannot prepare {}: {e}", paths.root().display());
    }
    let db = paths.db();
    let ready = me.clone();
    app.init(move || Shell::open(db, me))
        // After the first frame, not before: the window shows at once and the
        // provider check lands when it answers.
        .on_ready(move |_| {
            if !rune::is_headless() {
                ready.update(|s: &mut Shell, _| zerith_ui::screens::chat::fetch_models(s));
            }
        })
        .run();
}
