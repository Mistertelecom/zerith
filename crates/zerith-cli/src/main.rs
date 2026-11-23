//! `zerith` — the binary. Phase 1 surface: version, paths, sessions, memory.
//!
//! Arguments are parsed by hand: six subcommands do not justify a dependency,
//! and an agent harness that shells out to itself should start fast.

mod args;
mod cmds;
mod daemon;
mod headless;
mod media_cmd;
mod schema;
mod signals;
mod telegram;

use std::process::ExitCode;
use zerith_core::{secrets, Paths, Settings};
use zerith_importers::{detect, import, import_jsonl, ImportReport};
use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderError};
use zerith_runtime::guards::Guards;
use zerith_runtime::plan::{parse_plan, run_plan};
use zerith_runtime::{run_agent_turn, AgentOptions, RunError, RunEvent, TurnContext, TurnReport};
use zerith_security::{redact, Action, Policy, Verdict, PRESETS};
use zerith_sessions::{NewMessage, Role, SessionSource, Store, TaskState};
use zerith_skills::catalog;
use zerith_skills::{discover, scan, Library, Status};
use zerith_tools::{CallCtx, Registry};

use crate::args::{
    parse_chat, parse_import_hermes, parse_rename, parse_repl, select_session, SessionArgs, Target,
};
use crate::headless::{load_settings, provider_config, resolve_model, ExitWith};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // An error string can carry a response body, and a response body
            // can carry the key that was sent. Redact on the way out.
            eprintln!("zerith: {}", redact(&e.to_string()));
            // A signal that stopped the run decides the exit code (130/143).
            let code = e.downcast_ref::<ExitWith>().map_or(1, |x| x.0);
            ExitCode::from(code)
        }
    }
}

fn usage() -> &'static str {
    "zerith — native AI agent harness

usage:
  zerith version
  zerith paths
  zerith doctor
  zerith policy [check <path>]
  zerith bench
  zerith backup [--to <file>]
  zerith restore <file>
  zerith session list [--limit N]
  zerith session new <title>
  zerith session show <id|name>
  zerith session rename <id|name> <new title>
  zerith export <id|name> [--format md|jsonl] [--out FILE]
  zerith search <query> [--limit N]
  zerith checkpoints <id|name>
  zerith rewind <id|name> [--turn N] [--code|--conversation|--both] [--purge]
  zerith memory remember <scope> <text> [--inferred]
  zerith memory recall <scope> <query>
  zerith memory list <scope>
  zerith memory forget <id>
  zerith memory edit <id> <text>
  zerith memory export [scope]
  zerith memory import <file.jsonl>
  zerith memory pause|resume|status
  zerith skills list|installed|catalog [--refresh]
  zerith skills install <dir> | remove <name> | enable|disable <name>
  zerith skills update <name> <dir> | rollback <name>
  zerith skills validate <dir>
  zerith tools [list|call <tool> [json-args]]
  zerith agents list
  zerith plugin list|show <name>|install <dir>|remove|enable|disable|untrust <name>
  zerith plugin trust <name> --yes | tools <name>
  zerith plan run <file|-> [--agents N] [--model id]
  zerith plan list
  zerith mcp list|add <name> (-- <cmd..> | --url <url>)|remove <name>
  zerith mcp approve <name> --yes
  zerith mcp tools <server> | call <server> <tool> [json-args]
  zerith mcp tools <command and args>            (raw server, no config)
  zerith providers [show|probe|models|chat <model> <prompt>]
  zerith usage [--hours N]
  zerith quota [--hours N]
  zerith accounts [list|add <id> <provider> ...|remove|enable|disable|reset-cooldown <id>]
  zerith routing [show|main|subagent|explore|plan|compact <model>|role <agent> <model>|combo <name> <m1,m2,...>]
  zerith tokensaver [show|ponytail <off|lite|full|ultra>|caveman <off|lite|full|ultra|wenyan>|rtk <on|off>]
  zerith repl [--session <id>|--resume [id|name]|--continue] [--model <id>] [--mode <mode>] [--effort <off|low|medium|high|xhigh|max>] [--remote <uri|alias>]
  zerith chat [--session <id>|--resume [id|name]|--continue] [--fork] [--model <id>] [--remote <uri|alias>] [<prompt>]
  zerith -p <prompt> [--output-format text|json|stream-json] [--model M]
        [--session ID | --resume [id|name] | --continue] [--fork]
        [--max-turns N] [--effort <off|low|medium|high|xhigh|max>]
        [--orchestrator]
        [--max-budget-usd X --price-in P --price-out P]
        [--allowed-tools a,b] [--disallowed-tools a,b] [--permission-mode MODE]
        [--json-schema <json|file>] [--no-tools] [--remote <uri|alias>]
  zerith schedule add <nome> --cron <expr> (--command <cmd> --no-agent | --prompt <p>) [--workspace dir] [--model M]
  zerith schedule list|remove <id|nome>|toggle <id|nome> <on|off>|run <id|nome>
  zerith daemon [--once] [--poll-secs N]
  zerith gateway telegram setup --token <tok|-> --chats <id1,id2> [--model M] [--workspace dir]
  zerith gateway telegram run [--poll-secs N]
  zerith remote list|add <alias> <ssh://...|docker://...>|exec <alvo> [--] <cmd...>
  zerith media show|models|set|key|generate   (imagem e vídeo; veja `zerith media`)
  zerith config show
  zerith secret set <name> [value] | get <name> | delete <name>
  zerith login <claude-code|codex|antigravity> [--account <id>] [--import] [--no-browser]
  zerith import hermes [--profile <name>] [--jsonl <file>]
  zerith import detect
  zerith import hermes-skills [nome..]   (copia skills do Hermes, ativas)
  zerith mcp import-hermes [nome..]      (servidores de config.yaml do Hermes)"
}

/// `%USERPROFILE%` on Windows, `$HOME` elsewhere: where the CLIs write the
/// credentials `zerith login --import` reads.
fn home_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("{}", usage());
        return Ok(());
    };
    let paths = Paths::resolve();
    if args::is_headless(args) {
        return headless::run(&paths, args);
    }

    match (cmd, args.get(1).map(String::as_str)) {
        ("config", _) => config_cmd(&paths, args)?,
        ("secret", _) => secret_cmd(args)?,
        ("version", _) | ("--version", _) | ("-V", _) => {
            println!(
                "{} {} — {}",
                zerith_core::NAME,
                zerith_core::VERSION,
                zerith_core::TAGLINE
            );
        }
        ("paths", _) => {
            println!("root  {}", paths.root().display());
            println!("db    {}", paths.db().display());
            println!("skills {}", paths.skills().display());
        }
        ("policy", _) => policy(&paths, args)?,
        ("bench", _) => bench()?,
        ("backup", _) => backup(&paths, args)?,
        ("restore", _) => restore(&paths, args)?,
        ("doctor", _) => cmds::doctor(&paths, args)?,
        ("session", Some("list")) => {
            let limit = flag_usize(args, "--limit").unwrap_or(50);
            let store = Store::open(paths.db())?;
            let rows = store.list_sessions(limit)?;
            if rows.is_empty() {
                println!("(no sessions)");
            }
            for s in rows {
                println!("{}  {:>6}  {}", s.id, s.source.as_str(), s.title);
            }
        }
        ("session", Some("new")) => {
            let title = args.get(2).cloned().unwrap_or_else(|| "untitled".into());
            let store = Store::open(paths.db())?;
            let s = store.create_session(&title, SessionSource::Native)?;
            println!("{}", s.id);
        }
        ("session", Some("rename")) => {
            let (key, title) = parse_rename(args)?;
            let store = Store::open(paths.db())?;
            let s = store.resolve_session(&key)?;
            store.rename_session(&s.id, &title)?;
            println!("{} agora se chama {:?}", s.id, title.trim());
        }
        ("session", Some("show")) => {
            let key = args.get(2).ok_or("session show needs an id or name")?;
            let store = Store::open(paths.db())?;
            let s = store.resolve_session(key)?;
            let id = &s.id;
            println!("# {} ({})", s.title, s.id);
            println!("source      {}", s.source.as_str());
            println!("created_at  {}", s.created_at);
            println!("messages    {}", store.message_count(id)?);
            for m in store.messages(id, 200, 0)? {
                println!("\n[{}] {}", m.role.as_str(), m.content);
            }
        }
        ("providers", _) => providers(&paths, args)?,
        ("usage", _) | ("quota", _) => cmds::usage_cmd(&paths, args)?,
        ("accounts", _) | ("account", _) => cmds::accounts_cmd(&paths, args)?,
        ("login", _) => cmds::login_cmd(&paths, args, &home_dir())?,
        ("routing", _) => cmds::routing_cmd(&paths, args)?,
        ("tokensaver", _) => cmds::tokensaver_cmd(&paths, args)?,
        ("repl", _) => repl(&paths, args)?,
        ("chat", None) => repl(&paths, args)?,
        ("chat", _) => chat(&paths, args)?,
        ("import", Some("hermes")) => import_hermes(&paths, args)?,
        ("import", Some("hermes-skills")) => {
            let home =
                zerith_importers::hermes_setup::hermes_home().ok_or("Hermes não encontrado")?;
            let lib = zerith_skills::Library::open(paths.skills())?;
            let found = zerith_importers::hermes_setup::hermes_skills(&home, &lib);
            let names: Vec<String> = match args.get(2) {
                Some(_) => args[2..].to_vec(),
                None => found.iter().map(|s| s.name.clone()).collect(),
            };
            let r = zerith_importers::hermes_setup::import_hermes_skills(&home, &lib, &names);
            println!(
                "{} skills no Hermes: {} importadas, {} já existiam, {} com erro",
                found.len(),
                r.imported.len(),
                r.existed.len(),
                r.failed.len()
            );
            for f in &r.failed {
                eprintln!("  {f}");
            }
        }
        ("export", _) => cmds::export_cmd(&paths, args)?,
        ("search", _) => cmds::search_cmd(&paths, args)?,
        ("checkpoints", _) => cmds::checkpoints_cmd(&paths, args)?,
        ("rewind", _) => cmds::rewind_cmd(&paths, args)?,
        ("agents", _) => cmds::agents_cmd(&paths, args)?,
        ("import", Some("detect")) => {
            let found = detect();
            if found.is_empty() {
                println!("(no Hermes install found)");
            }
            for s in found {
                println!(
                    "{}  profile={}  tables: sessions={} messages={}",
                    s.path.display(),
                    s.profile,
                    s.sessions_table,
                    s.messages_table
                );
            }
        }
        ("memory", Some(sub)) => memory(&paths, args, sub)?,
        ("skills", _) => skills(&paths, args)?,
        ("tools", _) => tools_cmd(&paths, args)?,
        ("plugin", _) | ("plugins", _) => cmds::plugin_cmd(&paths, args)?,
        ("mcp", _) => cmds::mcp_cmd(&paths, args)?,
        ("plan", _) => plan(&paths, args)?,
        ("schedule", _) => daemon::schedule_cmd(&paths, args)?,
        ("daemon", _) => daemon::daemon_cmd(&paths, args)?,
        ("gateway", _) => telegram::cmd(&paths, args)?,
        ("remote", _) => remote_cmd(&paths, args)?,
        ("media", _) => media_cmd::run(&paths, args)?,
        _ => {
            println!("{}", usage());
            return Err(format!("unknown command: {cmd}").into());
        }
    }
    Ok(())
}

/// Lists the built-in tools, or runs one against the current workspace.
///
/// The catalogue prints every field the spec requires a tool to declare, so a
/// tool that hides its timeout or its permissions stands out immediately.
fn tools_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let reg = Registry::new();
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            for t in reg.specs() {
                println!(
                    "{:<14} {:<8} {:<6} {:?}  {}",
                    t.name,
                    t.permission.as_str(),
                    if t.cancellable { "cancel" } else { "block" },
                    t.timeout,
                    t.description
                );
                println!("               returns {}", t.returns);
                println!("               origin  {}  local {}", t.source, t.local);
            }
        }
        Some("call") => {
            let name = args.get(2).ok_or("tools call needs a tool name")?;
            let raw = args.get(3).cloned().unwrap_or_else(|| "{}".to_string());
            let json: serde_json::Value = serde_json::from_str(&raw)?;
            paths.ensure()?;
            let settings = load_settings(paths);
            let workspace = std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf());
            let policy = Policy::from_settings(&settings, &workspace)?;
            let memory = SqliteMemory::open(paths.db()).ok();
            let scope = Scope::workspace(&workspace);
            let ctx = CallCtx {
                workspace: &workspace,
                policy: &policy,
                memory: memory.as_ref().map(|m| m as &dyn MemoryBackend),
                scope: Some(&scope),
            };
            println!("{}", reg.call(name, &json, &ctx)?);
        }
        Some(other) => return Err(format!("unknown tools command: {other}").into()),
    }
    Ok(())
}
/// Plan mode: a list of tasks, run in dependency order with a bounded number
/// of workers. Each task becomes its own session, so the transcript of what a
/// subagent did is a conversation the user can read.
/// Shows what the current preset permits, and what it cannot enforce. The
/// second half is the point: a preset that hides its limits is a lie.
fn policy(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_settings(paths);
    let workspace = std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf());
    let policy = Policy::from_settings(&settings, &workspace)?;
    println!("preset        {}", policy.preset.name);
    println!("sandbox       {}", policy.preset.sandbox.as_str());
    println!("enforcement   {}", policy.enforcement.as_str());
    let jail = zerith_security::jail::report(policy.preset.sandbox);
    println!("plataforma    {}", jail.platform);
    for i in &jail.isolated {
        println!("isolado       {i}");
    }
    for n in &jail.not_isolated {
        println!("NÃO isolado   {n}");
    }
    println!("workspace     {}", policy.workspace.display());
    println!("description   {}", policy.preset.description);
    if args.get(1).map(String::as_str) == Some("check") {
        let path = std::path::PathBuf::from(args.get(2).ok_or("policy check needs a path")?);
        match policy.check(&Action::Write(path)) {
            Verdict::Allow => println!("write         allow"),
            Verdict::Ask(why) => println!("write         ask ({why})"),
            Verdict::Deny(why) => println!("write         deny ({why})"),
        }
    }
    println!(
        "presets       {}",
        PRESETS
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// Numbers the user can check, not a claim. Everything runs against a
/// throwaway database, so a benchmark never touches real data.
fn bench() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;
    // Not the OS temp dir: on this machine it is ACL-restricted and a
    // benchmark that fails on permissions measures nothing.
    let dir = Paths::resolve()
        .root()
        .join(format!("bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("bench.db");

    let t = Instant::now();
    let store = Store::open(&db)?;
    println!(
        "db_open_and_migrate   {:>8.2} ms",
        t.elapsed().as_secs_f64() * 1e3
    );

    let session = store.create_session("bench", SessionSource::Native)?;
    let t = Instant::now();
    for i in 0..1000 {
        store.append(
            &session.id,
            &NewMessage::new(Role::User, format!("mensagem {i}")),
        )?;
    }
    let insert = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "append_1000_messages  {insert:>8.2} ms  ({:.2} ms each)",
        insert / 1000.0
    );

    let t = Instant::now();
    let page = store.messages(&session.id, 50, 0)?;
    println!(
        "page_50_messages      {:>8.2} ms  ({} rows)",
        t.elapsed().as_secs_f64() * 1e3,
        page.len()
    );

    let mem = SqliteMemory::open(&db)?;
    let scope = Scope::user("bench");
    for i in 0..200 {
        mem.remember(
            &scope,
            FactKind::Fact,
            &format!("fato {i}"),
            Confidence::Confirmed,
            None,
        )?;
    }
    let t = Instant::now();
    let hits = mem.recall(&scope, "fato", 12)?;
    println!(
        "recall_200_facts      {:>8.2} ms  ({} hits)",
        t.elapsed().as_secs_f64() * 1e3,
        hits.len()
    );

    // The whole file, so the number is about the data and not about caching
    // one row.
    let bytes = std::fs::metadata(&db)?.len();
    println!("db_size               {:>8} bytes", bytes);
    // Resident memory after the load. Wall time above is the CPU proxy: this
    // host has no portable CPU-time call in std, and a number invented from
    // nothing would be worse than a documented proxy.
    match rss_bytes() {
        Some(rss) => println!("rss_after_load        {:>8} KB", rss / 1024),
        None => println!("rss_after_load        (not measurable here)"),
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Resident set size, in bytes.
///
/// Windows is read through PowerShell because the alternative is a hand-written
/// FFI struct for one benchmark line. The cost is ~200 ms once, not per turn.
fn rss_bytes() -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        let script = format!("(Get-Process -Id {}).WorkingSet64", std::process::id());
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script.as_str()])
            .output()
            .ok()?;
        String::from_utf8(out.stdout).ok()?.trim().parse().ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
}
/// Consistent copy through the SQLite backup API. Copying a live WAL file with
/// the filesystem would produce a database that only sometimes opens.
fn backup(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    paths.ensure()?;
    let target = match flag_value(args, "--to") {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let stamp = zerith_sessions::migrations::now_ms();
            paths.backups().join(format!("zerith-{stamp}.db"))
        }
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(paths.db())?;
    store.backup_to(&target)?;
    println!("{}", target.display());
    Ok(())
}

/// Replaces the database with a backup, after checking the backup opens and
/// passes `integrity_check`. The current database is copied aside first, so a
/// bad restore is a mistake the user can undo.
fn restore(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let from = args.get(1).ok_or("restore needs a backup file")?;
    paths.ensure()?;
    let candidate = Store::open(from)?;
    if !candidate.integrity_ok()? {
        return Err("the backup failed integrity_check; nothing was replaced".into());
    }
    if paths.db().exists() {
        let stamp = zerith_sessions::migrations::now_ms();
        Store::open(paths.db())?
            .backup_to(paths.backups().join(format!("before-restore-{stamp}.db")))?;
    }
    drop(candidate);
    std::fs::copy(from, paths.db())?;
    println!("restored {} into {}", from, paths.db().display());
    Ok(())
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn plan(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args.get(1).map(String::as_str).unwrap_or("list");
    paths.ensure()?;
    match sub {
        "list" => {
            let store = Store::open(paths.db())?;
            let plans = store.unfinished_plans()?;
            if plans.is_empty() {
                println!("(no unfinished plans)");
            }
            for p in plans {
                let tasks = store.tasks(&p)?;
                let done = tasks.iter().filter(|t| t.state == TaskState::Done).count();
                println!("{p}  {done}/{} done", tasks.len());
            }
        }
        "run" => {
            let file = args
                .get(2)
                .ok_or("plan run needs a file (use - for stdin)")?;
            let text = if file == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else {
                std::fs::read_to_string(file)?
            };
            // The profile's subagent cap; `--agents` overrides it.
            let mut agents = zerith_runtime::active_profile().limits().max_subagents;
            let mut model = std::env::var("ZERITH_MODEL").unwrap_or_default();
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--agents" => {
                        agents = args.get(i + 1).ok_or("--agents needs a number")?.parse()?;
                        i += 2;
                    }
                    "--model" => {
                        model = args.get(i + 1).ok_or("--model needs an id")?.clone();
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            let plan_id = format!("plan-{}", zerith_sessions::plan::task_id_seed());
            let tasks = parse_plan(&plan_id, &text);
            if tasks.is_empty() {
                return Err("the plan is empty".into());
            }
            let store = Store::open(paths.db())?;
            store.create_plan(&plan_id, &tasks)?;
            println!(
                "plan {plan_id}: {} task(s), up to {agents} agent(s)",
                tasks.len()
            );
            let report = run_plan(
                &paths.db(),
                &plan_id,
                &model,
                agents,
                Some(Scope::workspace(
                    &std::env::current_dir().unwrap_or_default(),
                )),
                |task, state| println!("  {}  {}", state.as_str(), task.title),
            )?;
            println!(
                "done {}  failed {}  skipped {}",
                report.done, report.failed, report.skipped
            );
            if report.failed > 0 || report.skipped > 0 {
                return Err("the plan did not finish cleanly".into());
            }
        }
        other => return Err(format!("unknown plan command: {other}").into()),
    }
    Ok(())
}

fn memory(paths: &Paths, args: &[String], sub: &str) -> Result<(), Box<dyn std::error::Error>> {
    if cmds::memory_admin(paths, args)? {
        return Ok(());
    }
    let mem = SqliteMemory::open(paths.db())?;
    match sub {
        "remember" => {
            let scope = Scope(
                args.get(2)
                    .cloned()
                    .ok_or("memory remember needs a scope")?,
            );
            let text = args.get(3).cloned().ok_or("memory remember needs text")?;
            let confidence = if args.iter().any(|a| a == "--inferred") {
                Confidence::Inferred
            } else {
                Confidence::Confirmed
            };
            let id = mem.remember(&scope, FactKind::Fact, &text, confidence, Some("cli"))?;
            println!("{id}");
        }
        "recall" => {
            let scope = Scope(args.get(2).cloned().ok_or("memory recall needs a scope")?);
            let query = args.get(3).cloned().ok_or("memory recall needs a query")?;
            let hits = mem.recall(&scope, &query, 20)?;
            if hits.is_empty() {
                println!("(nothing found)");
            }
            for h in hits {
                println!("{:>5}  {:.3}  {}", h.fact.id, h.score, h.fact.text);
            }
        }
        "list" => {
            let scope = Scope(args.get(2).cloned().ok_or("memory list needs a scope")?);
            for f in mem.list(&scope, 100)? {
                println!(
                    "{:>5}  {}  {}  {}",
                    f.id,
                    f.confidence.as_str(),
                    f.kind.as_str(),
                    f.text
                );
            }
        }
        "forget" => {
            let id: i64 = args.get(2).ok_or("memory forget needs an id")?.parse()?;
            mem.forget(id)?;
            println!("forgotten {id}");
        }
        "edit" => {
            let id: i64 = args.get(2).ok_or("memory edit needs an id")?.parse()?;
            let text = args.get(3).cloned().ok_or("memory edit needs text")?;
            mem.update(id, &text)?;
            println!("{id} atualizado");
        }
        "export" => {
            let scope = Scope(args.get(2).cloned().ok_or("memory export needs a scope")?);
            let facts = mem.export(&scope)?;
            println!("{}", serde_json::to_string_pretty(&facts)?);
        }
        _ => return Err(format!("unknown memory command: {sub}").into()),
    }
    Ok(())
}

/// `zerith config show`: every merged key with the layer it came from.
fn config_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.get(1).map(String::as_str) {
        None | Some("show") => {
            let cwd = std::env::current_dir().ok();
            let loaded = Settings::load(paths, cwd.as_deref());
            println!("# usuário  {}", paths.settings().display());
            if let Some(d) = &cwd {
                println!("# projeto  {}", d.join(".zerith").display());
            }
            for (k, v, src) in loaded.entries() {
                println!("{k:<36} {v:<32} [{src}]");
            }
            for e in &loaded.errors {
                eprintln!("zerith: {e}");
            }
        }
        Some(other) => return Err(format!("unknown config command: {other}").into()),
    }
    Ok(())
}

/// `zerith secret set|get|delete <name>`. `set` reads the value from stdin
/// when not given, so it stays out of shell history.
fn secret_cmd(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args.get(1).map(String::as_str);
    let name = args.get(2).ok_or("secret needs a name")?;
    match sub {
        Some("set") => {
            let value = match args.get(3) {
                Some(v) => v.clone(),
                None => {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim_end_matches(['\r', '\n']).to_string()
                }
            };
            if value.is_empty() {
                return Err("valor vazio; nada gravado".into());
            }
            secrets::set_secret(name, &value)?;
            println!("{name} gravado no cofre do sistema");
        }
        Some("get") => println!("{name} {}", secrets::mask(&secrets::get_secret(name)?)),
        Some("delete") => {
            secrets::delete_secret(name)?;
            println!("{name} removido");
        }
        _ => return Err("usage: zerith secret set|get|delete <name>".into()),
    }
    Ok(())
}

/// `[s]im / [n]ão / [t]odos nesta conversa` on stderr; anything else is no.
/// The turn waits here, on its own thread, which is what the gate expects.
fn ask_terminal(tool: &str, summary: &str) -> zerith_runtime::Approval {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = write!(
        err,
        "\n\x1b[1m[aprovação]\x1b[0m {tool}: {summary}\n  Permitir? [s]im / [n]ão / [t]odos nesta conversa: "
    );
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return zerith_runtime::Approval::Deny;
    }
    parse_answer(&line)
}

fn parse_answer(line: &str) -> zerith_runtime::Approval {
    match line.trim().to_lowercase().as_str() {
        "s" | "sim" | "y" | "yes" => zerith_runtime::Approval::AllowOnce,
        "t" | "todos" | "a" | "all" => zerith_runtime::Approval::AllowSession,
        _ => zerith_runtime::Approval::Deny,
    }
}

struct CliTurnCtx<'a> {
    settings: &'a Settings,
    store: &'a Store,
    client: &'a Client,
    memory: Option<&'a dyn MemoryBackend>,
    scope: &'a Scope,
    skills: Option<&'a Library>,
    tools: &'a mut Registry,
    policy: &'a mut Policy,
    approver: &'a zerith_runtime::ApprovalSender,
    gate: &'a zerith_runtime::ApprovalGate,
    interactive: bool,
    workspace: &'a std::path::Path,
    spill_dir: std::path::PathBuf,
    cancel: &'a Cancel,
}

fn execute_cli_turn(
    ctx: &mut CliTurnCtx<'_>,
    session_id: &str,
    model: &str,
    prompt: &str,
    reasoning_effort: Option<&str>,
) -> Result<TurnReport, Box<dyn std::error::Error>> {
    let guards = Guards::new(ctx.settings.clone(), ctx.policy, session_id.to_string());
    ctx.tools.register(guards.enter_plan_mode_tool());
    ctx.tools.register(guards.exit_plan_mode_tool());
    let opts = AgentOptions {
        tools: Some(ctx.tools),
        policy: Some(ctx.policy),
        approvals: ctx.interactive.then_some(ctx.gate),
        middleware: vec![Box::new(guards)],
        spill_dir: Some(ctx.spill_dir.clone()),
        reasoning_effort: reasoning_effort.map(str::to_string),
        ..AgentOptions::for_profile(zerith_runtime::active_profile())
    };
    match zerith_runtime::compact::auto_compact_routed(
        ctx.store,
        ctx.settings,
        ctx.client,
        session_id,
        model,
        opts.history,
        ctx.cancel,
    ) {
        Ok(Some(r)) => eprintln!(
            "zerith: histórico resumido ({} → {} tokens)",
            r.tokens_before, r.tokens_after
        ),
        Ok(None) => {}
        Err(e) => eprintln!("zerith: resumo automático falhou: {e}"),
    }

    let mut streamed = false;
    let mut digest = zerith_runtime::learn::TurnDigest::new(prompt);
    let result = run_agent_turn(
        ctx.store,
        ctx.client,
        TurnContext {
            memory: ctx.memory,
            scope: Some(ctx.scope),
            skills: ctx.skills,
            settings: Some(ctx.settings),
            project_dir: Some(ctx.workspace),
            ..TurnContext::default()
        },
        opts,
        session_id,
        model,
        prompt,
        ctx.cancel,
        |e| {
            digest.observe(&e);
            match e {
                RunEvent::Delta(text) => {
                    streamed = true;
                    print!("{text}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                RunEvent::Reasoning(r) => eprintln!("[reasoning] {r}"),
                RunEvent::ApprovalNeeded { id, tool, summary } => {
                    ctx.approver.answer(id, ask_terminal(&tool, &summary));
                }
                RunEvent::ToolCall { name, args, .. } => eprintln!("\n[tool] {name} {args}"),
                RunEvent::ToolResult {
                    name,
                    ok,
                    output_preview,
                    ..
                } => eprintln!(
                    "[tool:{}] {name}: {output_preview}",
                    if ok { "ok" } else { "erro" }
                ),
                RunEvent::Steered { text } => eprintln!("\n[steer] {text}"),
                RunEvent::SkillsLoaded { names } => eprintln!("[skill] {}", names.join(", ")),
                RunEvent::Route {
                    account_id,
                    provider,
                    model,
                    attempts,
                } if attempts > 1 => {
                    eprintln!(
                        "[gateway:failover] tentativa #{attempts} -> conta {account_id} ({provider}/{model})"
                    );
                }
                _ => {}
            }
        },
    );

    if let Ok(r) = &result {
        digest.answer = r.text.clone();
        headless::learn_after(
            ctx.settings,
            ctx.store,
            ctx.client,
            session_id,
            model,
            ctx.memory.map(|m| (m, ctx.scope)),
            ctx.skills,
            &digest,
            ctx.cancel,
        );
    }

    let report = match result {
        Ok(r) => r,
        Err(RunError::Provider(ProviderError::Cancelled)) => {
            return Err(Box::new(ExitWith(
                signals::received().unwrap_or(143),
                "cancelado".into(),
            )))
        }
        Err(e) => return Err(e.into()),
    };
    if streamed {
        println!();
    } else {
        println!("{}", report.text);
    }
    eprintln!("{}", format_chat_footer(&report, model));
    Ok(report)
}

/// One turn: resolve the session, stream the answer, persist it whole.
fn chat(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let args::ChatArgs {
        prompt,
        session: session_flag,
        model: model_flag,
        remote: remote_flag,
    } = parse_chat(args)?;

    let mut settings = load_settings(paths);
    let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }
    zerith_runtime::bili::attach(paths.root(), settings.context.billion_context);
    let cfg = provider_config(&settings)?;
    let model = resolve_model(model_flag, &settings, &cfg)
        .ok_or("sem modelo: use --model, routing.main_model ou ZERITH_MODEL")?;
    let client = Client::new(cfg)?;

    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let session_id = select_session(&store, &session_flag, &prompt)?;

    let workspace = std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf());
    let mut policy = Policy::from_settings(&settings, &workspace)?;
    let mut tools = Registry::new();
    let remote_label = if let Some(raw) = remote_flag.as_deref() {
        let target = zerith_tools::remote::parse_remote_target(raw, &settings.remote.targets)?;
        tools.register(zerith_tools::remote::remote_shell_tool(
            target,
            raw.to_string(),
        ));
        Some(raw.to_string())
    } else {
        None
    };
    let mut mcp = zerith_mcp::McpRegistry::load(paths, Some(&workspace));
    if let Some(c) = &contrib {
        mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
    }
    for note in &mcp.notes {
        eprintln!("zerith: {note}");
    }
    let done = mcp.register_into(&mut tools);
    for e in &done.errors {
        eprintln!("zerith: {e}");
    }
    let expanded = if mcp.is_empty() {
        prompt
    } else {
        let (text, errors) = mcp.expand_mentions(&prompt);
        for e in &errors {
            eprintln!("zerith: {e}");
        }
        text
    };
    let prompt = match remote_label {
        Some(raw) => format!(
            "[alvo remoto ativo: {raw} — use a ferramenta remote_shell para executar comandos nesse host/container]\n\n{expanded}"
        ),
        None => expanded,
    };
    let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
    for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
        paths: paths.clone(),
        project_dir: Some(workspace.clone()),
        tools: have,
        mode: policy.mode().settings_name().to_string(),
    }) {
        tools.register(t);
    }

    let memory = SqliteMemory::open(paths.db())?;
    let scope = Scope::workspace(&workspace);
    let memory = (!settings.memory.paused).then_some(&memory as &dyn MemoryBackend);
    let skills = Library::open(paths.skills()).ok().map(|l| {
        if let Some(c) = &contrib {
            l.with_extra(c.skill_dirs())
        } else {
            l
        }
    });

    let cancel = Cancel::default();
    signals::install(cancel.clone());
    let (approver, gate) = zerith_runtime::approval_channel();
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());

    let mut ctx = CliTurnCtx {
        settings: &settings,
        store: &store,
        client: &client,
        memory,
        scope: &scope,
        skills: skills.as_ref(),
        tools: &mut tools,
        policy: &mut policy,
        approver: &approver,
        gate: &gate,
        interactive,
        workspace: &workspace,
        spill_dir: paths.root().join("spill"),
        cancel: &cancel,
    };
    let res = execute_cli_turn(&mut ctx, &session_id, &model, &prompt, None);

    let end = zerith_core::hooks::run_hooks(
        zerith_core::hooks::HookEvent::SessionEnd,
        &zerith_core::hooks::HookPayload {
            session_id,
            cwd: workspace,
            ..Default::default()
        },
        &settings,
    );
    for e in &end.errors {
        eprintln!("zerith: {e}");
    }

    res?;
    Ok(())
}

#[derive(Debug, PartialEq)]
enum ReplSlash<'a> {
    Help,
    Exit,
    Clear,
    Compact,
    Sessions(Option<&'a str>),
    Model(Option<&'a str>),
    Mode(Option<&'a str>),
    Effort(Option<&'a str>),
    Rewind(Option<i64>),
    Unknown(&'a str),
}

fn parse_repl_slash(line: &str) -> Option<ReplSlash<'_>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().map(str::trim).filter(|s| !s.is_empty());
    Some(match cmd {
        "/help" | "/?" => ReplSlash::Help,
        "/exit" | "/quit" => ReplSlash::Exit,
        "/clear" | "/new" => ReplSlash::Clear,
        "/compact" => ReplSlash::Compact,
        "/sessions" => ReplSlash::Sessions(rest),
        "/model" => ReplSlash::Model(rest),
        "/mode" => ReplSlash::Mode(rest),
        "/effort" => ReplSlash::Effort(rest),
        "/rewind" => ReplSlash::Rewind(rest.and_then(|s| s.parse().ok())),
        other => ReplSlash::Unknown(other),
    })
}

/// Interactive multi-turn terminal REPL keeping store, warm HTTP client,
/// MCP servers, policy handle, and session grants loaded across turns.
fn repl(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, Write};
    let parsed = parse_repl(args)?;

    let mut settings = load_settings(paths);
    let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }
    zerith_runtime::bili::attach(paths.root(), settings.context.billion_context);
    let cfg = provider_config(&settings)?;
    let mut model = resolve_model(parsed.model, &settings, &cfg)
        .ok_or("sem modelo: use --model, routing.main_model ou ZERITH_MODEL")?;
    let client = Client::new(cfg)?;

    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let workspace = parsed
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf()));
    let base_policy = Policy::from_settings(&settings, &workspace)?;
    let mode_handle =
        zerith_security::ModeHandle::new(parsed.mode.unwrap_or_else(|| base_policy.mode()));
    let mut policy = base_policy.with_handle(mode_handle.clone());

    let mut tools = Registry::new();
    let remote_label = if let Some(raw) = parsed.remote.as_deref() {
        let target = zerith_tools::remote::parse_remote_target(raw, &settings.remote.targets)?;
        tools.register(zerith_tools::remote::remote_shell_tool(
            target,
            raw.to_string(),
        ));
        Some(raw.to_string())
    } else {
        None
    };
    let mut mcp = zerith_mcp::McpRegistry::load(paths, Some(&workspace));
    if let Some(c) = &contrib {
        mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
    }
    for note in &mcp.notes {
        eprintln!("zerith: {note}");
    }
    let done = mcp.register_into(&mut tools);
    for e in &done.errors {
        eprintln!("zerith: {e}");
    }
    let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
    for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
        paths: paths.clone(),
        project_dir: Some(workspace.clone()),
        tools: have,
        mode: policy.mode().settings_name().to_string(),
    }) {
        tools.register(t);
    }

    let memory = SqliteMemory::open(paths.db())?;
    let scope = Scope::workspace(&workspace);
    let memory = (!settings.memory.paused).then_some(&memory as &dyn MemoryBackend);
    let skills = Library::open(paths.skills()).ok().map(|l| {
        if let Some(c) = &contrib {
            l.with_extra(c.skill_dirs())
        } else {
            l
        }
    });

    let grants: zerith_runtime::SharedGrants = Default::default();
    let (approver, gate) = zerith_runtime::approval_channel_with_grants(grants.clone());
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());

    let mut session_id: Option<String> = if !matches!(parsed.session.target, Target::New) {
        Some(select_session(&store, &parsed.session, "REPL")?)
    } else {
        None
    };
    let mut effort: Option<String> = parsed.effort;
    let mut announced_remote = false;
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());

    eprintln!(
        "zerith REPL · modelo `{model}` · modo `{}` · /help para comandos, /exit para sair",
        mode_handle.get().settings_name()
    );
    if let Some(raw) = &remote_label {
        eprintln!("zerith: alvo remoto ativo `{raw}` (ferramenta remote_shell habilitada)");
    }

    loop {
        let sid_tag = session_id
            .as_deref()
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "nova".into());
        eprint!(
            "\nzerith[{sid_tag}|{}|{}]> ",
            mode_handle.get().settings_name(),
            model
        );
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(cmd) = parse_repl_slash(trimmed) {
            match cmd {
                ReplSlash::Help => eprintln!(
                    "/help  /model <id>  /mode <default|acceptEdits|plan|bypass>  \
                     /effort <off|low|medium|high|xhigh|max>  /compact  /rewind [N]  \
                     /sessions [id]  /clear  /exit"
                ),
                ReplSlash::Exit => break,
                ReplSlash::Clear => {
                    session_id = None;
                    announced_remote = false;
                    if let Ok(mut g) = grants.lock() {
                        g.clear();
                    }
                    eprintln!("zerith: próxima mensagem iniciará nova sessão (grants limpos).");
                }
                ReplSlash::Model(Some(m)) => {
                    model = m.to_string();
                    eprintln!("zerith: modelo -> {model}");
                }
                ReplSlash::Model(None) => eprintln!("zerith: modelo atual -> {model}"),
                ReplSlash::Mode(Some(m)) => match zerith_security::Mode::parse(m) {
                    Some(parsed_mode) => {
                        mode_handle.set(parsed_mode);
                        eprintln!("zerith: modo -> {}", parsed_mode.settings_name());
                    }
                    None => eprintln!(
                        "zerith: modo inválido `{m}` (use default, acceptEdits, plan ou bypass)"
                    ),
                },
                ReplSlash::Mode(None) => {
                    eprintln!(
                        "zerith: modo atual -> {}",
                        mode_handle.get().settings_name()
                    )
                }
                ReplSlash::Effort(Some(e)) => {
                    let norm = e.to_ascii_lowercase();
                    if norm == "off" {
                        effort = None;
                        eprintln!("zerith: effort -> off");
                    } else if matches!(norm.as_str(), "low" | "medium" | "high" | "xhigh" | "max") {
                        effort = Some(norm.clone());
                        eprintln!("zerith: effort -> {norm}");
                    } else {
                        eprintln!("zerith: effort inválido `{e}` (use off, low, medium, high, xhigh ou max)");
                    }
                }
                ReplSlash::Effort(None) => {
                    eprintln!(
                        "zerith: effort atual -> {}",
                        effort.as_deref().unwrap_or("off")
                    )
                }
                ReplSlash::Compact => {
                    let Some(sid) = session_id.as_deref() else {
                        eprintln!("zerith: nenhuma sessão ativa para compactar.");
                        continue;
                    };
                    let cancel = Cancel::default();
                    match zerith_runtime::compact::compact_now(
                        &store, &client, sid, &model, &cancel, None,
                    ) {
                        Ok(Some(r)) => eprintln!(
                            "zerith: compactado ({} -> {} tokens)",
                            r.tokens_before, r.tokens_after
                        ),
                        Ok(None) => eprintln!("zerith: nada para compactar."),
                        Err(e) => eprintln!("zerith: falha ao compactar: {e}"),
                    }
                }
                ReplSlash::Rewind(turn) => {
                    let Some(sid) = session_id.as_deref() else {
                        eprintln!("zerith: nenhuma sessão ativa.");
                        continue;
                    };
                    match turn {
                        Some(t) => match zerith_sessions::checkpoints::restore_turn(
                            &store,
                            paths.root(),
                            sid,
                            t,
                        ) {
                            Ok(rep) => eprintln!(
                                "zerith: restaurado turno {t} (arquivos restaurados={}, removidos={}, msgs={})",
                                rep.files_restored, rep.files_removed, rep.messages_archived
                            ),
                            Err(e) => eprintln!("zerith: rewind falhou: {e}"),
                        },
                        None => match zerith_sessions::checkpoints::list(&store, paths.root(), sid)
                        {
                            Ok(cps) if cps.is_empty() => eprintln!("zerith: sem checkpoints nesta sessão."),
                            Ok(cps) => {
                                for cp in cps {
                                    eprintln!("  turno {} ({} arquivos)", cp.turn, cp.files);
                                }
                                eprintln!("use /rewind <turno> para restaurar.");
                            }
                            Err(e) => eprintln!("zerith: erro ao listar checkpoints: {e}"),
                        },
                    }
                }
                ReplSlash::Sessions(None) => match store.list_sessions(15) {
                    Ok(rows) => {
                        for s in rows {
                            eprintln!(
                                "  {}  {}",
                                s.id.chars().take(8).collect::<String>(),
                                s.title
                            );
                        }
                    }
                    Err(e) => eprintln!("zerith: {e}"),
                },
                ReplSlash::Sessions(Some(key)) => match store.resolve_session(key) {
                    Ok(s) => {
                        session_id = Some(s.id.clone());
                        eprintln!("zerith: sessão ativa -> {} ({})", s.id, s.title);
                    }
                    Err(e) => eprintln!("zerith: {e}"),
                },
                ReplSlash::Unknown(other) => {
                    eprintln!("zerith: comando desconhecido {other} (tente /help)");
                }
            }
            continue;
        }

        let sid = match &session_id {
            Some(s) => s.clone(),
            None => {
                let s = select_session(&store, &SessionArgs::default(), trimmed)?;
                let _ = store.set_workspace(&s, &workspace.to_string_lossy());
                session_id = Some(s.clone());
                s
            }
        };
        let expanded = if mcp.is_empty() {
            trimmed.to_string()
        } else {
            let (text, errors) = mcp.expand_mentions(trimmed);
            for e in &errors {
                eprintln!("zerith: {e}");
            }
            text
        };
        let prompt = match remote_label.as_deref() {
            Some(raw) if !announced_remote => {
                announced_remote = true;
                format!(
                    "[alvo remoto ativo: {raw} — use a ferramenta remote_shell para executar comandos nesse host/container]\n\n{expanded}"
                )
            }
            _ => expanded,
        };

        let cancel = Cancel::default();
        let mut ctx = CliTurnCtx {
            settings: &settings,
            store: &store,
            client: &client,
            memory,
            scope: &scope,
            skills: skills.as_ref(),
            tools: &mut tools,
            policy: &mut policy,
            approver: &approver,
            gate: &gate,
            interactive,
            workspace: &workspace,
            spill_dir: paths.root().join("spill"),
            cancel: &cancel,
        };
        if let Err(e) = execute_cli_turn(&mut ctx, &sid, &model, &prompt, effort.as_deref()) {
            eprintln!("zerith: {}", redact(&e.to_string()));
        }
    }

    if let Some(sid) = session_id {
        let end = zerith_core::hooks::run_hooks(
            zerith_core::hooks::HookEvent::SessionEnd,
            &zerith_core::hooks::HookPayload {
                session_id: sid,
                cwd: workspace,
                ..Default::default()
            },
            &settings,
        );
        for e in &end.errors {
            eprintln!("zerith: {e}");
        }
    }
    Ok(())
}

fn format_chat_footer(report: &TurnReport, fallback_model: &str) -> String {
    let acc = report.account_id.as_deref().unwrap_or("-");
    let mdl = report.model_used.as_deref().unwrap_or(fallback_model);
    if report.saved_tokens > 0 {
        format!(
            "\n[turn] {}  conta {}  modelo {}  tokens {}→{}  saved {} (rtk:{} ponytail:{} caveman:{})  msg {}",
            report.session_id,
            acc,
            mdl,
            report.prompt_tokens,
            report.completion_tokens,
            report.saved_tokens,
            report.saved_rtk_tokens,
            report.saved_ponytail_tokens,
            report.saved_caveman_tokens,
            report.message_id,
        )
    } else {
        format!(
            "\n[turn] {}  conta {}  modelo {}  tokens {}→{}  msg {}",
            report.session_id,
            acc,
            mdl,
            report.prompt_tokens,
            report.completion_tokens,
            report.message_id,
        )
    }
}

fn providers(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_settings(paths);
    let cfg = provider_config(&settings)?;
    match args.get(1).map(String::as_str) {
        None | Some("show") => println!("{}", cfg.redacted()),
        Some("probe") => {
            let client = Client::new(cfg)?;
            let probe = client.probe();
            println!(
                "{}  http {}  {} ms  {} models",
                if probe.ok { "ok" } else { "fail" },
                probe.status,
                probe.latency_ms,
                probe.models.len()
            );
            if !probe.detail.is_empty() {
                println!("{}", probe.detail);
            }
        }
        Some("models") => {
            let client = Client::new(cfg)?;
            for id in client.models()? {
                println!("{id}");
            }
        }
        Some("chat") => {
            let model = args
                .get(2)
                .cloned()
                .or_else(|| {
                    settings
                        .routing
                        .main_model
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .or_else(|| cfg.default_model.clone())
                .ok_or("providers chat precisa de um modelo (ou ZERITH_MODEL)")?;
            let prompt = args
                .get(3)
                .cloned()
                .ok_or("providers chat precisa de um prompt")?;
            let client = Client::new(cfg)?;
            let req = ChatRequest::new(model, vec![ChatMessage::user(prompt)]);
            let out = client.complete(&req, &Default::default())?;
            if let Some(r) = out.reasoning {
                eprintln!("[reasoning]\n{r}");
            }
            println!("{}", out.text);
        }
        Some(other) => return Err(format!("comando desconhecido: providers {other}").into()),
    }
    Ok(())
}

fn import_hermes(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let a = parse_import_hermes(args)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let report: ImportReport = match &a.jsonl {
        Some(file) => import_jsonl(
            std::path::Path::new(file),
            a.profile.as_deref().unwrap_or("jsonl"),
            &store,
        )?,
        None => {
            let sources = detect();
            let Some(first) = sources.first() else {
                return Err("no Hermes database found (~/.hermes/state.db); \
                            use --jsonl <file> for an export"
                    .into());
            };
            let picked = match &a.profile {
                Some(p) => sources
                    .iter()
                    .find(|s| &s.profile == p)
                    .ok_or_else(|| format!("no Hermes profile named {p}"))?,
                None => first,
            };
            import(&picked.path, &picked.profile, &store)?
        }
    };
    println!(
        "sessions seen {}  imported {}  skipped {}  resumed {}  messages {}  skipped {}  failed {}",
        report.sessions_seen,
        report.sessions_imported,
        report.sessions_skipped,
        report.sessions_resumed,
        report.messages_imported,
        report.messages_skipped,
        report.failed.len()
    );
    for f in &report.failed {
        eprintln!("failed: {f}");
    }
    Ok(())
}

fn flag_usize(args: &[String], name: &str) -> Option<usize> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1)?.parse().ok()
}

/// Reads `SKILL.md` files. Nothing here runs a skill: `validate` reports facts
/// and exits, which is exactly what an install decision needs.
fn skills(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args.get(1).map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            paths.ensure()?;
            let found = discover(&paths.skills())?;
            if found.is_empty() {
                println!("(no skills in {})", paths.skills().display());
            }
            for s in found {
                println!(
                    "{}  scripts:{}  findings:{}",
                    s.frontmatter.name,
                    s.scripts.len(),
                    s.findings.len()
                );
            }
        }
        "validate" | "show" => {
            let dir = args.get(2).ok_or("skills validate needs a directory")?;
            let s = scan(std::path::Path::new(dir))?;
            println!("name          {}", s.frontmatter.name);
            println!("description   {}", s.frontmatter.description);
            if let Some(l) = &s.frontmatter.license {
                println!("license       {l}");
            }
            if let Some(c) = &s.frontmatter.compatibility {
                println!("compatibility {c}");
            }
            for (ext, n) in &s.inventory {
                println!("file          {n} x {ext}");
            }
            for script in &s.scripts {
                println!("script        {script}  (not executed)");
            }
            for f in &s.findings {
                println!("finding       {}  {}", f.severity.as_str(), f.message);
            }
        }
        "install" => {
            let dir = args.get(2).ok_or("skills install needs a directory")?;
            paths.ensure()?;
            let lib = Library::open(paths.skills())?;
            let row = lib.install(std::path::Path::new(dir), dir)?;
            println!("installed {} ({})", row.name, row.status.as_str());
        }
        "remove" => {
            let name = args.get(2).ok_or("skills remove needs a name")?;
            paths.ensure()?;
            Library::open(paths.skills())?.remove(name)?;
            println!("removed {name}");
        }
        "enable" | "disable" => {
            let name = args.get(2).ok_or("skills enable/disable needs a name")?;
            paths.ensure()?;
            let status = if sub == "enable" {
                Status::Enabled
            } else {
                Status::Disabled
            };
            Library::open(paths.skills())?.set_status(name, status)?;
            println!("{name} {}", status.as_str());
        }
        "update" => {
            let (name, from) = args::parse_skills_update(args)?;
            paths.ensure()?;
            let lib = Library::open(paths.skills())?;
            let from = match from {
                Some(f) => f,
                // Without a directory, re-read where it was installed from.
                None => lib
                    .manifest()?
                    .skills
                    .into_iter()
                    .find(|r| r.name == name)
                    .map(|r| r.source)
                    .ok_or_else(|| format!("skill {name} não está instalada"))?,
            };
            if !std::path::Path::new(&from).is_dir() {
                return Err(format!(
                    "{from} não é uma pasta; informe de onde atualizar: skills update {name} <pasta>"
                )
                .into());
            }
            let fresh = lib.update(&name, std::path::Path::new(&from))?;
            println!(
                "updated {name} from {from} to {} file(s)",
                fresh.inventory.values().sum::<usize>()
            );
            for f in &fresh.findings {
                println!("finding       {}  {}", f.severity.as_str(), f.message);
            }
        }
        "rollback" => {
            let name = args.get(2).ok_or("skills rollback needs a name")?;
            paths.ensure()?;
            Library::open(paths.skills())?.rollback(name)?;
            println!("rolled back {name}");
        }
        "installed" => {
            paths.ensure()?;
            let lib = Library::open(paths.skills())?;
            let rows = lib.manifest()?.skills;
            if rows.is_empty() {
                println!("(nothing installed)");
            }
            for r in rows {
                println!(
                    "{}  {:<9} {}  {}",
                    r.name,
                    r.status.as_str(),
                    r.license,
                    r.source
                );
            }
        }
        "catalog" => {
            paths.ensure()?;
            let force = args.iter().any(|a| a == "--refresh");
            let token = std::env::var("GITHUB_TOKEN").ok();
            let cat = catalog::load(paths.root(), token.as_deref(), force)?;
            println!("fetched_at    {}", cat.fetched_at);
            for e in cat.entries {
                println!(
                    "{:<38} {:>7} stars  {:<12} {}  {}",
                    e.repo, e.stars, e.license, e.updated_at, e.description
                );
            }
        }
        other => return Err(format!("unknown skills command: {other}").into()),
    }
    Ok(())
}

fn remote_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.get(1).map(String::as_str) {
        Some("list") | None => {
            let settings = load_settings(paths);
            if settings.remote.targets.is_empty() {
                println!("(nenhum alvo remoto configurado em [remote.targets])");
                return Ok(());
            }
            for (alias, uri) in &settings.remote.targets {
                println!("{alias}\t{uri}");
            }
            Ok(())
        }
        Some("add") => {
            let alias = args
                .get(2)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("uso: zerith remote add <alias> <ssh://...|docker://...>")?;
            let uri = args
                .get(3)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("uso: zerith remote add <alias> <ssh://...|docker://...>")?;
            if alias.starts_with('-')
                || alias.contains('/')
                || alias.chars().any(char::is_whitespace)
            {
                return Err(format!("alias inválido: {alias}").into());
            }
            let empty = std::collections::BTreeMap::new();
            let _ = zerith_tools::remote::parse_remote_target(uri, &empty)?;
            Settings::save_user(paths, |s| {
                s.remote.targets.insert(alias.to_string(), uri.to_string());
            })?;
            println!("Alvo remoto `{alias}` salvo -> {uri}");
            Ok(())
        }
        Some("remove") | Some("rm") => {
            let alias = args.get(2).ok_or("uso: zerith remote remove <alias>")?;
            let mut removed = false;
            Settings::save_user(paths, |s| {
                removed = s.remote.targets.remove(alias).is_some();
            })?;
            if removed {
                println!("Alvo remoto removido: {alias}");
                Ok(())
            } else {
                Err(format!("alvo remoto não encontrado: {alias}").into())
            }
        }
        Some("exec") => {
            let target_arg = args
                .get(2)
                .ok_or("uso: zerith remote exec <alvo> [--] <comando...>")?;
            let rest = args.get(3..).unwrap_or(&[]);
            let cmd_parts = if rest.first().map(String::as_str) == Some("--") {
                &rest[1..]
            } else {
                rest
            };
            if cmd_parts.is_empty() {
                return Err("informe o comando a executar no alvo remoto".into());
            }
            let command = cmd_parts.join(" ");
            let settings = load_settings(paths);
            let target =
                zerith_tools::remote::parse_remote_target(target_arg, &settings.remote.targets)?;
            let (prog, prog_args) = zerith_tools::remote::wrap_remote_command(&target, &command);
            let status = std::process::Command::new(&prog)
                .args(&prog_args)
                .status()?;
            if status.success() {
                Ok(())
            } else {
                let code = status.code().unwrap_or(1) as u8;
                Err(Box::new(ExitWith(
                    code.max(1),
                    format!(
                        "comando remoto encerrou com código {}",
                        status.code().unwrap_or(1)
                    ),
                )))
            }
        }
        Some(other) => Err(format!(
            "subcomando remote desconhecido: {other} (use list|add|remove|exec)"
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flag_helpers() {
        let args = v(&["session", "list", "--limit", "7", "--to", "f"]);
        assert_eq!(flag_usize(&args, "--limit"), Some(7));
        assert_eq!(flag_value(&args, "--to").as_deref(), Some("f"));
        assert_eq!(flag_usize(&args, "--missing"), None);
    }

    #[test]
    fn chat_footer_renders_account_model_and_optional_token_savings() {
        let mut report = TurnReport {
            session_id: "s-1".into(),
            message_id: 9,
            text: "ok".into(),
            reasoning: None,
            prompt_tokens: 120,
            completion_tokens: 45,
            saved_rtk_tokens: 30,
            saved_ponytail_tokens: 15,
            saved_caveman_tokens: 5,
            saved_tokens: 50,
            account_id: Some("acct-a".into()),
            model_used: Some("gpt-4o".into()),
            ..Default::default()
        };
        let with_saved = format_chat_footer(&report, "fallback");
        assert_eq!(
            with_saved,
            "\n[turn] s-1  conta acct-a  modelo gpt-4o  tokens 120→45  saved 50 (rtk:30 ponytail:15 caveman:5)  msg 9"
        );

        report.saved_tokens = 0;
        report.account_id = None;
        report.model_used = None;
        let without_saved = format_chat_footer(&report, "fallback-m");
        assert_eq!(
            without_saved,
            "\n[turn] s-1  conta -  modelo fallback-m  tokens 120→45  msg 9"
        );
    }

    #[test]
    fn repl_slash_parser_recognizes_all_commands() {
        assert_eq!(parse_repl_slash("/help"), Some(ReplSlash::Help));
        assert_eq!(parse_repl_slash("  /quit "), Some(ReplSlash::Exit));
        assert_eq!(
            parse_repl_slash("/model claude-opus-4"),
            Some(ReplSlash::Model(Some("claude-opus-4")))
        );
        assert_eq!(
            parse_repl_slash("/mode plan"),
            Some(ReplSlash::Mode(Some("plan")))
        );
        assert_eq!(
            parse_repl_slash("/effort high"),
            Some(ReplSlash::Effort(Some("high")))
        );
        assert_eq!(
            parse_repl_slash("/rewind 3"),
            Some(ReplSlash::Rewind(Some(3)))
        );
        assert_eq!(parse_repl_slash("/rewind"), Some(ReplSlash::Rewind(None)));
        assert_eq!(
            parse_repl_slash("/sessions abc"),
            Some(ReplSlash::Sessions(Some("abc")))
        );
        assert_eq!(parse_repl_slash("/compact"), Some(ReplSlash::Compact));
        assert_eq!(parse_repl_slash("/clear"), Some(ReplSlash::Clear));
        assert_eq!(parse_repl_slash("oi tudo bem"), None);
    }
}
