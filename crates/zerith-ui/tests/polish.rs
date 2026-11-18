//! The review fixes that are logic, not pixels: tabs, navigation, the empty
//! chat's variants, the first message never failing with "no model", the new
//! agent form, and what the chat may show of generated media.

use zerith_providers::{ProviderConfig, ProviderKind};
use zerith_ui::mediaout;
use zerith_ui::screens::agents::{name_problem, render_file, write_agent, NewAgent};
use zerith_ui::screens::chat::{self, EmptyKind};
use zerith_ui::screens::{providers, settings};
use zerith_ui::{spec, Load, Screen, Shell};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir")
}

fn shell_at(home: &std::path::Path) -> Shell {
    let app = rune::App::new(spec());
    let mut s = Shell::open(home.join("zerith.db"), app.handle());
    s.inline = true;
    s
}

// ------------------------------------------------------------------ tabs

#[test]
fn provider_tabs_switch_and_usage_loads_with_the_screen() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    assert_eq!(s.providers.tab, providers::Tab::Provider);
    let labels: Vec<&str> = providers::Tab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(labels, ["Provedor", "Contas", "Delegação", "Uso"]);
    for t in providers::Tab::ALL {
        providers::set_tab(&mut s, t);
        assert_eq!(s.providers.tab, t);
    }
    // The tab survives leaving and coming back.
    s.navigate(Screen::Chat);
    s.navigate(Screen::Providers);
    assert_eq!(s.providers.tab, providers::Tab::Usage);
    assert!(s.providers.usage.ready().is_some());
}

#[test]
fn settings_tabs_switch() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    let labels: Vec<&str> = settings::Tab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(labels, ["Geral", "Economia de tokens", "Armazenamento"]);
    assert_eq!(s.settings.tab, settings::Tab::General);
    settings::set_tab(&mut s, settings::Tab::Tokens);
    assert_eq!(s.settings.tab, settings::Tab::Tokens);
    // Switching tabs changes no stored value.
    assert_eq!(
        s.settings.token_saver,
        zerith_core::TokenSaverSettings::default()
    );
}

#[test]
fn the_media_screen_sits_after_skills_and_has_a_shortcut() {
    let all = Screen::ALL;
    let skills = all.iter().position(|s| *s == Screen::Skills).unwrap();
    assert_eq!(all[skills + 1], Screen::Media);
    assert_eq!(Screen::Media.label(), "Mídia");
    assert_eq!(all.len(), 8, "Ctrl+1..8");
}

// ----------------------------------------------------------- empty states

fn give(s: &mut Shell, cfg: ProviderConfig) {
    s.providers.list = vec![cfg];
    s.providers.selected = 0;
}

#[test]
fn the_empty_chat_says_what_is_missing() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());

    // A provider that needs a key and has none.
    let mut cfg = ProviderConfig::new(ProviderKind::OpenAi);
    cfg.api_key = None;
    give(&mut s, cfg);
    s.chat.model = "gpt-4o".into();
    assert_eq!(chat::empty_kind(&s), EmptyKind::NoProvider);
    assert!(!chat::provider_usable(&s));

    // A local provider needs no key: usable, but no model yet.
    give(&mut s, ProviderConfig::new(ProviderKind::Ollama));
    s.chat.model.clear();
    assert_eq!(chat::empty_kind(&s), EmptyKind::NoModel);

    s.chat.model = "qwen2.5-coder".into();
    assert_eq!(chat::empty_kind(&s), EmptyKind::Ready);

    // A key makes the first provider usable.
    give(
        &mut s,
        ProviderConfig::new(ProviderKind::OpenAi).with_key("sk-test"),
    );
    s.chat.model.clear();
    assert_eq!(chat::empty_kind(&s), EmptyKind::NoModel);
}

#[test]
fn pick_model_opens_the_menu_and_lists_models_once() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    give(&mut s, ProviderConfig::new(ProviderKind::Ollama));
    s.chat.models = Load::Ready(vec!["a".into(), "b".into()]);
    chat::pick_model(&mut s);
    assert!(s.chat.model_menu);
    assert_eq!(
        s.chat.models.ready().map(Vec::len),
        Some(2),
        "cached list kept"
    );
}

#[test]
fn the_first_message_never_fails_with_no_model() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.chat.models = Load::Ready(vec![]);

    // No usable provider: nothing is sent, nothing is lost, no error line.
    give(&mut s, ProviderConfig::new(ProviderKind::OpenAi));
    s.chat.model = "gpt-4o".into();
    s.chat.draft = rune::TextEdit::new("oi");
    chat::send(&mut s);
    assert!(s.chat.lines.is_empty(), "no failed turn in the transcript");
    assert!(!s.chat.busy);
    assert_eq!(s.chat.draft.text, "oi", "the draft is kept");
    assert!(s.chat.status.contains("provedor"), "{}", s.chat.status);

    // A provider but no model: the model menu opens instead of an error.
    give(&mut s, ProviderConfig::new(ProviderKind::Ollama));
    s.chat.model.clear();
    chat::send(&mut s);
    assert!(s.chat.lines.is_empty());
    assert!(s.chat.model_menu);
    assert_eq!(s.chat.draft.text, "oi");
    assert!(s.chat.status.contains("modelo"), "{}", s.chat.status);
    assert!(
        s.chat.lines.iter().all(|l| l.role != "error"),
        "never the old `Nenhum modelo` error line"
    );

    // The provider's own default model is used when there is one.
    let mut cfg = ProviderConfig::new(ProviderKind::Ollama).with_model("qwen2.5-coder");
    cfg.default_model = Some("qwen2.5-coder".into());
    give(&mut s, cfg);
    s.chat.model.clear();
    s.chat.model_menu = false;
    assert_eq!(chat::empty_kind(&s), EmptyKind::NoModel);
}

// ----------------------------------------------------------- agents form

fn form(name: &str, instructions: &str) -> NewAgent {
    NewAgent {
        name: rune::TextEdit::new(name),
        description: rune::TextEdit::new(""),
        instructions: rune::TextEdit::new(instructions),
        tools: rune::TextEdit::new(""),
        model: rune::TextEdit::new(""),
        error: None,
    }
}

#[test]
fn agent_names_follow_the_core_rules() {
    for ok in ["revisor", "revisor-rust", "a_b-1", &"x".repeat(64)] {
        assert_eq!(name_problem(ok), None, "{ok}");
    }
    for bad in [
        "",
        "  ",
        "com espaço",
        "acentuação",
        "a/b",
        "..",
        "a.b",
        "default",
        &"x".repeat(65),
    ] {
        assert!(name_problem(bad).is_some(), "{bad:?}");
    }
}

#[test]
fn a_new_agent_is_written_read_back_and_never_overwritten() {
    let dir = tempdir();
    let agents = zerith_core::Paths::with_root(dir.path()).agents();

    let mut f = form("revisor", "Você revisa diffs em Rust.");
    f.description = rune::TextEdit::new("Revisa diffs\nem Rust");
    f.tools = rune::TextEdit::new("read_file, grep");
    f.model = rune::TextEdit::new("gpt-4o-mini");
    let path = write_agent(&agents, &f).expect("written");
    assert_eq!(path, agents.join("revisor.md"));

    // The core reads back what the form meant.
    let def = zerith_core::agents::load_file(&path, "user").expect("loads");
    assert_eq!(def.name, "revisor");
    assert_eq!(def.description, "Revisa diffs em Rust");
    assert_eq!(def.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(
        def.tools.as_deref(),
        Some(&["read_file".to_string(), "grep".to_string()][..])
    );
    assert_eq!(def.instructions, "Você revisa diffs em Rust.");

    // A second one with the same name is refused and the file is untouched.
    let before = std::fs::read_to_string(&path).unwrap();
    let again = form("revisor", "Outra coisa.");
    let e = write_agent(&agents, &again).expect_err("refuses to overwrite");
    assert!(e.contains("Já existe"), "{e}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // Bad names and empty instructions write nothing.
    assert!(write_agent(&agents, &form("../fora", "x")).is_err());
    assert!(write_agent(&agents, &form("vazio", "   ")).is_err());
    assert!(!agents.join("vazio.md").exists());
    assert!(!agents.parent().unwrap().join("fora.md").exists());
}

#[test]
fn a_plain_agent_has_no_frontmatter() {
    let f = form("simples", "Só instruções.");
    assert_eq!(render_file(&f), "Só instruções.\n");
}

#[test]
fn the_form_creates_the_file_and_reloads_the_list() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    s.agents.form = Some(form("novo-agente", "Faça X."));
    zerith_ui::screens::agents::create(&mut s);
    assert!(s.agents.form.is_none(), "the form closes on success");
    assert!(matches!(&s.agents.notice, Some(Ok(m)) if m.contains("novo-agente.md")));
    let list = s.agents.list.ready().expect("list reloaded");
    assert!(list.iter().any(|a| a.name == "novo-agente"));

    // A clash keeps the form open with the reason and what was typed.
    s.agents.form = Some(form("novo-agente", "Outra."));
    zerith_ui::screens::agents::create(&mut s);
    let f = s.agents.form.as_ref().expect("form stays");
    assert!(f.error.as_deref().is_some_and(|e| e.contains("Já existe")));
    assert_eq!(f.instructions.text, "Outra.");

    // A bad name is caught before any worker runs.
    s.agents.form = Some(form("não pode", "x"));
    zerith_ui::screens::agents::create(&mut s);
    assert!(s.agents.form.as_ref().unwrap().error.is_some());
}

// ------------------------------------------------------- generated media

fn media_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("zerith-polish-media")
}

#[test]
fn the_chat_shows_only_local_files_from_the_media_folder() {
    let d = media_dir();
    let a = format!("{}/a.png", d.display()).replace('\\', "/");
    let ok = mediaout::parse(&format!("![x]({a})"), &d);
    assert_eq!(ok.pictures.len(), 1);
    for evil in [
        "https://evil.example/a.png",
        "http://localhost/a.png",
        "data:image/png;base64,AAAA",
        "file:///C:/Windows/win.ini",
        "C:/Windows/win.ini",
        "/etc/passwd",
        "../a.png",
    ] {
        let m = mediaout::parse(
            &format!("![x]({evil})\nArquivo: {evil} (image/png, 1 KB)"),
            &d,
        );
        assert!(m.is_empty(), "{evil} must not be shown");
    }
}

#[test]
fn opening_a_file_outside_the_media_folder_is_refused() {
    let dir = tempdir();
    let s = shell_at(dir.path());
    let outside = dir.path().join("zerith.db");
    let e = zerith_ui::open_media_for_test(&s.media_dir, &outside).expect_err("refused");
    assert!(e.contains("pasta de mídia"), "{e}");
    assert!(
        zerith_ui::open_media_for_test(&s.media_dir, std::path::Path::new("https://x/a.png"))
            .is_err()
    );
}

#[test]
fn generation_tool_rows_have_names_and_a_cost() {
    assert_eq!(mediaout::tool_title("generate_image"), Some("Gerar imagem"));
    assert_eq!(mediaout::tool_title("generate_video"), Some("Gerar vídeo"));
    let d = media_dir();
    let v = format!("{}/c.mp4", d.display()).replace('\\', "/");
    let m = mediaout::parse(
        &format!("Vídeo gerado com x.\nCusto: US$ 1,2000\nArquivo: {v} (video/mp4, 4096 KB)\n"),
        &d,
    );
    assert_eq!(m.clips.len(), 1);
    assert_eq!(m.clips[0].size, "4096 KB");
    assert_eq!(m.cost.as_deref(), Some("US$ 1,2000"));
}

// ----------------------------------------------- agents the model wrote

#[test]
fn an_agent_the_model_wrote_is_listed_disabled_and_enabling_it_reaches_the_chat() {
    use zerith_core::agents_write::{create_ai_agent, Creator, NewAgent as AiAgent};

    let dir = tempdir();
    let paths = zerith_core::Paths::with_root(dir.path());
    let tools = ["read_file", "list_dir", "glob", "grep"];
    create_ai_agent(
        &paths,
        None,
        &AiAgent {
            name: "da-ia".into(),
            description: "Le codigo e resume.".into(),
            instructions: "Leia e resuma, sem editar nada.".into(),
            ..AiAgent::default()
        },
        &Creator {
            tools: &tools,
            mode: "default",
        },
    )
    .unwrap();

    let mut s = shell_at(dir.path());
    s.load_agents();
    let Load::Ready(list) = &s.agents.list else {
        panic!("agents did not load: {:?}", s.agents.list.is_idle());
    };
    let a = list
        .iter()
        .find(|a| a.name == "da-ia")
        .expect("shown for review");
    assert!(a.disabled && a.created_by_ai);

    // The chat's agent picker and any turn read the enabled list only.
    assert!(zerith_core::agents::find(&paths, None, "da-ia").is_none());

    zerith_core::agents_write::set_disabled(&paths, "da-ia", false).unwrap();
    assert!(zerith_core::agents::find(&paths, None, "da-ia").is_some());
    s.load_agents();
    let Load::Ready(list) = &s.agents.list else {
        panic!("reload failed")
    };
    assert!(!list.iter().find(|a| a.name == "da-ia").unwrap().disabled);
}

// ----------------------------------------------------- a provider error

#[test]
fn a_failed_turn_shows_one_sentence_in_the_chat_and_a_short_footer() {
    let dir = tempdir();
    let mut s = shell_at(dir.path());
    let e = zerith_providers::ProviderError::Http {
        status: 401,
        body: "{\n  \"error\": {\n    \"message\": \"Missing bearer authentication in header\",\n    \"type\": \"invalid_request_error\",\n    \"param\": null,\n    \"code\": null\n  }\n}".into(),
    };
    chat::fail(&mut s, zerith_providers::explain::headline(&e));

    let err = s
        .chat
        .lines
        .iter()
        .find(|l| l.role == "error")
        .expect("the error is in the transcript");
    assert!(
        err.content.contains("Chave de API ausente ou inválida"),
        "{}",
        err.content
    );
    assert!(
        !err.content.contains('{') && !err.content.contains('\n'),
        "{}",
        err.content
    );
    // The footer line next to the composer is one short line, never JSON.
    assert!(s.chat.status.chars().count() <= 81, "{}", s.chat.status);
    assert!(!s.chat.status.contains('\n') && !s.chat.status.contains('{'));
    assert!(!s.chat.busy);
}
