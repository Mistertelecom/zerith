//! The desktop shell: a Tauri window in front of the Rust core.
//!
//! The window owns nothing. Every command here reads or writes through the
//! core crates, so the same rules the CLI applies hold in the GUI.

pub mod agents;
pub mod attachments;
pub mod changes;
pub mod chat;
pub mod commands;
pub mod history;
pub mod learn;
pub mod mcp;
pub mod media;
pub mod memory;
pub mod notify;
pub mod prefs;
pub mod providers;
pub mod skills;
pub mod state;
pub mod turn;
pub mod types;
pub mod update;
pub mod wire;
pub mod workflows;
pub mod workspace;

/// Where the data root lives: `ZERITH_HOME` wins so a test or a second profile
/// never touches the user's database.
pub fn data_root() -> std::path::PathBuf {
    match std::env::var_os("ZERITH_HOME") {
        Some(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => zerith_core::Paths::resolve().root().to_path_buf(),
    }
}

/// Appends one line to `boot.log` beside the exe. The window may never appear,
/// so the console is not enough: when a launch goes wrong, this file says why.
fn boot_log(msg: &str) {
    use std::io::Write;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("boot.log"))
    {
        let _ = writeln!(f, "{secs} pid={} {msg}", std::process::id());
    }
}

/// The skills that ship with the app arrive installed (disabled until the user
/// turns them on). Installed builds carry them as a bundle resource beside the
/// exe; a dev build reads the checkout's folder.
fn seed_bundled_skills(root: &std::path::Path) {
    let bundle = zerith_core::Paths::with_root(root)
        .bundled_skills()
        .or_else(|| {
            let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundled-skills");
            dev.is_dir().then_some(dev)
        });
    let Some(bundle) = bundle else { return };
    match zerith_skills::Library::open(root.join("skills")).and_then(|l| l.seed_disabled(&bundle)) {
        Ok((_, failed)) => failed
            .iter()
            .for_each(|f| eprintln!("zerith-desktop: skill embutida ignorada: {f}")),
        Err(e) => {
            eprintln!("zerith-desktop: não consegui preparar as skills embutidas: {e}");
            boot_log(&format!("skills: {e}"));
        }
    }
}

/// Official Zerith plugins (`zerith-infra`, `zerith-dev`, `zerith-workflows`, `zerith-finance`)
/// arrive installed and enabled by default so their skills and agents are immediately active
/// without manual setup.
fn seed_bundled_plugins(root: &std::path::Path) {
    let bundle = zerith_core::Paths::with_root(root)
        .bundled_plugins()
        .or_else(|| {
            let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
            dev.is_dir().then_some(dev)
        });
    let Some(bundle) = bundle else { return };
    let lib = zerith_plugins::Library::open(&root.join("plugins"));
    // Merged into `zerith-infra` in 0.3.7; left installed they would duplicate its skills.
    if let Ok(l) = &lib {
        for old in [
            "zerith-netops",
            "zerith-proxmox",
            "zerith-noc-observability",
            "zerith-ftth-olt",
            "zerith-linux-devops",
        ] {
            let _ = l.remove(old);
        }
    }
    match lib.and_then(|l| l.seed_enabled(&bundle)) {
        Ok((_, failed)) => failed
            .iter()
            .for_each(|f| eprintln!("zerith-desktop: plugin embutido ignorado: {f}")),
        Err(e) => {
            eprintln!("zerith-desktop: não consegui preparar os plugins embutidos: {e}");
            boot_log(&format!("plugins: {e}"));
        }
    }
}

/// Builds and runs the window.
pub fn run() {
    let root = data_root();
    seed_bundled_skills(&root);
    seed_bundled_plugins(&root);
    // billion-context comes up while the window loads; the first turn waits
    // for it only if it is still starting.
    {
        let paths = zerith_core::Paths::with_root(&root);
        let on = zerith_core::Settings::load(&paths, None)
            .settings
            .context
            .billion_context;
        zerith_runtime::bili::ensure(&root, on, false);
    }
    // WebView2 profile: %LOCALAPPDATA%\Zerith\webview, or whatever
    // `ZERITH_WEBVIEW_DIR` pins. `ZERITH_HOME` may point at a roaming folder,
    // and WebView2 refuses to boot a profile from there (0x8000FFFF).
    #[cfg(target_os = "windows")]
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        let dir = std::env::var_os("ZERITH_WEBVIEW_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|d| std::path::PathBuf::from(d).join("Zerith").join("webview"))
            });
        if let Some(dir) = dir {
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &dir);
        }
    }
    // WebView2 fails with 0x8000FFFF when its profile folder is not usable:
    // a confined process (agent sandbox, restricted token) can only open a
    // profile inside its own tree, and a roaming profile (%APPDATA%) never
    // works. The window builder below walks these in order.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER") {
        candidates.push(std::path::PathBuf::from(d));
    }
    candidates.push(root.join("webview"));
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("webview")))
    {
        candidates.push(d);
    }
    candidates.push(std::env::temp_dir().join("zerith-webview"));
    boot_log(&format!(
        "boot root={} webview={:?}",
        root.display(),
        std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")
    ));
    let scheduler_root = root.clone();
    tauri::Builder::default()
        .setup(move |app| {
            learn::bind(app.handle());
            workflows::spawn_scheduler(app.handle().clone(), scheduler_root);
            let term_handle = app.handle().clone();
            zerith_tools::set_terminal_change_hook(std::sync::Arc::new(move || {
                use tauri::Emitter;
                let _ = term_handle.emit("terminals:changed", ());
            }));
            // The window is built here, not by the config, so a WebView2
            // failure surfaces as a message instead of a panic.
            let cfg = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .ok_or("janela principal ausente da configuração")?
                .clone();
            let build = |app: &tauri::AppHandle| {
                tauri::WebviewWindowBuilder::from_config(app, &cfg)
                    .map_err(|e| format!("configuração inválida: {e}"))
                    .and_then(|b| b.build().map(|_| ()).map_err(|e| e.to_string()))
            };
            let mut last = String::new();
            let mut opened = false;
            for dir in &candidates {
                let _ = std::fs::create_dir_all(dir);
                std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir);
                match build(app.handle()) {
                    Ok(()) => {
                        boot_log(&format!("webview ok em {}", dir.display()));
                        opened = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "zerith-desktop: janela não abriu com {}: {e}",
                            dir.display()
                        );
                        boot_log(&format!("webview falhou em {}: {e}", dir.display()));
                        last = e;
                    }
                }
            }
            if !opened {
                eprintln!("zerith-desktop: sem janela. Último erro: {last}");
            }
            Ok(())
        })
        .manage(state::AppState::new(root))
        .manage(chat::Turns::default())
        .invoke_handler(tauri::generate_handler![
            chat::send_turn,
            providers::oauth_code,
            providers::oauth_import,
            providers::oauth_import_pasted,
            providers::oauth_import_9router,
            providers::account_quota,
            providers::account_models,
            chat::cancel_turn,
            chat::steer_turn,
            chat::answer_approval,
            chat::answer_question,
            commands::boot,
            commands::list_sessions,
            commands::background_jobs,
            commands::background_job_tail,
            commands::background_job_stop,
            commands::terminals_list,
            commands::terminal_tail,
            commands::terminal_interrupt,
            commands::terminal_close,
            commands::terminal_send,
            commands::session_usage,
            commands::create_session,
            commands::session_messages,
            commands::session_fork,
            commands::set_archived,
            commands::rename_session,
            commands::delete_session,
            commands::pinned_sessions,
            commands::set_session_pinned,
            commands::export_session,
            commands::save_prefs,
            commands::providers,
            commands::memory_paused,
            commands::set_memory_paused,
            commands::search_sessions,
            commands::reveal_root,
            commands::set_session_workspace,
            commands::recent_workspaces,
            commands::pick_folder,
            commands::reveal_workspace,
            providers::presets,
            providers::provider_save,
            providers::provider_test,
            providers::accounts,
            providers::account_save,
            providers::account_test,
            providers::account_delete,
            providers::oauth_login,
            providers::oauth_status,
            providers::routing,
            providers::routing_save,
            providers::usage,
            providers::clear_cooldown,
            memory::memory_facts,
            memory::memory_search,
            memory::memory_update,
            memory::memory_forget,
            memory::memory_add,
            memory::memory_import_hermes,
            skills::skills_list,
            skills::slash_skills,
            skills::skills_catalog,
            skills::skills_plugins,
            skills::plugin_action,
            skills::skill_install,
            skills::skills_remote,
            skills::skill_install_remote,
            skills::skill_toggle,
            skills::skill_remove,
            skills::hermes_skills,
            skills::hermes_skills_import,
            mcp::mcp_list,
            mcp::mcp_add,
            mcp::mcp_remove,
            mcp::mcp_toggle,
            mcp::mcp_approve,
            mcp::mcp_test,
            mcp::mcp_hermes_preview,
            mcp::mcp_hermes_import,
            mcp::mcp_oauth_login,
            mcp::mcp_oauth_status,
            mcp::mcp_oauth_logout,
            agents::agents_list,
            agents::agent_create,
            agents::agent_save,
            agents::agent_delete,
            agents::agent_toggle,
            agents::agent_reveal,
            history::sessions_history,
            history::hermes_detect,
            history::hermes_preview,
            history::hermes_import,
            media::media_models,
            media::media_key,
            media::media_save_key,
            media::media_settings,
            media::media_save_slot,
            media::media_generate,
            media::media_open,
            media::media_open_dir,
            prefs::token_saver,
            prefs::set_token_saver,
            prefs::long_context,
            prefs::set_long_context,
            prefs::install_billion_context,
            prefs::storage,
            prefs::backup,
            prefs::profile_limits,
            workspace::workspace_files,
            workspace::checkpoints,
            workspace::rewind,
            workspace::compact,
            workspace::read_image,
            workspace::open_image,
            workspace::open_url,
            workspace::open_in_editor,
            workspace::git_branch,
            attachments::save_attachment,
            attachments::save_file_attachment,
            attachments::attach_dropped_path,
            changes::turn_changes,
            changes::turn_undo,
            learn::learning,
            learn::set_learning,
            learn::learn_undo,
            workflows::workflow_list,
            workflows::workflow_save,
            workflows::workflow_delete,
            workflows::workflow_toggle,
            workflows::workflow_run_now,
            workflows::workflow_runs,
            workflows::workflow_generate_from_prompt,
            workflows::workflow_templates,
            workflows::workflow_test_node,
            workflows::workflow_webhook_base_url,
            workflows::channel_list,
            workflows::channel_save,
            workflows::channel_delete,
            workflows::channel_test,
            update::update_check,
            update::update_install,
        ])
        .build(tauri::generate_context!())
        .expect("erro ao preparar a janela do Zerith")
        .run(|app, event| {
            // A background job outlives its turn, not the app: closing the
            // window stops whatever is still running.
            if matches!(event, tauri::RunEvent::Exit) {
                zerith_tools::kill_all_jobs();
                zerith_tools::close_all_terminals();
                zerith_tools::lsp::shutdown_all();
                use tauri::Manager;
                let state = app.state::<state::AppState>();
                state.invalidate_mcp_cache();
                state.reset_store();
            }
        });
}
