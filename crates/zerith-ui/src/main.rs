//! `zerith-ui` — opens the native window.
//!
//! `--tree` and `--snap <file.png>` are Rune's headless modes, so the same
//! binary proves the UI renders without a display. `--scene <name>` opens a
//! screen directly (`--list-scenes`), for snapshots of every screen.

use zerith_core::Paths;
use zerith_ui::{spec, Screen, Shell, Theme};

fn go(s: &mut Shell, to: Screen) {
    // Scenes follow `--theme` so one scene snaps in both appearances.
    s.theme = Theme::System;
    // Headless snapshots draw one frame: load inline so it shows data, not a spinner.
    s.inline = true;
    s.navigate(to);
    s.inline = false;
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
