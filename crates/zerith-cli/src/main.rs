//! `zerith` — the binary. Phase 1 surface: version, paths, sessions, memory.
//!
//! Arguments are parsed by hand: six subcommands do not justify a dependency,
//! and an agent harness that shells out to itself should start fast.

use std::process::ExitCode;
use zerith_core::Paths;
use zerith_importers::{detect, import, ImportReport};
use zerith_mcp::Client as McpClient;
use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_runtime::{run_turn_with_memory, RunEvent};
use zerith_sessions::{SessionSource, Store};
use zerith_skills::{discover, scan};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zerith: {e}");
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
  zerith session list [--limit N]
  zerith session new <title>
  zerith session show <id>
  zerith memory remember <scope> <text> [--inferred]
  zerith memory recall <scope> <query>
  zerith memory list <scope>
  zerith memory forget <id>
  zerith skills list
  zerith skills validate <dir>
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
        ("mcp", _) => mcp(args)?,
        _ => {
            println!("{}", usage());
            return Err(format!("unknown command: {cmd}").into());
        }
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

    let mut streamed = false;
    let report = run_turn_with_memory(
        &store,
        &client,
        Some(&memory),
        Some(&scope),
        &session_id,
        &model,
        &prompt,
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
        other => return Err(format!("unknown skills command: {other}").into()),
    }
    Ok(())
}
