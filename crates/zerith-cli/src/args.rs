//! Argument parsing, by hand and pure: every function takes the argument
//! slice (command name first) and returns a typed value or a pt-BR error.
//! Nothing here touches the disk, the network or the environment.

use zerith_security::Mode as Permission;
use zerith_sessions::checkpoints::Mode as RewindMode;
use zerith_sessions::Store;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

/// Which session a command runs in.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Target {
    /// A fresh session.
    #[default]
    New,
    /// The most recently active, non-archived session.
    Latest,
    /// An id, a title or a unique id prefix (see `Store::resolve_session`).
    Key(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionArgs {
    pub target: Target,
    /// Continue in a copy, leaving the original untouched.
    pub fork: bool,
}

impl SessionArgs {
    fn set(&mut self, t: Target) -> Result<(), String> {
        if self.target != Target::New && self.target != t {
            return Err("use só um entre --session, --resume e --continue".into());
        }
        self.target = t;
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        if self.fork && self.target == Target::New {
            return Err("--fork precisa de --session, --resume ou --continue".into());
        }
        Ok(())
    }
}

/// Handles `--session X`, `--resume [X]`, `--continue|-c` and `--fork` at
/// `args[*i]`. `Ok(true)`: consumed (and `*i` advanced).
fn session_flag(args: &[String], i: &mut usize, out: &mut SessionArgs) -> Result<bool, String> {
    let a = args[*i].as_str();
    match a {
        "--session" => {
            let v = args
                .get(*i + 1)
                .ok_or("--session precisa de um valor")?
                .clone();
            out.set(Target::Key(v))?;
            *i += 2;
        }
        // The value is optional: `--resume -p x` means "the latest".
        "--resume" | "-r" => {
            match args.get(*i + 1).filter(|n| !n.starts_with('-')) {
                Some(v) => {
                    out.set(Target::Key(v.clone()))?;
                    *i += 2;
                }
                None => {
                    out.set(Target::Latest)?;
                    *i += 1;
                }
            };
        }
        "--continue" | "-c" => {
            out.set(Target::Latest)?;
            *i += 1;
        }
        "--fork" => {
            out.fork = true;
            *i += 1;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// `zerith chat` arguments. `--flag value` pairs are consumed first, so a
/// model id is never mistaken for the prompt; the rest is user text.
#[derive(Debug, PartialEq)]
pub struct ChatArgs {
    pub prompt: String,
    pub session: SessionArgs,
    pub model: Option<String>,
}

pub fn parse_chat(args: &[String]) -> Result<ChatArgs, String> {
    let mut out = ChatArgs {
        prompt: String::new(),
        session: SessionArgs::default(),
        model: None,
    };
    let mut words: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if session_flag(args, &mut i, &mut out.session)? {
            continue;
        }
        let a = args[i].as_str();
        if a == "--model" {
            out.model = Some(
                args.get(i + 1)
                    .ok_or("--model precisa de um valor")?
                    .clone(),
            );
            i += 2;
            continue;
        }
        if !a.starts_with("--") {
            words.push(a);
        }
        i += 1;
    }
    out.session.check()?;
    if words.is_empty() {
        return Err("chat precisa de um prompt".into());
    }
    out.prompt = words.join(" ");
    Ok(out)
}

/// `zerith -p <prompt> ...`
#[derive(Debug, PartialEq)]
pub struct Headless {
    pub prompt: String,
    pub format: OutputFormat,
    pub model: Option<String>,
    pub session: SessionArgs,
    /// Model requests per turn (`AgentOptions::max_steps`).
    pub max_turns: Option<usize>,
    pub max_budget_usd: Option<f64>,
    /// USD per million tokens; a budget cannot be computed without them.
    pub price_in: Option<f64>,
    pub price_out: Option<f64>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub permission_mode: Option<Permission>,
    /// Inline JSON (starts with `{`) or a file path.
    pub json_schema: Option<String>,
    pub no_tools: bool,
}

pub fn is_headless(args: &[String]) -> bool {
    args.iter().any(|a| a == "-p" || a == "--print")
}

/// Splits `Tool,Tool(pattern, with comma),Other` on the commas that are not
/// inside parentheses.
pub fn split_rules(list: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in list.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn number<T: std::str::FromStr>(name: &str, v: &str) -> Result<T, String> {
    v.parse()
        .map_err(|_| format!("{name} precisa de um número"))
}

pub fn parse_headless(args: &[String]) -> Result<Headless, String> {
    let mut out = Headless {
        prompt: String::new(),
        format: OutputFormat::Text,
        model: None,
        session: SessionArgs::default(),
        max_turns: None,
        max_budget_usd: None,
        price_in: None,
        price_out: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        permission_mode: None,
        json_schema: None,
        no_tools: false,
    };
    let mut prompt = None;
    let mut i = 0;
    while i < args.len() {
        if session_flag(args, &mut i, &mut out.session)? {
            continue;
        }
        let a = args[i].as_str();
        if a == "--no-tools" {
            out.no_tools = true;
            i += 1;
            continue;
        }
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{a} precisa de um valor"))
        };
        match a {
            "-p" | "--print" => prompt = Some(value()?),
            "--model" => out.model = Some(value()?),
            "--output-format" => {
                out.format = match value()?.as_str() {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    "stream-json" => OutputFormat::StreamJson,
                    other => return Err(format!("--output-format inválido: {other}")),
                }
            }
            "--max-turns" => {
                let n: usize = number(a, &value()?)?;
                if n == 0 {
                    return Err("--max-turns precisa ser pelo menos 1".into());
                }
                out.max_turns = Some(n);
            }
            "--max-budget-usd" => {
                let n: f64 = number(a, &value()?)?;
                if !n.is_finite() || n <= 0.0 {
                    return Err("--max-budget-usd precisa ser maior que zero".into());
                }
                out.max_budget_usd = Some(n);
            }
            "--price-in" => out.price_in = Some(number(a, &value()?)?),
            "--price-out" => out.price_out = Some(number(a, &value()?)?),
            "--allowed-tools" | "--allowedTools" => {
                out.allowed_tools.extend(split_rules(&value()?))
            }
            "--disallowed-tools" | "--disallowedTools" => {
                out.disallowed_tools.extend(split_rules(&value()?))
            }
            "--permission-mode" => {
                let v = value()?;
                out.permission_mode = Some(Permission::parse(&v).ok_or_else(|| {
                    format!(
                        "--permission-mode inválido: {v} (use default, acceptEdits, plan, dontAsk ou bypassPermissions)"
                    )
                })?)
            }
            "--json-schema" => out.json_schema = Some(value()?),
            other => return Err(format!("argumento desconhecido: {other}")),
        }
        i += 2;
    }
    out.session.check()?;
    out.prompt = prompt
        .filter(|p| !p.trim().is_empty())
        .ok_or("-p precisa de um prompt")?;
    Ok(out)
}

/// `zerith rewind <session> [--turn N] [--code|--conversation|--both] [--purge]`
#[derive(Debug, PartialEq)]
pub struct RewindArgs {
    pub session: String,
    pub turn: Option<i64>,
    /// `None`: only preview, change nothing.
    pub mode: Option<RewindMode>,
    /// Hard-delete the messages a conversation rewind set aside.
    pub purge: bool,
}

pub fn parse_rewind(args: &[String]) -> Result<RewindArgs, String> {
    let mut session = None;
    let mut out = RewindArgs {
        session: String::new(),
        turn: None,
        mode: None,
        purge: false,
    };
    let mut i = 1;
    while i < args.len() {
        let want = match args[i].as_str() {
            "--turn" => {
                let n: i64 = number(
                    "--turn",
                    args.get(i + 1).ok_or("--turn precisa de um valor")?,
                )?;
                if n < 1 {
                    return Err("--turn começa em 1".into());
                }
                out.turn = Some(n);
                i += 2;
                continue;
            }
            "--purge" => {
                out.purge = true;
                i += 1;
                continue;
            }
            "--code" => RewindMode::Code,
            "--conversation" => RewindMode::Conversation,
            "--both" => RewindMode::Both,
            s if s.starts_with("--") => return Err(format!("argumento desconhecido: {s}")),
            s => {
                if session.replace(s.to_string()).is_some() {
                    return Err("rewind recebe uma sessão só".into());
                }
                i += 1;
                continue;
            }
        };
        if out.mode.is_some_and(|m| m != want) {
            return Err("use só um entre --code, --conversation e --both".into());
        }
        out.mode = Some(want);
        i += 1;
    }
    out.session = session.ok_or("rewind precisa de uma sessão")?;
    if out.purge && matches!(out.mode, None | Some(RewindMode::Code)) {
        return Err("--purge só vale com --conversation ou --both".into());
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportFormat {
    Markdown,
    Jsonl,
}

/// `zerith export <session> [--format md|jsonl] [--out FILE]`
#[derive(Debug, PartialEq)]
pub struct ExportArgs {
    pub session: String,
    pub format: ExportFormat,
    pub out: Option<String>,
}

pub fn parse_export(args: &[String]) -> Result<ExportArgs, String> {
    let mut out = ExportArgs {
        session: String::new(),
        format: ExportFormat::Markdown,
        out: None,
    };
    let mut session = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                out.format = match args.get(i + 1).map(String::as_str) {
                    Some("md" | "markdown") => ExportFormat::Markdown,
                    Some("jsonl") => ExportFormat::Jsonl,
                    Some(o) => return Err(format!("--format inválido: {o} (use md ou jsonl)")),
                    None => return Err("--format precisa de um valor".into()),
                };
                i += 2;
            }
            "--out" => {
                out.out = Some(args.get(i + 1).ok_or("--out precisa de um valor")?.clone());
                i += 2;
            }
            s if s.starts_with("--") => return Err(format!("argumento desconhecido: {s}")),
            s => {
                if session.replace(s.to_string()).is_some() {
                    return Err("export recebe uma sessão só".into());
                }
                i += 1;
            }
        }
    }
    out.session = session.ok_or("export precisa de uma sessão")?;
    Ok(out)
}

/// `zerith search <query...> [--limit N]`
#[derive(Debug, PartialEq)]
pub struct SearchArgs {
    pub query: String,
    pub limit: usize,
}

pub fn parse_search(args: &[String]) -> Result<SearchArgs, String> {
    let (mut words, mut limit, mut i) = (Vec::new(), 20usize, 1);
    while i < args.len() {
        if args[i] == "--limit" {
            limit = number(
                "--limit",
                args.get(i + 1).ok_or("--limit precisa de um valor")?,
            )?;
            i += 2;
        } else {
            words.push(args[i].as_str());
            i += 1;
        }
    }
    let query = words.join(" ");
    if query.trim().is_empty() {
        return Err("search precisa de um texto".into());
    }
    Ok(SearchArgs { query, limit })
}

/// `zerith session rename <id|name> <new title...>`
pub fn parse_rename(args: &[String]) -> Result<(String, String), String> {
    let key = args.get(2).ok_or("session rename precisa de uma sessão")?;
    let title = args.get(3..).unwrap_or_default().join(" ");
    if title.trim().is_empty() {
        return Err("session rename precisa de um título novo".into());
    }
    Ok((key.clone(), title))
}

/// `zerith import hermes [--profile P] [--jsonl FILE]`
#[derive(Debug, PartialEq)]
pub struct ImportHermes {
    pub profile: Option<String>,
    pub jsonl: Option<String>,
}

pub fn parse_import_hermes(args: &[String]) -> Result<ImportHermes, String> {
    let mut out = ImportHermes {
        profile: None,
        jsonl: None,
    };
    let mut i = 2;
    while i < args.len() {
        let v = args.get(i + 1);
        match args[i].as_str() {
            "--profile" => out.profile = Some(v.ok_or("--profile precisa de um valor")?.clone()),
            "--jsonl" => out.jsonl = Some(v.ok_or("--jsonl precisa de um arquivo")?.clone()),
            other => return Err(format!("argumento desconhecido: {other}")),
        }
        i += 2;
    }
    Ok(out)
}

/// `zerith skills update <name> [dir]`: without a directory the source
/// recorded at install time is used.
pub fn parse_skills_update(args: &[String]) -> Result<(String, Option<String>), String> {
    let name = args.get(2).ok_or("skills update precisa de um nome")?;
    Ok((name.clone(), args.get(3).cloned()))
}

/// `zerith memory export [scope] | import <file> | pause | resume | status`
#[derive(Debug, PartialEq)]
pub enum MemoryCmd {
    Pause,
    Resume,
    Status,
    /// No scope: every scope, one JSON object per line.
    Export(Option<String>),
    Import(String),
}

pub fn parse_memory_admin(args: &[String]) -> Result<Option<MemoryCmd>, String> {
    Ok(Some(match args.get(1).map(String::as_str) {
        Some("pause") => MemoryCmd::Pause,
        Some("resume") => MemoryCmd::Resume,
        Some("status") => MemoryCmd::Status,
        Some("export") => MemoryCmd::Export(args.get(2).cloned()),
        Some("import") => MemoryCmd::Import(
            args.get(2)
                .ok_or("memory import precisa de um arquivo")?
                .clone(),
        ),
        _ => return Ok(None),
    }))
}

/// `zerith doctor [--offline]`
pub fn parse_doctor(args: &[String]) -> Result<bool, String> {
    let mut offline = false;
    for a in &args[1..] {
        match a.as_str() {
            "--offline" => offline = true,
            other => return Err(format!("argumento desconhecido: {other}")),
        }
    }
    Ok(offline)
}

/// Picks (or creates, or forks) the session a command runs in. `hint` names a
/// new session or a fork.
pub fn select_session(store: &Store, sel: &SessionArgs, hint: &str) -> Result<String, String> {
    let title: String = hint.chars().take(48).collect();
    let base = match &sel.target {
        Target::New => {
            return store
                .create_session(&title, zerith_sessions::SessionSource::Native)
                .map(|s| s.id)
                .map_err(|e| e.to_string())
        }
        Target::Latest => store
            .latest_session()
            .map_err(|e| e.to_string())?
            .ok_or("não há sessão anterior para continuar")?,
        Target::Key(k) => store.resolve_session(k).map_err(|e| e.to_string())?,
    };
    if !sel.fork {
        return Ok(base.id);
    }
    store
        .fork_session(&base.id, None, &format!("{} (fork)", base.title))
        .map(|s| s.id)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_sessions::{NewMessage, Role, SessionSource};

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn chat_flags_never_become_the_prompt() {
        let a = parse_chat(&v(&[
            "chat",
            "--model",
            "m1",
            "oi",
            "--session",
            "s",
            "mundo",
        ]))
        .unwrap();
        assert_eq!(
            a,
            ChatArgs {
                prompt: "oi mundo".into(),
                session: SessionArgs {
                    target: Target::Key("s".into()),
                    fork: false
                },
                model: Some("m1".into())
            }
        );
        assert!(parse_chat(&v(&["chat", "--model", "m1"])).is_err());
        let c = parse_chat(&v(&["chat", "-c", "--fork", "continue"])).unwrap();
        assert_eq!(c.session.target, Target::Latest);
        assert!(c.session.fork);
    }

    #[test]
    fn headless_parses_every_flag_in_any_order() {
        let args = v(&[
            "--output-format",
            "stream-json",
            "-p",
            "olá",
            "--model",
            "m",
            "--resume",
            "s1",
            "--max-turns",
            "3",
            "--max-budget-usd",
            "0.5",
            "--price-in",
            "1.5",
            "--price-out",
            "6",
            "--allowed-tools",
            "read_file,shell(cargo test*, cargo build*)",
            "--disallowed-tools",
            "write_file",
            "--permission-mode",
            "acceptEdits",
            "--json-schema",
            "{\"type\":\"object\"}",
            "--no-tools",
        ]);
        assert!(is_headless(&args));
        let h = parse_headless(&args).unwrap();
        assert_eq!(h.prompt, "olá");
        assert_eq!(h.format, OutputFormat::StreamJson);
        assert_eq!(h.model.as_deref(), Some("m"));
        assert_eq!(h.session.target, Target::Key("s1".into()));
        assert_eq!(h.max_turns, Some(3));
        assert_eq!(h.max_budget_usd, Some(0.5));
        assert_eq!((h.price_in, h.price_out), (Some(1.5), Some(6.0)));
        assert_eq!(
            h.allowed_tools,
            ["read_file", "shell(cargo test*, cargo build*)"]
        );
        assert_eq!(h.disallowed_tools, ["write_file"]);
        assert_eq!(h.permission_mode, Some(Permission::AcceptEdits));
        assert!(h.json_schema.is_some() && h.no_tools);
        assert_eq!(
            parse_headless(&v(&["-p", "x"])).unwrap().format,
            OutputFormat::Text
        );
    }

    #[test]
    fn resume_value_is_optional_and_sources_do_not_mix() {
        let h = parse_headless(&v(&["--resume", "-p", "x"])).unwrap();
        assert_eq!(h.session.target, Target::Latest);
        assert_eq!(
            parse_headless(&v(&["-p", "x", "--continue"]))
                .unwrap()
                .session
                .target,
            Target::Latest
        );
        assert!(parse_headless(&v(&["-p", "x", "--continue", "--session", "a"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--fork"])).is_err());
        assert!(
            parse_headless(&v(&["-p", "x", "--session", "a", "--fork"]))
                .unwrap()
                .session
                .fork
        );
    }

    #[test]
    fn headless_rejects_bad_input() {
        assert!(parse_headless(&v(&["-p"])).is_err());
        assert!(parse_headless(&v(&["-p", " "])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--output-format", "xml"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--bogus"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--max-turns", "0"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--max-turns", "abc"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--max-budget-usd", "-1"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--max-budget-usd", "nan"])).is_err());
        assert!(parse_headless(&v(&["-p", "x", "--permission-mode", "yolo"])).is_err());
        assert!(!is_headless(&v(&["chat", "oi"])));
    }

    #[test]
    fn rule_lists_split_outside_parentheses_only() {
        assert_eq!(split_rules("a, b(x,y) ,,c"), ["a", "b(x,y)", "c"]);
        assert!(split_rules("").is_empty());
    }

    #[test]
    fn rewind_needs_a_mode_to_change_anything() {
        let r = parse_rewind(&v(&["rewind", "s1", "--turn", "3", "--both", "--purge"])).unwrap();
        assert_eq!(
            r,
            RewindArgs {
                session: "s1".into(),
                turn: Some(3),
                mode: Some(RewindMode::Both),
                purge: true
            }
        );
        assert_eq!(parse_rewind(&v(&["rewind", "s1"])).unwrap().mode, None);
        assert!(parse_rewind(&v(&["rewind"])).is_err());
        assert!(parse_rewind(&v(&["rewind", "s", "--code", "--both"])).is_err());
        assert!(parse_rewind(&v(&["rewind", "s", "--turn", "0"])).is_err());
        assert!(parse_rewind(&v(&["rewind", "s", "--code", "--purge"])).is_err());
        assert!(parse_rewind(&v(&["rewind", "a", "b"])).is_err());
    }

    #[test]
    fn export_search_rename_and_import_parsers() {
        let e = parse_export(&v(&["export", "s", "--format", "jsonl", "--out", "f"])).unwrap();
        assert_eq!(e.format, ExportFormat::Jsonl);
        assert_eq!(e.out.as_deref(), Some("f"));
        assert_eq!(
            parse_export(&v(&["export", "s"])).unwrap().format,
            ExportFormat::Markdown
        );
        assert!(parse_export(&v(&["export", "s", "--format", "pdf"])).is_err());
        assert!(parse_export(&v(&["export"])).is_err());

        let s = parse_search(&v(&["search", "girafa", "azul", "--limit", "5"])).unwrap();
        assert_eq!((s.query.as_str(), s.limit), ("girafa azul", 5));
        assert!(parse_search(&v(&["search"])).is_err());

        assert_eq!(
            parse_rename(&v(&["session", "rename", "a", "novo", "nome"])).unwrap(),
            ("a".into(), "novo nome".into())
        );
        assert!(parse_rename(&v(&["session", "rename", "a"])).is_err());

        let i = parse_import_hermes(&v(&[
            "import",
            "hermes",
            "--jsonl",
            "x.jsonl",
            "--profile",
            "p",
        ]))
        .unwrap();
        assert_eq!(i.jsonl.as_deref(), Some("x.jsonl"));
        assert_eq!(i.profile.as_deref(), Some("p"));
        assert!(parse_import_hermes(&v(&["import", "hermes", "--jsonl"])).is_err());
    }

    #[test]
    fn memory_and_doctor_and_skills_parsers() {
        assert_eq!(
            parse_memory_admin(&v(&["memory", "pause"])).unwrap(),
            Some(MemoryCmd::Pause)
        );
        assert_eq!(
            parse_memory_admin(&v(&["memory", "export"])).unwrap(),
            Some(MemoryCmd::Export(None))
        );
        assert_eq!(
            parse_memory_admin(&v(&["memory", "export", "user:x"])).unwrap(),
            Some(MemoryCmd::Export(Some("user:x".into())))
        );
        assert!(parse_memory_admin(&v(&["memory", "import"])).is_err());
        assert_eq!(parse_memory_admin(&v(&["memory", "recall"])).unwrap(), None);
        assert!(parse_doctor(&v(&["doctor", "--offline"])).unwrap());
        assert!(!parse_doctor(&v(&["doctor"])).unwrap());
        assert!(parse_doctor(&v(&["doctor", "--x"])).is_err());
        assert_eq!(
            parse_skills_update(&v(&["skills", "update", "n"])).unwrap(),
            ("n".into(), None)
        );
        assert!(parse_skills_update(&v(&["skills", "update"])).is_err());
    }

    #[test]
    fn select_session_creates_resumes_forks_and_explains_failures() {
        let s = Store::open_in_memory().unwrap();
        // Nothing to continue yet.
        let latest = SessionArgs {
            target: Target::Latest,
            fork: false,
        };
        assert!(select_session(&s, &latest, "x").is_err());

        let fresh = select_session(&s, &SessionArgs::default(), "primeira conversa").unwrap();
        s.append(&fresh, &NewMessage::new(Role::User, "oi"))
            .unwrap();
        assert_eq!(select_session(&s, &latest, "x").unwrap(), fresh);
        let by_name = SessionArgs {
            target: Target::Key("PRIMEIRA conversa".into()),
            fork: false,
        };
        assert_eq!(select_session(&s, &by_name, "x").unwrap(), fresh);

        let forked = select_session(
            &s,
            &SessionArgs {
                target: Target::Key(fresh.clone()),
                fork: true,
            },
            "x",
        )
        .unwrap();
        assert_ne!(forked, fresh);
        assert_eq!(s.message_count(&forked).unwrap(), 1);
        assert_eq!(
            s.session(&forked).unwrap().unwrap().title,
            "primeira conversa (fork)"
        );
        assert_eq!(
            s.session(&forked).unwrap().unwrap().source,
            SessionSource::Native
        );
        assert!(select_session(
            &s,
            &SessionArgs {
                target: Target::Key("nada-assim".into()),
                fork: false
            },
            "x"
        )
        .is_err());
    }
}
