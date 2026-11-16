//! Session tooling, checkpoints, MCP, plugins, memory admin and `doctor`.
//! The pure argument parsing lives in `args`; this file does the I/O.

use std::io::{BufReader, Write};
use std::path::Path;

use zerith_core::{secrets, Paths};
use zerith_mcp::config::{
    add_server, approve_server, load_servers, parse_add, remove_server, user_file,
};
use zerith_mcp::{Client as McpClient, McpRegistry};
use zerith_memory::{Scope, SqliteMemory};
use zerith_plugins::{Library as Plugins, Status as PluginStatus};
use zerith_providers::Client;
use zerith_security::Policy;
use zerith_sessions::checkpoints::{self, Mode as RewindMode};
use zerith_sessions::Store;

use crate::args::{
    parse_doctor, parse_export, parse_memory_admin, parse_rewind, parse_search, ExportFormat,
    MemoryCmd,
};
use crate::headless::{load_settings, provider_config};

pub type Res = Result<(), Box<dyn std::error::Error>>;

fn when(ms: i64) -> String {
    // Seconds since the epoch: no date library for one column.
    format!("t={}", ms / 1000)
}

pub fn checkpoints_cmd(paths: &Paths, args: &[String]) -> Res {
    let key = args.get(1).ok_or("checkpoints precisa de uma sessão")?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let s = store.resolve_session(key)?;
    let rows = checkpoints::list(&store, paths.root(), &s.id)?;
    if rows.is_empty() {
        println!("(nenhum checkpoint em {})", s.id);
    }
    for c in rows {
        println!(
            "turno {:>3}  {}  arquivos {:>3}  mensagens após o ponto: {}{}",
            c.turn,
            when(c.created_at),
            c.files,
            c.messages_after,
            if c.code_restored {
                "  (código já restaurado)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

pub fn rewind_cmd(paths: &Paths, args: &[String]) -> Res {
    let a = parse_rewind(args)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let s = store.resolve_session(&a.session)?;
    let rows = checkpoints::list(&store, paths.root(), &s.id)?;
    let turn = match a.turn {
        Some(t) => t,
        None => rows
            .last()
            .map(|c| c.turn)
            .ok_or("esta sessão não tem checkpoints")?,
    };
    let Some(mode) = a.mode else {
        let p = checkpoints::preview(&store, paths.root(), &s.id, turn)?;
        println!("rewind do turno {turn} em {} (nada foi alterado):", s.id);
        println!("  mensagens que sairiam da conversa: {}", p.messages_after);
        for (path, existed) in &p.files {
            println!(
                "  {}  {path}",
                if *existed { "restaurar" } else { "apagar   " }
            );
        }
        println!("escolha --code, --conversation ou --both para aplicar.");
        println!("{}", LIMITS);
        return Ok(());
    };
    let report = checkpoints::restore(&store, paths.root(), &s.id, turn, mode)?;
    println!(
        "turno {turn}: {} arquivo(s) restaurado(s), {} removido(s), {} mensagem(ns) arquivada(s)",
        report.files_restored, report.files_removed, report.messages_archived
    );
    for e in &report.errors {
        eprintln!("zerith: {e}");
    }
    if a.purge {
        let n = checkpoints::purge_superseded(&store, &s.id)?;
        println!("{n} mensagem(ns) apagada(s) de vez (--purge)");
    } else if matches!(mode, RewindMode::Conversation | RewindMode::Both) {
        println!("as mensagens foram arquivadas, não apagadas; --purge as remove");
    }
    println!("{}", LIMITS);
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} arquivo(s) não puderam ser restaurados",
            report.errors.len()
        )
        .into())
    }
}

const LIMITS: &str = "limite: só edições feitas pelas ferramentas de arquivo são rastreadas; \
mudanças feitas por comandos de shell não são desfeitas.";

pub fn export_cmd(paths: &Paths, args: &[String]) -> Res {
    let a = parse_export(args)?;
    let store = Store::open(paths.db())?;
    let s = store.resolve_session(&a.session)?;
    let mut w: Box<dyn Write> = match &a.out {
        Some(f) => Box::new(std::io::BufWriter::new(std::fs::File::create(f)?)),
        None => Box::new(std::io::stdout().lock()),
    };
    match a.format {
        ExportFormat::Markdown => store.export_session_markdown(&s.id, &mut w)?,
        ExportFormat::Jsonl => store.export_session_jsonl(&s.id, &mut w)?,
    }
    w.flush()?;
    Ok(())
}

pub fn search_cmd(paths: &Paths, args: &[String]) -> Res {
    let a = parse_search(args)?;
    let store = Store::open(paths.db())?;
    let hits = store.search_messages(&a.query, a.limit)?;
    if hits.is_empty() {
        println!("(nada encontrado)");
    }
    for h in hits {
        let title = store
            .session(&h.session_id)?
            .map(|s| s.title)
            .unwrap_or_default();
        println!(
            "{}  msg {:>6}  {}\n    {}",
            h.session_id,
            h.message_id,
            title,
            h.snippet.replace('\n', " ")
        );
    }
    Ok(())
}

pub fn agents_cmd(paths: &Paths, args: &[String]) -> Res {
    match args.get(1).map(String::as_str) {
        None | Some("list") => {}
        Some(o) => return Err(format!("comando desconhecido: agents {o}").into()),
    }
    let cwd = std::env::current_dir().ok();
    let defs = zerith_core::agents::list(paths, cwd.as_deref());
    if defs.is_empty() {
        println!(
            "(nenhum agente; crie arquivos .md em {} ou .zerith/agents/)",
            paths.agents().display()
        );
    }
    for d in defs {
        println!(
            "{:<20} {:<8} {:<16} {}",
            d.name,
            d.layer,
            d.model.as_deref().unwrap_or("-"),
            d.description
        );
    }
    if let Ok(lib) = Plugins::open(&paths.root().join("plugins")) {
        for (plugin, file) in lib.contributions().map(|c| c.agents).unwrap_or_default() {
            println!(
                "{:<20} plugin   (declarado por {plugin}, ainda não carregado pelo runtime)",
                file.file_stem().and_then(|s| s.to_str()).unwrap_or("?")
            );
        }
    }
    Ok(())
}

/// `memory pause|resume|status|export|import`. `Ok(false)`: not one of these,
/// the caller handles the older subcommands.
pub fn memory_admin(paths: &Paths, args: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
    let Some(cmd) = parse_memory_admin(args)? else {
        return Ok(false);
    };
    paths.ensure()?;
    let mem = SqliteMemory::open(paths.db())?;
    match cmd {
        MemoryCmd::Pause => {
            mem.set_paused(true)?;
            println!("extração automática pausada (gravada no banco)");
            println!("`memory remember` continua funcionando; a recuperação não muda.");
        }
        MemoryCmd::Resume => {
            mem.set_paused(false)?;
            println!("extração automática retomada");
        }
        MemoryCmd::Status => {
            let settings = load_settings(paths);
            println!(
                "extração (banco)     {}",
                if mem.is_paused()? { "pausada" } else { "ativa" }
            );
            println!(
                "memória em -p        {}",
                if settings.memory.paused {
                    "desligada por [memory] paused = true"
                } else {
                    "ligada"
                }
            );
        }
        MemoryCmd::Export(scope) => {
            let scope = scope.map(Scope);
            let n = mem.export_jsonl(scope.as_ref(), std::io::stdout().lock())?;
            eprintln!("{n} fato(s) exportado(s)");
        }
        MemoryCmd::Import(file) => {
            let f = std::fs::File::open(&file).map_err(|e| format!("{file}: {e}"))?;
            let n = mem.import_jsonl(BufReader::new(f))?;
            println!("{n} linha(s) aplicada(s); fatos repetidos não são duplicados");
        }
    }
    Ok(true)
}

fn project_dir() -> Option<std::path::PathBuf> {
    std::env::current_dir().ok()
}

/// `mcp list|tools|call|add|remove|approve`. The older forms that took a raw
/// server command (`mcp tools <cmd..>`, `mcp call <cmd..> -- <tool>`) still
/// work when the first word is not a configured server.
pub fn mcp_cmd(paths: &Paths, args: &[String]) -> Res {
    let sub = args.get(1).map(String::as_str).unwrap_or("list");
    let reg = McpRegistry::load(paths, project_dir().as_deref());
    let configured = |name: &str| reg.server_names().contains(&name);
    match sub {
        "list" => {
            let loaded = load_servers(paths, project_dir().as_deref());
            if loaded.servers.is_empty() && loaded.unapproved.is_empty() {
                println!("(nenhum servidor MCP; use `zerith mcp add`)");
            }
            for s in &loaded.servers {
                println!(
                    "{:<16} {:<8} {}",
                    s.name,
                    if s.is_http() { "http" } else { "stdio" },
                    s.url.clone().unwrap_or_else(|| format!(
                        "{} {}",
                        s.command.clone().unwrap_or_default(),
                        s.args.join(" ")
                    ))
                );
            }
            for s in &loaded.unapproved {
                println!(
                    "{:<16} {:<8} do projeto, NÃO aprovado: zerith mcp approve {}",
                    s.name, "-", s.name
                );
            }
            for e in &loaded.errors {
                eprintln!("zerith: {e}");
            }
            println!("config do usuário: {}", user_file(paths).display());
        }
        "add" => {
            let server = parse_add(&args[2..])?;
            let name = server.name.clone();
            add_server(paths, server)?;
            println!("servidor {name} gravado em {}", user_file(paths).display());
        }
        "remove" => {
            let name = args.get(2).ok_or("mcp remove precisa de um nome")?;
            if remove_server(paths, name)? {
                println!("removido {name}");
            } else {
                return Err(format!(
                    "{name} não está em {}; se vem de settings.toml, edite o arquivo",
                    user_file(paths).display()
                )
                .into());
            }
        }
        "approve" => {
            let name = args.get(2).ok_or("mcp approve precisa de um nome")?;
            let loaded = load_servers(paths, project_dir().as_deref());
            let s = loaded
                .unapproved
                .iter()
                .find(|s| &s.name == name)
                .ok_or_else(|| format!("nenhum servidor de projeto pendente chamado {name}"))?;
            println!(
                "o projeto quer rodar: {}",
                s.url.clone().unwrap_or_else(|| format!(
                    "{} {}",
                    s.command.clone().unwrap_or_default(),
                    s.args.join(" ")
                ))
            );
            if !args.iter().any(|a| a == "--yes") {
                return Err("revise o comando acima e repita com --yes para aprovar".into());
            }
            approve_server(paths, name)?;
            println!("{name} aprovado");
        }
        "tools" | "call"
            if args.get(2).is_some_and(|n| configured(n)) && !args.contains(&"--".to_string()) =>
        {
            let server = &args[2];
            if sub == "tools" {
                for t in reg.list_tools(server)? {
                    println!("{}  {}", t.name, t.description.unwrap_or_default());
                }
            } else {
                let tool = args
                    .get(3)
                    .ok_or("mcp call <servidor> <ferramenta> [json]")?;
                let raw = args.get(4).map(String::as_str).unwrap_or("{}");
                let result = reg.call_tool(server, tool, serde_json::from_str(raw)?)?;
                if result.is_error == Some(true) {
                    eprintln!("zerith: o servidor marcou a chamada como erro");
                }
                println!("{}", result.text());
            }
        }
        "tools" | "call" => legacy_mcp(args)?,
        other => return Err(format!("comando desconhecido: mcp {other}").into()),
    }
    Ok(())
}

/// Speaks to one MCP server given as a raw command line.
///
/// The command is split on whitespace so no shell is involved: a config value
/// that came from outside cannot become an extra command.
fn legacy_mcp(args: &[String]) -> Res {
    let sub = &args[1];
    let rest = &args[2..];
    let (cmdline, tail) = match rest.iter().position(|a| a == "--") {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &rest[rest.len()..]),
    };
    let mut parts = cmdline.iter();
    let command = parts
        .next()
        .ok_or("mcp precisa de um servidor ou comando")?;
    let sargs: Vec<String> = parts.cloned().collect();
    let mut client = McpClient::connect("cli", command, &sargs)?;
    if sub == "tools" {
        let tools = client.tools()?;
        if tools.is_empty() {
            println!("(o servidor não anuncia ferramentas)");
        }
        for t in tools {
            println!("{}  {}", t.name, t.description.unwrap_or_default());
        }
    } else {
        let tool = tail
            .first()
            .ok_or("mcp call precisa de uma ferramenta após --")?;
        let raw = tail.get(1).map(String::as_str).unwrap_or("{}");
        let result = client.call(tool, serde_json::from_str(raw)?)?;
        if result.is_error == Some(true) {
            eprintln!("zerith: o servidor marcou a chamada como erro");
        }
        println!("{}", result.text());
    }
    Ok(())
}

/// `plugin list|show|install|enable|disable|trust|untrust|remove|tools`.
/// Metadata only; nothing here spawns a process except `tools`, which still
/// requires the plugin to be enabled *and* reviewed.
pub fn plugin_cmd(paths: &Paths, args: &[String]) -> Res {
    paths.ensure()?;
    let lib = Plugins::open(&paths.root().join("plugins"))?;
    let name = |what: &str| {
        args.get(2)
            .cloned()
            .ok_or_else(|| format!("plugin {what} precisa de um nome"))
    };
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            let all = lib.plugins()?;
            if all.is_empty() {
                println!("(nenhum plugin instalado)");
            }
            for p in all {
                let m = &p.manifest;
                println!(
                    "{:<16} {:<8} {:<9} {:<10} skills {} agentes {} hooks {} mcp {}  {}",
                    m.name,
                    m.version,
                    p.status.as_str(),
                    if p.trusted {
                        "revisado"
                    } else {
                        "não revisado"
                    },
                    m.skills.len(),
                    m.agents.len(),
                    m.hooks.len(),
                    m.mcp.len(),
                    m.description
                );
            }
        }
        Some("show") => print_review(&lib.get(&name("show")?)?),
        Some("install") => {
            let dir = args.get(2).ok_or("plugin install precisa de uma pasta")?;
            let p = lib.install(Path::new(dir))?;
            println!(
                "{} {} instalado (desativado, não revisado)",
                p.manifest.name, p.manifest.version
            );
            println!(
                "veja o que ele pede com `zerith plugin show {}`",
                p.manifest.name
            );
        }
        Some("remove") => {
            let n = name("remove")?;
            lib.remove(&n)?;
            println!("removido {n}");
        }
        Some("enable") => {
            let n = name("enable")?;
            lib.set_status(&n, PluginStatus::Enabled)?;
            println!("{n} ativado");
            if !lib.get(&n)?.trusted {
                println!("ainda não revisado: nada dele carrega até `zerith plugin trust {n}`");
            }
        }
        Some("disable") => {
            let n = name("disable")?;
            lib.set_status(&n, PluginStatus::Disabled)?;
            println!("{n} desativado");
        }
        Some("trust") => {
            let n = name("trust")?;
            print_review(&lib.get(&n)?);
            if !args.iter().any(|a| a == "--yes") {
                return Err("revise o que ele executa acima e repita com --yes".into());
            }
            lib.trust(&n)?;
            println!("{n} marcado como revisado");
        }
        Some("untrust") => {
            let n = name("untrust")?;
            lib.untrust(&n)?;
            println!("{n} voltou a não revisado");
        }
        Some("tools") => {
            let mut client = lib.launch(&name("tools")?)?;
            for t in client.tools()? {
                println!("{}  {}", t.name, t.description.unwrap_or_default());
            }
        }
        Some(other) => return Err(format!("comando desconhecido: plugin {other}").into()),
    }
    Ok(())
}

fn print_review(p: &zerith_plugins::Plugin) {
    let m = &p.manifest;
    println!("{} {}  {}", m.name, m.version, m.description);
    println!("pasta        {}", p.dir.display());
    if !m.command.is_empty() {
        println!("processo     {} {}", m.command, m.args.join(" "));
    }
    for s in &m.skills {
        println!("skill        {s}");
    }
    for a in &m.agents {
        println!("agente       {a}");
    }
    for h in &m.hooks {
        println!("hook         {} -> {}", h.event, h.command);
    }
    for e in &m.mcp {
        println!(
            "mcp          {} -> {}",
            e.name,
            e.url.clone().unwrap_or_else(|| format!(
                "{} {}",
                e.command.clone().unwrap_or_default(),
                e.args.join(" ")
            ))
        );
    }
    println!("tudo isso roda com os seus privilégios, sem sandbox.");
}

/// What `doctor` found, one line each. Pure so it can be tested.
#[derive(Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub fn keyring_check() -> Check {
    // A name no one uses: NotFound means the store answered, Unavailable means
    // it cannot be used at all.
    let (ok, detail) = match secrets::get_secret("zerith-doctor-probe") {
        Ok(_) | Err(secrets::SecretError::NotFound(_)) => (true, "disponível".to_string()),
        Err(secrets::SecretError::Unavailable(why)) => {
            (false, format!("indisponível ({why}); use ZERITH_API_KEY"))
        }
    };
    Check {
        name: "cofre de credenciais",
        ok,
        detail,
    }
}

pub fn doctor(paths: &Paths, args: &[String]) -> Res {
    let offline = parse_doctor(args)?;
    paths.ensure()?;
    let mut checks: Vec<Check> = Vec::new();
    let store = Store::open(paths.db())?;
    println!("db            {}", paths.db().display());
    println!("schema        v{}", store.schema_version()?);
    println!(
        "sessões       {}",
        store
            .list_sessions(usize::MAX)
            .map(|v| v.len())
            .unwrap_or(0)
    );
    checks.push(Check {
        name: "integridade do banco",
        ok: store.integrity_ok()?,
        detail: "PRAGMA integrity_check".into(),
    });

    let cwd = project_dir();
    let loaded = zerith_core::Settings::load(paths, cwd.as_deref());
    checks.push(Check {
        name: "configuração",
        ok: loaded.errors.is_empty(),
        detail: if loaded.errors.is_empty() {
            "sem erros".into()
        } else {
            loaded.errors.join(" | ")
        },
    });

    match Policy::from_settings(&loaded.settings, cwd.clone().unwrap_or_default()) {
        Ok(p) => checks.push(Check {
            name: "sandbox",
            ok: true,
            detail: format!(
                "preset {}, modo {}, imposição {}{}",
                p.preset.name,
                p.mode().settings_name(),
                p.enforcement.as_str(),
                if p.enforcement == zerith_security::Enforcement::Full {
                    ""
                } else {
                    " (política checada pelo Zerith; não há jaula do SO)"
                }
            ),
        }),
        Err(e) => checks.push(Check {
            name: "sandbox",
            ok: false,
            detail: e,
        }),
    }

    checks.push(keyring_check());

    if offline {
        checks.push(Check {
            name: "provider",
            ok: true,
            detail: "não verificado (--offline)".into(),
        });
    } else {
        checks.push(match provider_config(&loaded.settings) {
            Err(e) => Check {
                name: "provider",
                ok: false,
                detail: e.to_string(),
            },
            Ok(cfg) => {
                let shown = cfg.redacted();
                match Client::new(cfg) {
                    Err(e) => Check {
                        name: "provider",
                        ok: false,
                        detail: format!("{shown}: {e}"),
                    },
                    Ok(c) => {
                        let p = c.probe();
                        Check {
                            name: "provider",
                            ok: p.ok,
                            detail: format!(
                                "{shown}: http {} em {} ms, {} modelos {}",
                                p.status,
                                p.latency_ms,
                                p.models.len(),
                                p.detail
                            ),
                        }
                    }
                }
            }
        });
    }

    let mcp = load_servers(paths, cwd.as_deref());
    checks.push(Check {
        name: "mcp",
        ok: mcp.errors.is_empty() && mcp.unapproved.is_empty(),
        detail: format!(
            "{} servidor(es), {} aguardando aprovação, {} erro(s) de configuração",
            mcp.servers.len(),
            mcp.unapproved.len(),
            mcp.errors.len()
        ),
    });
    if let Ok(lib) = Plugins::open(&paths.root().join("plugins")) {
        let all = lib.plugins().unwrap_or_default();
        checks.push(Check {
            name: "plugins",
            ok: true,
            detail: format!(
                "{} instalado(s), {} ativo(s) e revisado(s)",
                all.len(),
                all.iter()
                    .filter(|p| p.trusted && p.status == PluginStatus::Enabled)
                    .count()
            ),
        });
    }

    let mut failed = 0;
    for c in &checks {
        println!(
            "{:<22} {:<5} {}",
            c.name,
            if c.ok { "ok" } else { "FALHA" },
            c.detail
        );
        failed += usize::from(!c.ok);
    }
    if failed > 0 {
        return Err(format!("{failed} verificação(ões) falharam").into());
    }
    Ok(())
}
