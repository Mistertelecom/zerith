//! `zerith` — the binary. Phase 1 surface: version, paths, sessions, memory.
//!
//! Arguments are parsed by hand: six subcommands do not justify a dependency,
//! and an agent harness that shells out to itself should start fast.

use std::process::ExitCode;
use zerith_core::Paths;
use zerith_importers::{detect, import, ImportReport};
use zerith_mcp::Client as McpClient;
use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_plugins::{Library as Plugins, Status as PluginStatus};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_runtime::plan::{parse_plan, run_plan};
use zerith_runtime::{run_turn_in, RunEvent, TurnContext};
use zerith_security::{redact, Action, Policy, Verdict, PRESETS};
use zerith_sessions::{NewMessage, Role, SessionSource, Store, TaskState};
use zerith_skills::catalog;
use zerith_skills::{discover, scan, Library, Status};
use zerith_tools::{CallCtx, Registry};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // An error string can carry a response body, and a response body
            // can carry the key that was sent. Redact on the way out.
            eprintln!("zerith: {}", redact(&e.to_string()));
            ExitCode::FAILURE
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
  zerith session show <id>
  zerith memory remember <scope> <text> [--inferred]
  zerith memory recall <scope> <query>
  zerith memory list <scope>
  zerith memory forget <id>
  zerith memory edit <id> <text>
  zerith memory export <scope>
  zerith skills list|installed|catalog [--refresh]
  zerith skills install <dir> | remove <name> | enable|disable <name>
  zerith skills update <name> <dir> | rollback <name>
  zerith skills validate <dir>
  zerith tools [list|call <tool> [json-args]]
  zerith plugins [list|install <dir>|remove <name>|enable|disable <name>]
  zerith plugins trust <name> | tools <name>
  zerith plan run <file|-> [--agents N] [--model id]
  zerith plan list
  zerith mcp tools <command and args>
  zerith mcp call <command and args> -- <tool> [json-args]
  zerith providers [probe|models|chat <model> <prompt>]
  zerith chat [--session <id>] [--model <id>] <prompt>
  zerith import hermes [--profile <name>]
  zerith import detect"
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("{}", usage());
        return Ok(());
    };
    let paths = Paths::resolve();

    match (cmd, args.get(1).map(String::as_str)) {
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
        ("doctor", _) => {
            paths.ensure()?;
            let store = Store::open(paths.db())?;
            println!("db            {}", paths.db().display());
            println!("schema        v{}", store.schema_version()?);
            println!(
                "integrity     {}",
                if store.integrity_ok()? {
                    "ok"
                } else {
                    "FAILED"
                }
            );
            println!(
                "sessions      {}",
                store
                    .list_sessions(usize::MAX)
                    .map(|v| v.len())
                    .unwrap_or(0)
            );
        }
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
        ("session", Some("show")) => {
            let id = args.get(2).ok_or("session show needs an id")?;
            let store = Store::open(paths.db())?;
            let Some(s) = store.session(id)? else {
                return Err(format!("no session {id}").into());
            };
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
        ("plugins", _) => plugins_cmd(&paths, args)?,
        ("mcp", _) => mcp(args)?,
        ("plan", _) => plan(&paths, args)?,
        _ => {
            println!("{}", usage());
            return Err(format!("unknown command: {cmd}").into());
        }
    }
    Ok(())
}

/// Plugin metadata only. Nothing here spawns a process except `tools`, which
/// still requires the plugin to be enabled *and* reviewed.
fn plugins_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    paths.ensure()?;
    let lib = Plugins::open(&paths.root().join("plugins"))?;
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            let all = lib.plugins()?;
            if all.is_empty() {
                println!("(no plugins installed)");
            }
            for p in all {
                println!(
                    "{:<16} {:<8} {:<9} {} {}",
                    p.manifest.name,
                    p.manifest.version,
                    p.status.as_str(),
                    if p.trusted { "trusted" } else { "unreviewed" },
                    p.manifest.description
                );
            }
        }
        Some("install") => {
            let dir = args.get(2).ok_or("plugins install needs a directory")?;
            let p = lib.install(std::path::Path::new(dir))?;
            println!(
                "installed {} {} (disabled, unreviewed)",
                p.manifest.name, p.manifest.version
            );
        }
        Some("remove") => {
            let name = args.get(2).ok_or("plugins remove needs a name")?;
            lib.remove(name)?;
            println!("removed {name}");
        }
        Some("enable") => {
            let name = args.get(2).ok_or("plugins enable needs a name")?;
            lib.set_status(name, PluginStatus::Enabled)?;
            println!("enabled {name}");
        }
        Some("disable") => {
            let name = args.get(2).ok_or("plugins disable needs a name")?;
            lib.set_status(name, PluginStatus::Disabled)?;
            println!("disabled {name}");
        }
        Some("trust") => {
            let name = args.get(2).ok_or("plugins trust needs a name")?;
            lib.trust(name)?;
            println!("{name} marked as reviewed");
        }
        Some("tools") => {
            let name = args.get(2).ok_or("plugins tools needs a name")?;
            let mut client = lib.launch(name)?;
            for t in client.tools()? {
                println!("{}  {}", t.name, t.description.unwrap_or_default());
            }
        }
        Some(other) => return Err(format!("unknown plugins command: {other}").into()),
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
            let scope = Scope::user(&scope_id());
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
/// Speaks to one MCP server and prints what it advertises.
///
/// The command is split on whitespace so no shell is involved: a config value
/// that came from outside cannot become an extra command.
fn mcp(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args.get(1).ok_or("mcp needs a subcommand: tools | call")?;
    let rest = &args[2..];
    let (cmdline, tail) = match rest.iter().position(|a| a == "--") {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &rest[rest.len()..]),
    };
    let mut parts = cmdline.iter();
    let command = parts.next().ok_or("mcp needs a server command")?;
    let sargs: Vec<String> = parts.cloned().collect();
    let mut client = McpClient::connect("cli", command, &sargs)?;
    match sub.as_str() {
        "tools" => {
            let tools = client.tools()?;
            if tools.is_empty() {
                println!("(server advertises no tools)");
            }
            for t in tools {
                println!("{}  {}", t.name, t.description.unwrap_or_default());
            }
        }
        "call" => {
            let tool = tail.first().ok_or("mcp call needs a tool name after --")?;
            let raw = tail.get(1).map(String::as_str).unwrap_or("{}");
            let arguments: serde_json::Value = serde_json::from_str(raw)?;
            let result = client.call(tool, arguments)?;
            if result.is_error == Some(true) {
                eprintln!("zerith: the server reported the call as an error");
            }
            println!("{}", result.text());
        }
        other => return Err(format!("unknown mcp command: {other}").into()),
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
            let mut agents = 2usize;
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
                Some(Scope::user(&scope_id())),
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

/// Who the memory belongs to. One operator per install for now; a profile id
/// can override it without changing the schema.
fn scope_id() -> String {
    std::env::var("ZERITH_USER").unwrap_or_else(|_| whoami_fallback())
}

fn whoami_fallback() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "local".to_string())
        .to_lowercase()
}

/// One turn: resolve the session, stream the answer, persist it whole.
///
/// The turn runs on this thread. A CLI has nothing else to do while it waits,
/// so the extra thread `run_turn_async` exists for the window, not here.
fn chat(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // `--flag value` pairs are consumed first, so a model id is never
    // mistaken for the prompt. Everything left is user text, joined back.
    let mut positionals: Vec<&str> = Vec::new();
    let mut session_flag = None;
    let mut model_flag = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--session" || a == "--model" {
            let value = args.get(i + 1).cloned();
            if a == "--session" {
                session_flag = value;
            } else {
                model_flag = value;
            }
            i += 2;
            continue;
        }
        if a.starts_with("--") {
            i += 1;
            continue;
        }
        positionals.push(a);
        i += 1;
    }
    if positionals.is_empty() {
        return Err("chat needs a prompt".into());
    }
    let prompt = positionals.join(" ");

    let cfg = ProviderConfig::from_env();
    let model = model_flag
        .or_else(|| cfg.default_model.clone())
        .or_else(|| cfg.known_models.first().cloned())
        .ok_or("no model: pass --model or set ZERITH_MODEL")?;
    let client = Client::new(cfg)?;

    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let session_id = match session_flag {
        Some(id) => id,
        None => {
            let title: String = prompt.chars().take(48).collect();
            store.create_session(&title, SessionSource::Native)?.id
        }
    };

    // Memory shares the same database file: one store, one schema, no second
    // source of truth to keep in sync.
    let memory = SqliteMemory::open(paths.db())?;
    let scope = Scope::user(&scope_id());

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
            RunEvent::Done { .. } => {}
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
    let profile = args
        .iter()
        .position(|a| a == "--profile")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let sources = detect();
    let Some(first) = sources.first() else {
        return Err("no Hermes database found (~/.hermes/state.db)".into());
    };
    let picked = match &profile {
        Some(p) => sources
            .iter()
            .find(|s| &s.profile == p)
            .ok_or_else(|| format!("no Hermes profile named {p}"))?,
        None => first,
    };
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let report: ImportReport = import(&picked.path, &picked.profile, &store)?;
    println!(
        "sessions seen {}  imported {}  skipped {}  messages {}  skipped {}  failed {}",
        report.sessions_seen,
        report.sessions_imported,
        report.sessions_skipped,
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
            let name = args.get(2).ok_or("skills update needs a name")?;
            let from = args.get(3).ok_or("skills update needs a directory")?;
            paths.ensure()?;
            let fresh = Library::open(paths.skills())?.update(name, std::path::Path::new(from))?;
            println!(
                "updated {name} to {} file(s)",
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
