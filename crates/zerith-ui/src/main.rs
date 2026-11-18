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
use zerith_ui::screens::providers::Tab as ProvTab;
use zerith_ui::screens::settings::Tab as SetTab;
use zerith_ui::{spec, Line, Screen, Shell, Theme};

fn providers_tab(s: &mut Shell, t: ProvTab) {
    go(s, Screen::Providers);
    s.inline = true;
    zerith_ui::screens::providers::set_tab(s, t);
    s.inline = false;
}

fn settings_tab(s: &mut Shell, t: SetTab) {
    go(s, Screen::Settings);
    zerith_ui::screens::settings::set_tab(s, t);
}

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

/// The Media screen over the fake catalogue: a saved image model with fixed
/// parameters, a connected key, nothing from the network or the keyring.
fn demo_media(s: &mut Shell) {
    use zerith_providers::media::Kind;
    use zerith_ui::screens::media::{self, demo};
    let (backend, _) = demo::backend(Some(media::KeySource::Keyring));
    s.media = media::MediaState::with_backend(backend, String::new());
    let m = demo::model(Kind::Image, 6);
    s.media.image.model_id = m.id.clone();
    s.media.image.saved = [
        ("aspect_ratio".to_string(), "16:9".into()),
        ("n".to_string(), 1.into()),
        ("quality".to_string(), "high".into()),
    ]
    .into_iter()
    .collect();
    s.media.video.model_id = demo::model(Kind::Video, 2).id;
    s.media.video.saved = [("duration".to_string(), 6.into())].into_iter().collect();
    go(s, Screen::Media);
}

type Scene = (&'static str, fn(&mut Shell));

const SCENES: &[Scene] = &[
    ("chat", |s| go(s, Screen::Chat)),
    ("new-chat", |s| {
        s.new_chat();
        s.theme = Theme::System;
    }),
    ("new-chat-model", |s| {
        // A provider that needs no key, but no model chosen yet.
        s.providers = zerith_ui::ProvidersState::new(vec![zerith_providers::ProviderConfig::new(
            zerith_providers::ProviderKind::Ollama,
        )]);
        s.chat.model.clear();
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
    ("agent-new", |s| {
        go(s, Screen::Agents);
        s.agents.form = Some(zerith_ui::NewAgent::demo());
    }),
    ("skills", |s| go(s, Screen::Skills)),
    ("memory", |s| go(s, Screen::Memory)),
    ("providers", |s| go(s, Screen::Providers)),
    ("providers-accounts", |s| {
        providers_tab(s, ProvTab::Accounts)
    }),
    ("providers-routing", |s| providers_tab(s, ProvTab::Routing)),
    ("providers-usage", |s| providers_tab(s, ProvTab::Usage)),
    ("settings", |s| go(s, Screen::Settings)),
    ("settings-tokens", |s| settings_tab(s, SetTab::Tokens)),
    ("settings-storage", |s| settings_tab(s, SetTab::Storage)),
    ("media", |s| {
        demo_media(s);
    }),
    ("media-search", |s| {
        use zerith_providers::media::Kind;
        demo_media(s);
        s.inline = true;
        zerith_ui::screens::media::set_kind(s, Kind::Image);
        s.media.image.query = rune::TextEdit::new("flux");
        s.media.image.picking = true;
        zerith_ui::screens::media::search_now(s, Kind::Image);
        s.inline = false;
    }),
    ("media-chat", |s| {
        demo_work(s);
        let dir = s.media_dir.clone();
        let img = dir.join("2026-11-16-gato.png");
        s.images
            .seed(&img, 480, 300, zerith_ui::mediaout::demo_pixels(480, 300));
        let vid = dir.join("2026-11-16-clipe.mp4");
        let slash = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
        let p = slash(&img);
        let result = format!(
            "Imagem gerada com black-forest-labs/flux-2.0-6.\nParâmetros enviados: aspect_ratio=16:9, n=1\nCusto: US$ 0,0400\nArquivo: {p} (image/png, 212 KB)\n![um gato de óculos]({p})\n"
        );
        let video = format!(
            "Vídeo gerado com google/veo-3.\nParâmetros enviados: duration=6\nCusto: US$ 1,2000\nArquivo: {} (video/mp4, 4096 KB)\n",
            slash(&vid)
        );
        let tool = |id: &str, name: &str, arg: &str, preview: String, ms| {
            Line::item(Item::Tool(ToolRow {
                id: id.into(),
                name: name.into(),
                arg: arg.into(),
                state: ToolState::Ok,
                millis: Some(ms),
                started: None,
                preview,
                diff: None,
            }))
        };
        s.chat.lines = vec![
            Line::new("user", "Gere um gato de óculos e depois um clipe curto."),
            tool("1", "generate_image", "um gato de óculos", result, 8_200),
            Line::new(
                "assistant",
                format!("Pronto, a imagem está acima.\n\n![gato](<{p}>)\n\n![remota](https://exemplo.invalid/x.png)"),
            ),
            tool("2", "generate_video", "gato andando", video, 94_000),
        ];
        s.chat.tasks.clear();
        s.chat.rewind = false;
    }),
    ("media-video", |s| {
        use zerith_providers::media::Kind;
        demo_media(s);
        zerith_ui::screens::media::set_kind(s, Kind::Video);
    }),
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
    let app = zerith_ui::app_with(spec);
    let me = app.handle();
    let mut paths = Paths::resolve();
    let writable = paths.ensure().is_ok()
        && zerith_sessions::Store::open(paths.db()).is_ok();
    if !writable {
        let fallback = Paths::with_root(std::env::temp_dir().join("zerith-ui-home"));
        if fallback.ensure().is_ok() {
            paths = fallback;
        } else if let Err(e) = paths.ensure() {
            eprintln!("zerith-ui: cannot prepare {}: {e}", paths.root().display());
        }
    }
    // First start: the skills that ship with the app arrive disabled. Quiet on
    // success; a failure is only a stderr note, never a reason to not open.
    if let Some(bundle) = paths.bundled_skills() {
        match zerith_skills::Library::open(paths.skills()).and_then(|l| l.seed_disabled(&bundle)) {
            Ok((_, failed)) => failed
                .iter()
                .for_each(|f| eprintln!("zerith-ui: skill embutida ignorada: {f}")),
            Err(e) => eprintln!("zerith-ui: não consegui preparar as skills embutidas: {e}"),
        }
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
