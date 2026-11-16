//! `zerith` — the binary. Phase 1 surface: version, paths, sessions, memory.
//!
//! Arguments are parsed by hand: six subcommands do not justify a dependency,
//! and an agent harness that shells out to itself should start fast.

mod args;
mod cmds;
mod headless;
mod schema;
mod signals;

use std::process::ExitCode;
use zerith_core::{secrets, Paths, Settings};
use zerith_importers::{detect, import, import_jsonl, ImportReport};
use zerith_memory::{default_scope, Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_runtime::plan::{parse_plan, run_plan};
use zerith_runtime::{run_turn_in, RunEvent, TurnContext};
use zerith_security::{redact, Action, Policy, Verdict, PRESETS};
use zerith_sessions::{NewMessage, Role, SessionSource, Store, TaskState};
use zerith_skills::catalog;
use zerith_skills::{discover, scan, Library, Status};
use zerith_tools::{CallCtx, Registry};

use crate::args::{parse_chat, parse_import_hermes, parse_rename, select_session, SessionArgs};
use crate::headless::{load_settings, provider_config, ExitWith};

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
  zerith providers [probe|models|chat <model> <prompt>]
  zerith chat [--session <id>|--resume [id|name]|--continue] [--fork] [--model <id>] <prompt>
  zerith -p <prompt> [--output-format text|json|stream-json] [--model M]
        [--session ID | --resume [id|name] | --continue] [--fork]
        [--max-turns N] [--max-budget-usd X --price-in P --price-out P]
        [--allowed-tools a,b] [--disallowed-tools a,b] [--permission-mode MODE]
        [--json-schema <json|file>] [--no-tools]
  zerith config show
  zerith secret set <name> [value] | get <name> | delete <name>
  zerith import hermes [--profile <name>] [--jsonl <file>]
  zerith import detect"
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
        ("providers", _) => providers(args)?,
        ("chat", _) => chat(&paths, args)?,
        ("import", Some("hermes")) => import_hermes(&paths, args)?,
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
            let policy = Policy::from_env(paths.root())?;
            let memory = SqliteMemory::open(paths.db()).ok();
            let scope = default_scope();
            let ctx = CallCtx {
                workspace: paths.root(),
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
    let policy = Policy::from_env(paths.root())?;
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
                Some(default_scope()),
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

/// One turn: resolve the session, stream the answer, persist it whole.
///
/// The turn runs on this thread. A CLI has nothing else to do while it waits,
/// so the extra thread `run_turn_async` exists for the window, not here.
fn chat(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let args::ChatArgs {
        prompt,
        session: session_flag,
        model: model_flag,
    } = parse_chat(args)?;

    let settings = load_settings(paths);
    let cfg = provider_config(&settings)?;
    let model = model_flag
        .or_else(|| cfg.default_model.clone())
        .or_else(|| cfg.known_models.first().cloned())
        .ok_or("no model: pass --model or set ZERITH_MODEL")?;
    let client = Client::new(cfg)?;

    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let session_id = select_session(&store, &session_flag, &prompt)?;
    let _: &SessionArgs = &session_flag;

    // Memory shares the same database file: one store, one schema, no second
    // source of truth to keep in sync.
    let memory = SqliteMemory::open(paths.db())?;
    let scope = default_scope();

    // Installed skills are consulted, but only their names and descriptions
    // reach the prompt unless the user named one.
    let skills = Library::open(paths.skills()).ok();

    let mut streamed = false;
    let report = run_turn_in(
        &store,
        &client,
        TurnContext {
            memory: Some(&memory),
            scope: Some(&scope),
            skills: skills.as_ref(),
            ..TurnContext::default()
        },
        &session_id,
        &model,
        &prompt,
        &Cancel::default(),
        |e| match e {
            RunEvent::Delta(text) => {
                streamed = true;
                println!("{text}");
            }
            RunEvent::Reasoning(r) => eprintln!("[reasoning] {r}"),
            _ => {}
        },
    )?;
    if !streamed {
        println!("{}", report.text);
    }
    eprintln!(
        "\n[turn] {}  tokens {}→{}  msg {}",
        report.session_id, report.prompt_tokens, report.completion_tokens, report.message_id
    );
    Ok(())
}

fn providers(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = ProviderConfig::from_env();
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
                .or_else(|| cfg.default_model.clone())
                .ok_or("providers chat needs a model (or ZERITH_MODEL)")?;
            let prompt = args
                .get(3)
                .cloned()
                .ok_or("providers chat needs a prompt")?;
            let client = Client::new(cfg)?;
            let req = ChatRequest::new(model, vec![ChatMessage::user(prompt)]);
            let out = client.complete(&req, &Default::default())?;
            if let Some(r) = out.reasoning {
                eprintln!("[reasoning]\n{r}");
            }
            println!("{}", out.text);
        }
        Some(other) => return Err(format!("unknown providers command: {other}").into()),
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
}
