//! `zerith` — the binary. Phase 1 surface: version, paths, sessions, memory.
//!
//! Arguments are parsed by hand: six subcommands do not justify a dependency,
//! and an agent harness that shells out to itself should start fast.

use std::process::ExitCode;
use zerith_core::Paths;
use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope, SqliteMemory};
use zerith_sessions::{SessionSource, Store};

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
  zerith memory forget <id>"
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
        ("memory", Some(sub)) => memory(&paths, args, sub)?,
        _ => {
            println!("{}", usage());
            return Err(format!("unknown command: {cmd}").into());
        }
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

fn flag_usize(args: &[String], name: &str) -> Option<usize> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1)?.parse().ok()
}
