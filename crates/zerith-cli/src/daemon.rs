//! Cron scheduler CLI (`zerith schedule ...`) and background polling daemon
//! (`zerith daemon [--once] [--poll-secs N]`).
//!
//! `no_agent` schedules execute shell commands directly with zero LLM tokens and
//! zero provider calls; agent schedules run fail-closed (`approvals: None`).
//! Failed jobs or agent responses starting with `"ALERTA:"` dispatch Telegram
//! notifications when `gateway.telegram` is configured.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zerith_core::{Paths, Settings};
use zerith_mcp::McpRegistry;
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::guards::Guards;
use zerith_runtime::{run_agent_turn, AgentOptions, TurnContext};
use zerith_security::Policy;
use zerith_sessions::migrations::now_ms;
use zerith_sessions::schedules::{CronExpr, NewSchedule, Schedule};
use zerith_sessions::{SessionSource, Store};
use zerith_skills::Library;
use zerith_tools::Registry;

use crate::headless::{provider_config, resolve_model};
use crate::{signals, telegram};

/// Returns `true` when `schedule` is enabled, its cron matches the current
/// UTC minute (`now_secs`), and it has not already executed in that minute.
pub fn is_schedule_due(schedule: &Schedule, now_secs: i64) -> bool {
    if !schedule.enabled {
        return false;
    }
    let Ok(expr) = CronExpr::parse(&schedule.cron_expr) else {
        return false;
    };
    if let Some(last_ms) = schedule.last_run_at {
        if last_ms.div_euclid(60_000) == now_secs.div_euclid(60) {
            return false;
        }
    }
    expr.matches_unix_secs(now_secs)
}

pub fn schedule_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    match args.get(1).map(String::as_str) {
        Some("add") => schedule_add(&store, &args[2..]),
        Some("list") | None => schedule_list(&store),
        Some("remove") | Some("rm") | Some("delete") => {
            let target = args.get(2).ok_or("uso: zerith schedule remove <id|nome>")?;
            if store.schedule_remove(target)? {
                println!("Tarefa removida: {target}");
                Ok(())
            } else {
                Err(format!("tarefa não encontrada: {target}").into())
            }
        }
        Some("toggle") => {
            let target = args
                .get(2)
                .ok_or("uso: zerith schedule toggle <id|nome> <on|off>")?;
            let raw_state = args
                .get(3)
                .ok_or("uso: zerith schedule toggle <id|nome> <on|off>")?;
            let enabled = match raw_state.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "enable" | "ativo" => true,
                "off" | "false" | "0" | "disable" | "pausado" => false,
                other => {
                    return Err(format!("estado inválido: {other} (use on ou off)").into());
                }
            };
            if store.schedule_set_enabled(target, enabled)? {
                println!(
                    "Tarefa `{target}` {}.",
                    if enabled { "ativada" } else { "pausada" }
                );
                Ok(())
            } else {
                Err(format!("tarefa não encontrada: {target}").into())
            }
        }
        Some("run") => {
            let target = args.get(2).ok_or("uso: zerith schedule run <id|nome>")?;
            let s = store
                .schedule_get(target)?
                .ok_or_else(|| format!("tarefa não encontrada: {target}"))?;
            let cancel = Cancel::default();
            signals::install(cancel.clone());
            let (status, out) = run_one_schedule(paths, &store, &s, &cancel);
            println!("[{status}] {}\n{out}", s.name);
            if status == "ok" {
                Ok(())
            } else {
                Err(format!("tarefa `{}` falhou", s.name).into())
            }
        }
        Some(other) => Err(format!(
            "subcomando schedule desconhecido: {other} (use add|list|remove|toggle|run)"
        )
        .into()),
    }
}

fn schedule_add(store: &Store, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut name: Option<String> = None;
    let mut cron: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut command: Option<String> = None;
    let mut no_agent = false;
    let mut workspace: Option<String> = None;
    let mut model: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--no-agent" {
            no_agent = true;
            i += 1;
            continue;
        }
        let next = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{a} precisa de um valor"))
        };
        match a {
            "--cron" => {
                cron = Some(next()?);
                i += 2;
            }
            "--prompt" => {
                prompt = Some(next()?);
                i += 2;
            }
            "--command" | "--cmd" => {
                command = Some(next()?);
                i += 2;
            }
            "--workspace" | "-C" => {
                workspace = Some(next()?);
                i += 2;
            }
            "--model" => {
                model = Some(next()?);
                i += 2;
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other.to_string());
                i += 1;
            }
            other => {
                return Err(format!("opção desconhecida em schedule add: {other}").into());
            }
        }
    }

    let name = name.ok_or("uso: zerith schedule add <nome> --cron \"...\" (--command \"...\" --no-agent | --prompt \"...\")")?;
    let cron_expr = cron.ok_or("--cron \"<min hora dom mes dow>\" é obrigatório")?;
    CronExpr::parse(&cron_expr).map_err(|e| format!("expressão cron inválida: {e}"))?;

    if command.is_some() && prompt.is_some() {
        return Err("informe só --command ou --prompt, não ambos".into());
    }
    if command.is_some() {
        no_agent = true;
    } else if no_agent {
        return Err("--no-agent exige --command \"<cmd>\"".into());
    } else if prompt.is_none() {
        return Err("informe --command \"...\" --no-agent ou --prompt \"...\"".into());
    }

    let ws = match workspace {
        Some(w) => {
            let p = PathBuf::from(w);
            p.canonicalize().unwrap_or(p).to_string_lossy().into_owned()
        }
        None => std::env::current_dir()?.to_string_lossy().into_owned(),
    };

    let created = store.schedule_add(&NewSchedule {
        name,
        cron_expr,
        workspace: ws,
        prompt,
        command,
        no_agent,
        model,
        enabled: true,
    })?;

    println!(
        "Tarefa agendada: {} ({}) cron=`{}` modo={}",
        created.id,
        created.name,
        created.cron_expr,
        if created.no_agent {
            "no-agent"
        } else {
            "agent"
        }
    );
    Ok(())
}

fn schedule_list(store: &Store) -> Result<(), Box<dyn std::error::Error>> {
    let list = store.schedule_list()?;
    if list.is_empty() {
        println!("(nenhuma tarefa agendada)");
        return Ok(());
    }
    for s in list {
        let short: String = s.id.chars().take(8).collect();
        let state = if s.enabled { "ativo  " } else { "pausado" };
        let mode = if s.no_agent { "cmd  " } else { "agent" };
        let last = s.last_status.as_deref().unwrap_or("-");
        println!(
            "{short}  {state}  {mode}  [{}]  {}  ultimo={last}",
            s.cron_expr, s.name
        );
    }
    Ok(())
}

pub fn daemon_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut once = false;
    let mut poll_secs: u64 = 20;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => {
                once = true;
                i += 1;
            }
            "--poll-secs" => {
                let v = args.get(i + 1).ok_or("--poll-secs precisa de um número")?;
                poll_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("--poll-secs inválido: {v}"))?
                    .clamp(5, 60);
                i += 2;
            }
            other => return Err(format!("opção desconhecida em daemon: {other}").into()),
        }
    }

    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let cancel = Cancel::default();
    signals::install(cancel.clone());

    if !once {
        eprintln!(
            "zerith: daemon de agendamentos iniciado (poll={}s)",
            poll_secs
        );
    }

    loop {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        match store.schedule_list() {
            Ok(items) => {
                for s in items {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if is_schedule_due(&s, now_secs) {
                        eprintln!(
                            "zerith: executando agendamento `{}` ({})",
                            s.name, s.cron_expr
                        );
                        let (status, _) = run_one_schedule(paths, &store, &s, &cancel);
                        eprintln!("zerith: agendamento `{}` finalizado: {status}", s.name);
                    }
                }
            }
            Err(e) => eprintln!("zerith: erro ao listar agendamentos: {e}"),
        }

        if once || cancel.is_cancelled() {
            break;
        }
        for _ in 0..poll_secs {
            if cancel.is_cancelled() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    Ok(())
}

fn maybe_alert_telegram(settings: &Settings, schedule_name: &str, status: &str, body: &str) {
    if !settings.gateway.telegram.enabled || settings.gateway.telegram.allowed_chats.is_empty() {
        return;
    }
    let preview: String = body.chars().take(1500).collect();
    let msg = format!("[zerith daemon · {schedule_name}] status={status}\n\n{preview}");
    if let Err(e) = telegram::send_telegram_alert(settings, &msg) {
        eprintln!("zerith: falha ao enviar alerta Telegram: {e}");
    }
}

fn run_one_schedule(
    paths: &Paths,
    store: &Store,
    s: &Schedule,
    cancel: &Cancel,
) -> (&'static str, String) {
    let ws = if s.workspace.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf())
    } else {
        PathBuf::from(&s.workspace)
    };
    let loaded = Settings::load(paths, Some(&ws));
    let mut settings = loaded.settings;
    let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }

    let (status, output, should_alert) = if s.no_agent {
        let Some(cmd) = s.command.as_deref().filter(|c| !c.trim().is_empty()) else {
            let msg = "tarefa no_agent sem command configurado".to_string();
            let _ = store.schedule_record_run(&s.id, now_ms(), "error", &msg);
            return ("error", msg);
        };
        let mut proc = zerith_tools::shell_command(cmd);
        proc.current_dir(&ws).stdin(Stdio::null());
        match proc.output() {
            Ok(out) => {
                let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
                let err_txt = String::from_utf8_lossy(&out.stderr);
                if !err_txt.trim().is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&err_txt);
                }
                if out.status.success() {
                    ("ok", combined, false)
                } else {
                    let code = out
                        .status
                        .code()
                        .map_or_else(|| "sinal".to_string(), |c| c.to_string());
                    let msg = format!("exit {code}\n{combined}");
                    ("error", msg, true)
                }
            }
            Err(e) => ("error", format!("falha ao iniciar comando: {e}"), true),
        }
    } else {
        match run_agent_schedule(paths, store, &settings, contrib.as_ref(), s, &ws, cancel) {
            Ok(text) => {
                let alert = text.trim_start().starts_with("ALERTA:");
                ("ok", text, alert)
            }
            Err(e) => ("error", e, true),
        }
    };

    if let Err(e) = store.schedule_record_run(&s.id, now_ms(), status, &output) {
        eprintln!("zerith: falha ao registrar execução da tarefa: {e}");
    }
    if should_alert {
        maybe_alert_telegram(&settings, &s.name, status, &output);
    }
    (status, output)
}

fn run_agent_schedule(
    paths: &Paths,
    store: &Store,
    settings: &Settings,
    contrib: Option<&zerith_plugins::Contributions>,
    s: &Schedule,
    ws: &std::path::Path,
    cancel: &Cancel,
) -> Result<String, String> {
    let prompt = s
        .prompt
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "tarefa agent sem prompt configurado".to_string())?;

    let cfg = provider_config(settings).map_err(|e| e.to_string())?;
    let model = resolve_model(s.model.clone(), settings, &cfg)
        .ok_or_else(|| "sem modelo configurado para tarefa agendada".to_string())?;
    let client = Client::new(cfg).map_err(|e| e.to_string())?;

    let session = store
        .create_session(&format!("schedule: {}", s.name), SessionSource::Native)
        .map_err(|e| e.to_string())?;

    let mut policy = Policy::from_settings(settings, ws).map_err(|e| e.to_string())?;
    let mut tools = Registry::new();
    let mut mcp = McpRegistry::load(paths, Some(ws));
    if let Some(c) = contrib {
        mcp.extend(c.mcp.iter().map(|(_, srv)| srv.clone()).collect());
    }
    let _ = mcp.register_into(&mut tools);

    let guards = Guards::new(settings.clone(), &mut policy, session.id.clone());
    tools.register(guards.enter_plan_mode_tool());
    tools.register(guards.exit_plan_mode_tool());

    let mem_db = SqliteMemory::open(paths.db()).map_err(|e| e.to_string())?;
    let scope = Scope::workspace(ws);
    let memory = (!settings.memory.paused).then_some(&mem_db as &dyn MemoryBackend);
    let skills = Library::open(paths.skills()).ok().map(|l| {
        if let Some(c) = contrib {
            l.with_extra(c.skill_dirs())
        } else {
            l
        }
    });

    let opts = AgentOptions {
        tools: Some(&tools),
        policy: Some(&policy),
        approvals: None, // Fail closed in unattended cron execution.
        middleware: vec![Box::new(guards)],
        spill_dir: Some(paths.root().join("spill")),
        ..AgentOptions::for_profile(zerith_runtime::active_profile())
    };

    let report = run_agent_turn(
        store,
        &client,
        TurnContext {
            memory,
            scope: Some(&scope),
            skills: skills.as_ref(),
            settings: Some(settings),
            project_dir: Some(ws),
            ..TurnContext::default()
        },
        opts,
        &session.id,
        &model,
        prompt,
        cancel,
        |_| {},
    )
    .map_err(|e| e.to_string())?;

    Ok(report.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_schedule(cron: &str, enabled: bool, last_run_at: Option<i64>) -> Schedule {
        Schedule {
            id: "sch-1".into(),
            name: "check-links".into(),
            cron_expr: cron.into(),
            workspace: "/tmp".into(),
            prompt: None,
            command: Some("echo ok".into()),
            no_agent: true,
            model: None,
            enabled,
            last_run_at,
            last_status: None,
            last_output: None,
            created_at: 0,
        }
    }

    #[test]
    fn is_schedule_due_handles_enabled_dedup_next_minute_and_invalid_cron() {
        // 2026-11-20 12:30:00 UTC
        let ts = 1_795_177_800i64;
        assert!(is_schedule_due(&mk_schedule("*/5 * * * *", true, None), ts));
        assert!(!is_schedule_due(
            &mk_schedule("*/5 * * * *", false, None),
            ts
        ));
        assert!(!is_schedule_due(&mk_schedule("bad-cron", true, None), ts));
        // Already ran inside the same UTC minute (ts * 1000 + 25_000 ms).
        assert!(!is_schedule_due(
            &mk_schedule("*/5 * * * *", true, Some(ts * 1000 + 25_000)),
            ts + 45
        ));
        // Runs again once the next matching minute arrives (ts + 300s).
        assert!(is_schedule_due(
            &mk_schedule("*/5 * * * *", true, Some(ts * 1000)),
            ts + 300
        ));
        // Minute 31 does not match */5.
        assert!(!is_schedule_due(
            &mk_schedule("*/5 * * * *", true, None),
            ts + 60
        ));
    }
}
