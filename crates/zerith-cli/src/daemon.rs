//! Cron scheduler CLI (`zerith schedule ...`) and background polling daemon
//! (`zerith daemon [--once] [--poll-secs N]`).
//!
//! `no_agent` schedules execute shell commands directly with zero LLM tokens and
//! zero provider calls; agent schedules ask approvals in the Telegram alert
//! chat when the gateway is configured (denied after 10 min without answer)
//! and run fail-closed (`approvals: None`) otherwise.
//! Failed jobs, agent responses starting with `"ALERTA:"`, agent turns cut at
//! the step limit and jobs longer than [`LONG_JOB`] dispatch a short Telegram
//! summary (name, status, duration, first lines) when `gateway.telegram` is
//! configured. Ideia: notificação de fim de tarefa do Claude Code e `notify` do Codex.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zerith_core::{Paths, Settings};
use zerith_mcp::McpRegistry;
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::guards::Guards;
use zerith_runtime::{approval_channel, run_agent_turn, AgentOptions, RunEvent, TurnContext};
use zerith_security::Policy;
use zerith_sessions::migrations::now_ms;
use zerith_sessions::schedules::{CronExpr, NewSchedule, Schedule};
use zerith_sessions::{SessionSource, Store};
use zerith_skills::Library;
use zerith_tools::Registry;

use crate::headless::{provider_config, resolve_model};
use crate::{signals, telegram};

/// Single-instance PID lock file used by long-running daemons (`daemon`, `gateway`).
pub(crate) struct PidLock {
    path: PathBuf,
}

impl PidLock {
    pub fn acquire(path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let pid = std::process::id();
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{pid}");
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let old_pid = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    if let Some(p) = old_pid.filter(|&p| pid_alive(p)) {
                        return Err(format!(
                            "outra instância já está ativa (PID {p}, lock em {})",
                            path.display()
                        ));
                    }
                    // Stale/dead lock file: clear and retry once.
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(format!("falha ao criar lock {}: {e}", path.display())),
            }
        }
        Err(format!("não consegui adquirir lock em {}", path.display()))
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH", "/FI", &format!("PID eq {pid}")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| {
                let txt = String::from_utf8_lossy(&o.stdout);
                txt.contains(&format!(",{pid},")) || txt.contains(&format!("\"{pid}\""))
            })
            .unwrap_or(false)
    }
}

/// Returns `true` when `schedule` is enabled, its cron matches the current
/// UTC minute (`now_secs`), and it has not already executed in that minute.
#[cfg(test)]
pub fn is_schedule_due(schedule: &Schedule, now_secs: i64) -> bool {
    let m = now_secs.div_euclid(60);
    is_schedule_due_in(schedule, m, m)
}

/// Missed minutes the daemon replays after a stall/sleep; older ones are dropped.
const MAX_CATCHUP_MINUTES: i64 = 15;

/// Like [`is_schedule_due`] over every UTC minute in `from_min..=to_min`
/// (epoch minutes): true once if any minute after the last run matches, so
/// several missed slots collapse into one run.
pub fn is_schedule_due_in(schedule: &Schedule, from_min: i64, to_min: i64) -> bool {
    if !schedule.enabled {
        return false;
    }
    let Ok(expr) = CronExpr::parse(&schedule.cron_expr) else {
        return false;
    };
    let after_last = schedule
        .last_run_at
        .map_or(i64::MIN, |ms| ms.div_euclid(60_000) + 1);
    (from_min.max(after_last)..=to_min).any(|m| expr.matches_unix_secs(m * 60))
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
            let timeout_secs = parse_timeout_flag(&args[3..])?;
            let s = store
                .schedule_get(target)?
                .ok_or_else(|| format!("tarefa não encontrada: {target}"))?;
            let cancel = Cancel::default();
            signals::install(cancel.clone());
            let (status, out) = run_one_schedule(paths, &store, &s, &cancel, timeout_secs);
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

const DEFAULT_CMD_TIMEOUT_SECS: u64 = 120;

fn parse_timeout_flag(args: &[String]) -> Result<u64, Box<dyn std::error::Error>> {
    let mut timeout_secs = DEFAULT_CMD_TIMEOUT_SECS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout-secs" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--timeout-secs precisa de um número")?;
                timeout_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("--timeout-secs inválido: {v}"))?
                    .clamp(10, 3600);
                i += 2;
            }
            other => return Err(format!("opção desconhecida em schedule run: {other}").into()),
        }
    }
    Ok(timeout_secs)
}

pub fn daemon_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut once = false;
    let mut poll_secs: u64 = 20;
    let mut timeout_secs: u64 = DEFAULT_CMD_TIMEOUT_SECS;

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
            "--timeout-secs" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--timeout-secs precisa de um número")?;
                timeout_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("--timeout-secs inválido: {v}"))?
                    .clamp(10, 3600);
                i += 2;
            }
            other => return Err(format!("opção desconhecida em daemon: {other}").into()),
        }
    }

    paths.ensure()?;
    let _instance_lock = if !once {
        Some(PidLock::acquire(paths.root().join("daemon.lock"))?)
    } else {
        None
    };
    let store = Store::open(paths.db())?;
    let cancel = Cancel::default();
    signals::install(cancel.clone());

    if !once {
        eprintln!(
            "zerith: daemon de agendamentos iniciado (poll={}s, timeout={}s)",
            poll_secs, timeout_secs
        );
    }

    // Jobs run on their own threads so one slow job cannot delay the others;
    // `running` keeps a job from overlapping with itself.
    let running: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut last_min: Option<i64> = None;
    loop {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let now_min = now_secs.div_euclid(60);
        let from_min = match last_min {
            Some(prev) if prev < now_min => (prev + 1).max(now_min - MAX_CATCHUP_MINUTES),
            _ => now_min,
        };
        last_min = Some(now_min);

        match store.schedule_list() {
            Ok(items) => {
                for s in items {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if !is_schedule_due_in(&s, from_min, now_min) {
                        continue;
                    }
                    if !running
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(s.id.clone())
                    {
                        continue;
                    }
                    eprintln!(
                        "zerith: executando agendamento `{}` ({})",
                        s.name, s.cron_expr
                    );
                    let (paths, cancel, running) = (paths.clone(), cancel.clone(), running.clone());
                    handles.push(std::thread::spawn(move || {
                        struct Done(Arc<Mutex<HashSet<String>>>, String);
                        impl Drop for Done {
                            fn drop(&mut self) {
                                self.0
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .remove(&self.1);
                            }
                        }
                        let _done = Done(running, s.id.clone());
                        let status = match Store::open(paths.db()) {
                            Ok(st) => run_one_schedule(&paths, &st, &s, &cancel, timeout_secs).0,
                            Err(e) => {
                                eprintln!("zerith: erro ao abrir banco: {e}");
                                "error"
                            }
                        };
                        eprintln!("zerith: agendamento `{}` finalizado: {status}", s.name);
                    }));
                }
            }
            Err(e) => eprintln!("zerith: erro ao listar agendamentos: {e}"),
        }
        handles.retain(|h| !h.is_finished());
        maybe_curate_memory(paths, &store);

        if once || cancel.is_cancelled() {
            for h in handles {
                let _ = h.join();
            }
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

/// Quando a curadoria da memória rodou pela última vez (ms), na tabela `settings`.
const CURATE_KEY: &str = "memory.curate.last_run";

/// Curadoria periódica (`[memory] curate_every_days`, 0 = desligada): a cada
/// tantos dias monta o plano de `zerith memory curate` e só avisa (stderr e
/// Telegram); nunca aplica. Intervalo como o do curador do Hermes (7 dias;
/// MIT).
fn maybe_curate_memory(paths: &Paths, store: &Store) {
    let cwd = std::env::current_dir().ok();
    let settings = Settings::load(paths, cwd.as_deref()).settings;
    let now = now_ms();
    let last = store
        .setting(CURATE_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok());
    if !curate_due(settings.memory.curate_every_days, last, now) {
        return;
    }
    let _ = store.set_setting(CURATE_KEY, &now.to_string());
    let plan =
        match SqliteMemory::open(paths.db()).and_then(|m| zerith_memory::curate::plan(&m, now)) {
            Ok(p) => p,
            Err(e) => return eprintln!("zerith: curadoria da memória falhou: {e}"),
        };
    if plan.is_empty() {
        return;
    }
    let msg = format!(
        "Curadoria da memória: {} fusão(ões) e {} arquivamento(s) propostos. Revise com `zerith memory curate` ou em Memória > Revisar memória; nada foi aplicado.",
        plan.merges.len(),
        plan.archives.len()
    );
    eprintln!("zerith: {msg}");
    maybe_alert_telegram(&settings, &msg);
}

/// Se a curadoria deve rodar agora: ligada e, sem execução anterior ou com a
/// última há `every_days` dias ou mais.
fn curate_due(every_days: u32, last_ms: Option<i64>, now_ms: i64) -> bool {
    every_days > 0 && last_ms.is_none_or(|l| now_ms - l >= i64::from(every_days) * 86_400_000)
}

/// Tarefa que passa disso avisa ao terminar mesmo dando certo: quem agendou
/// algo demorado quer saber que acabou. As curtas e bem-sucedidas ficam quietas
/// (um cron de minuto em minuto encheria o chat), a não ser que digam `ALERTA:`.
const LONG_JOB: Duration = Duration::from_secs(120);

/// Linhas e caracteres da resposta que vão no aviso.
const ALERT_LINES: usize = 12;
const ALERT_CHARS: usize = 1500;

/// `1h02m`, `3m05s`, `42s`.
fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, m, sec) => format!("{m}m{sec:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

/// Aviso do fim de uma tarefa: cabeçalho com nome, status e duração, e as
/// primeiras linhas da resposta.
fn format_alert(
    schedule_name: &str,
    status: &str,
    stop_reason: Option<&str>,
    elapsed: Duration,
    body: &str,
) -> String {
    let mut head = format!(
        "[zerith daemon · {schedule_name}] {status} em {}",
        fmt_duration(elapsed)
    );
    if stop_reason == Some("max_steps") {
        head.push_str(" · parou no limite de passos (tarefa incompleta)");
    }
    let lines: Vec<&str> = body.trim().lines().take(ALERT_LINES + 1).collect();
    let mut preview = lines[..lines.len().min(ALERT_LINES)].join("\n");
    let mut cut = lines.len() > ALERT_LINES;
    if preview.chars().count() > ALERT_CHARS {
        preview = preview.chars().take(ALERT_CHARS).collect();
        cut = true;
    }
    if cut {
        preview.push_str("\n…");
    }
    if preview.trim().is_empty() {
        head
    } else {
        format!("{head}\n\n{preview}")
    }
}

fn maybe_alert_telegram(settings: &Settings, msg: &str) {
    if !settings.gateway.telegram.enabled || settings.gateway.telegram.allowed_chats.is_empty() {
        return;
    }
    if let Err(e) = telegram::send_telegram_alert(settings, msg) {
        eprintln!("zerith: falha ao enviar alerta Telegram: {e}");
    }
}

fn run_command_with_timeout(
    mut proc: std::process::Command,
    timeout: Duration,
    cancel: &Cancel,
) -> std::result::Result<(bool, String), String> {
    proc.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = proc
        .spawn()
        .map_err(|e| format!("falha ao iniciar comando: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_t = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("execução cancelada".to_string());
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("timeout após {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("falha aguardando processo: {e}"));
            }
        }
    };

    let out_bytes = out_t.join().unwrap_or_default();
    let err_bytes = err_t.join().unwrap_or_default();
    let mut combined = String::from_utf8_lossy(&out_bytes).into_owned();
    let err_txt = String::from_utf8_lossy(&err_bytes);
    if !err_txt.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&err_txt);
    }

    match status {
        Ok(st) => {
            if st.success() {
                Ok((true, combined))
            } else {
                let code = st
                    .code()
                    .map_or_else(|| "sinal".to_string(), |c| c.to_string());
                Ok((false, format!("exit {code}\n{combined}")))
            }
        }
        Err(why) => Err(if combined.trim().is_empty() {
            why
        } else {
            format!("{why}\n{combined}")
        }),
    }
}

fn run_one_schedule(
    paths: &Paths,
    store: &Store,
    s: &Schedule,
    cancel: &Cancel,
    timeout_secs: u64,
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

    let started = Instant::now();
    let mut stop_reason = None;
    let (status, output, mut should_alert) = if s.no_agent {
        let Some(cmd) = s.command.as_deref().filter(|c| !c.trim().is_empty()) else {
            let msg = "tarefa no_agent sem command configurado".to_string();
            let _ = store.schedule_record_run(&s.id, now_ms(), "error", &msg);
            return ("error", msg);
        };
        let mut proc = zerith_tools::shell_command(cmd);
        proc.current_dir(&ws);
        match run_command_with_timeout(proc, Duration::from_secs(timeout_secs), cancel) {
            Ok((true, combined)) => ("ok", combined, false),
            Ok((false, msg)) | Err(msg) => ("error", msg, true),
        }
    } else {
        match run_agent_schedule(paths, store, &settings, contrib.as_ref(), s, &ws, cancel) {
            Ok((text, stop)) => {
                let alert = text.trim_start().starts_with("ALERTA:") || stop == "max_steps";
                stop_reason = Some(stop);
                ("ok", text, alert)
            }
            Err(e) => ("error", e, true),
        }
    };

    if let Err(e) = store.schedule_record_run(&s.id, now_ms(), status, &output) {
        eprintln!("zerith: falha ao registrar execução da tarefa: {e}");
    }
    let elapsed = started.elapsed();
    should_alert |= elapsed >= LONG_JOB;
    if should_alert {
        let msg = format_alert(&s.name, status, stop_reason, elapsed, &output);
        maybe_alert_telegram(&settings, &msg);
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
) -> Result<(String, &'static str), String> {
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

    // Com Telegram configurado, o pedido vai ao chat e espera a decisão
    // (expira negando); sem ele, fail closed como antes.
    let tg = &settings.gateway.telegram;
    let relay = (tg.enabled && !tg.allowed_chats.is_empty()).then(approval_channel);
    let opts = AgentOptions {
        tools: Some(&tools),
        policy: Some(&policy),
        approvals: relay.as_ref().map(|(_, gate)| gate),
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
        |e| match e {
            RunEvent::ApprovalNeeded {
                id,
                tool,
                summary,
                always,
            } => {
                if let Some((approver, _)) = &relay {
                    let d = telegram::ask_schedule_approval(
                        paths,
                        settings,
                        &s.name,
                        &tool,
                        &summary,
                        always.as_deref(),
                        cancel,
                    );
                    approver.answer(id, d);
                }
            }
            RunEvent::Notice { text } => maybe_alert_telegram(settings, &text),
            _ => {}
        },
    )
    .map_err(|e| e.to_string())?;

    Ok((report.text, report.stop_reason))
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

    #[test]
    fn memory_curation_is_off_by_default_and_runs_every_n_days() {
        let day = 86_400_000;
        assert_eq!(Settings::default().memory.curate_every_days, 0);
        assert!(!curate_due(0, None, 10 * day));
        assert!(curate_due(7, None, 10 * day));
        assert!(!curate_due(7, Some(5 * day), 11 * day));
        assert!(curate_due(7, Some(4 * day), 11 * day));
    }

    #[test]
    fn missed_minutes_are_caught_up_once() {
        // 2026-11-20 12:30:00 UTC matches */5; daemon stalled from 12:29 to 12:33.
        let m = 1_795_177_800i64 / 60;
        let s = mk_schedule("*/5 * * * *", true, None);
        assert!(is_schedule_due_in(&s, m - 1, m + 3));
        assert!(!is_schedule_due_in(&s, m + 1, m + 3));
        // Already ran at 12:30: replaying the same window does not fire again.
        let ran = mk_schedule("*/5 * * * *", true, Some(m * 60_000 + 5_000));
        assert!(!is_schedule_due_in(&ran, m - 1, m + 3));
        assert!(!is_schedule_due_in(
            &mk_schedule("*/5 * * * *", false, None),
            m,
            m
        ));
    }

    #[test]
    fn pid_lock_blocks_duplicate_recovers_stale_and_cleans_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "zerith-pidlock-{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("daemon.lock");

        // Stale non-numeric lock recovers transparently.
        std::fs::write(&lock_path, "stale-garbage").unwrap();
        {
            let _guard = PidLock::acquire(lock_path.clone()).unwrap();
            assert!(lock_path.exists());
            // Active lock in same process must fail.
            assert!(PidLock::acquire(lock_path.clone()).is_err());
        }
        // Dropped -> removed cleanly.
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alert_has_name_status_duration_stop_reason_and_first_lines() {
        assert_eq!(fmt_duration(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_duration(Duration::from_secs(185)), "3m05s");
        assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h02m");

        let msg = format_alert(
            "backup",
            "ok",
            Some("done"),
            Duration::from_secs(185),
            "\nTudo certo.\nSegunda linha.\n",
        );
        assert_eq!(
            msg,
            "[zerith daemon · backup] ok em 3m05s\n\nTudo certo.\nSegunda linha."
        );

        let body: String = (1..=20).map(|i| format!("linha {i}\n")).collect();
        let msg = format_alert(
            "rel",
            "ok",
            Some("max_steps"),
            Duration::from_secs(9),
            &body,
        );
        assert!(msg.contains("limite de passos"), "{msg}");
        assert!(msg.contains("linha 12\n…"), "{msg}");
        assert!(!msg.contains("linha 13"), "{msg}");

        let msg = format_alert("x", "error", None, Duration::ZERO, &"é".repeat(3000));
        assert!(
            msg.starts_with("[zerith daemon · x] error em 0s\n\n"),
            "{msg}"
        );
        assert!(msg.chars().count() < ALERT_CHARS + 60);
        assert!(msg.ends_with('…'));

        assert_eq!(
            format_alert("y", "ok", None, Duration::from_secs(1), "  "),
            "[zerith daemon · y] ok em 1s"
        );
    }

    #[test]
    fn command_timeout_kills_slow_process() {
        let cmd = if cfg!(windows) {
            "ping -n 6 127.0.0.1"
        } else {
            "sleep 5"
        };
        let proc = zerith_tools::shell_command(cmd);
        let cancel = Cancel::default();
        let err = run_command_with_timeout(proc, Duration::from_millis(200), &cancel).unwrap_err();
        assert!(err.contains("timeout"), "{err}");
    }
}
