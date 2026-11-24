//! Session tooling, checkpoints, MCP, plugins, memory admin and `doctor`.
//! The pure argument parsing lives in `args`; this file does the I/O.

use std::collections::HashSet;
use std::io::{BufReader, Write};
use std::path::Path;

use zerith_core::settings::WireProtocol;
use zerith_core::{
    secrets, AccountEntry, CavemanLevel, ComboEntry, Paths, PonytailLevel, RoutingSettings,
    Settings, TokenSaverSettings,
};
use zerith_mcp::config::{
    add_server, approve_project_server, load_servers, parse_add, remove_server, user_file,
};
use zerith_mcp::{Client as McpClient, McpRegistry};
use zerith_memory::{Scope, SqliteMemory};
use zerith_plugins::{Library as Plugins, Status as PluginStatus};
use zerith_providers::Client;
use zerith_security::Policy;
use zerith_sessions::checkpoints::{self, Mode as RewindMode};
use zerith_sessions::migrations::now_ms;
use zerith_sessions::{AccountWindowStats, Store, UsageSummary};

use crate::args::{
    parse_doctor, parse_export, parse_memory_admin, parse_rewind, parse_search, ExportFormat,
    MemoryCmd,
};
use crate::headless::{effective_accounts, load_settings, provider_config};

pub type Res = Result<(), Box<dyn std::error::Error>>;

mod accounts;
mod routing;
mod usage;

pub use accounts::*;
pub use routing::*;
pub use usage::*;

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
        for n in &p.notes {
            println!("aviso: {n}");
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
    for n in &report.notes {
        println!("{n}");
    }
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

const LIMITS: &str = "limite: arquivos ignorados (.gitignore, node_modules/, target/...) \
só voltam se uma ferramenta de arquivo os editou.";

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
    let extra_agents = Plugins::open(&paths.root().join("plugins"))
        .and_then(|lib| lib.contributions())
        .map(|c| c.agents)
        .unwrap_or_default();
    let defs = zerith_core::agents::list_with_extra(paths, cwd.as_deref(), &extra_agents);
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
            for s in &loaded.disabled {
                println!(
                    "{:<16} {:<8} desligado",
                    s.name,
                    if s.is_http() { "http" } else { "stdio" }
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
                zerith_mcp::oauth::forget(paths.root(), &zerith_core::secrets::Configured, name)?;
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
            let proj = project_dir().ok_or("diretório do projeto indisponível")?;
            approve_project_server(paths, &proj, s)?;
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
        "login" | "logout" | "status" if args.get(2).is_some() => {
            let name = &args[2];
            let loaded = load_servers(paths, project_dir().as_deref());
            let s = loaded
                .servers
                .iter()
                .chain(&loaded.disabled)
                .chain(&loaded.unapproved)
                .find(|s| &s.name == name && s.is_oauth())
                .ok_or_else(|| format!("{name} não é um servidor MCP com auth = \"oauth\""))?;
            let vault = zerith_core::secrets::Configured;
            match sub {
                "login" => {
                    let url = s.url.clone().unwrap_or_default();
                    let no_browser = args.iter().any(|a| a == "--no-browser")
                        || std::env::var_os("ZERITH_OAUTH_NO_BROWSER").is_some();
                    zerith_mcp::oauth::login(paths.root(), &vault, name, &url, &|u| {
                        println!("abra para entrar em {name}:\n{u}");
                        if !no_browser {
                            let _ = zerith_providers::oauth::open_browser(u);
                        }
                    })?;
                    println!("{name}: conectado (tokens no cofre do sistema)");
                }
                "logout" => {
                    zerith_mcp::oauth::logout(&vault, name)?;
                    println!("{name}: desconectado");
                }
                _ => {
                    let st = zerith_mcp::oauth::status(&vault, name);
                    println!(
                        "{name}: {}",
                        if st.connected {
                            "conectado"
                        } else {
                            "precisa entrar"
                        }
                    );
                }
            }
        }
        "import-hermes" => {
            let home = zerith_importers::hermes_setup::hermes_home()
                .ok_or("Hermes não encontrado (HERMES_HOME, ~/.hermes, %LOCALAPPDATA%\\hermes)")?;
            let found = zerith_importers::hermes_setup::hermes_mcp(&home, paths)?;
            let names: Vec<String> = match args.get(2) {
                Some(_) => args[2..].to_vec(),
                None => found.iter().map(|(m, _)| m.name.clone()).collect(),
            };
            for (m, st) in &found {
                println!("{:<22} {}", m.name, st.as_str());
            }
            let r = zerith_importers::hermes_setup::import_hermes_mcp(&home, paths, &names)?;
            println!(
                "{} importados, {} já existiam, {} com erro",
                r.imported.len(),
                r.existed.len(),
                r.failed.len()
            );
            for f in &r.failed {
                eprintln!("  {f}");
            }
            for (m, _) in found
                .iter()
                .filter(|(m, _)| m.oauth && r.imported.contains(&m.name))
            {
                println!("  {}: entre com `zerith mcp login {}`", m.name, m.name);
            }
        }
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
        Some("marketplace" | "market") => market_cmd(&lib, args)?,
        Some("browse" | "store" | "loja") => {
            let installed = lib.plugins()?;
            for o in zerith_plugins::market::offers(&lib)? {
                let have = installed.iter().any(|p| p.manifest.name == o.name);
                println!(
                    "{:<20} {:<10} {:<10} {}",
                    format!("{}@{}", o.name, o.market),
                    if have { "instalado" } else { "" },
                    o.category,
                    o.description
                );
            }
        }
        Some("install") => {
            let spec = args
                .get(2)
                .ok_or("plugin install precisa de nome@marketplace ou de uma pasta")?;
            let p = if Path::new(spec).is_dir() {
                let p = lib.install(Path::new(spec))?;
                println!(
                    "{} {} instalado (desativado, não revisado)",
                    p.manifest.name, p.manifest.version
                );
                p
            } else {
                println!("baixando {spec}...");
                let p = zerith_plugins::market::install(&lib, spec)?;
                println!(
                    "{} {} instalado e ativo",
                    p.manifest.name, p.manifest.version
                );
                p
            };
            let m = &p.manifest;
            println!(
                "skills {}  agentes {}  mcp {}  hooks {}",
                m.skills.len(),
                m.agents.len(),
                m.mcp.len(),
                m.hooks.len()
            );
            if p.status == PluginStatus::Enabled {
                println!("a IA já usa as skills e os agentes dele quando o assunto aparece");
            }
            if zerith_plugins::market::runs_code(&p) {
                println!(
                    "ele também roda código (MCP/hooks): fica desligado até `zerith plugin trust {} --yes`",
                    m.name
                );
            } else if p.status != PluginStatus::Enabled {
                println!("veja o que ele pede com `zerith plugin show {}`", m.name);
            }
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
            for line in enable_notice(&lib.get(&n)?) {
                println!("{line}");
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

/// `plugin marketplace list|add|update|remove`.
fn market_cmd(lib: &Plugins, args: &[String]) -> Res {
    use zerith_plugins::market;
    match args.get(2).map(String::as_str) {
        None | Some("list") => {
            for m in market::list(lib)? {
                println!("{:<20} {}", m.name, m.source);
            }
        }
        Some("add" | "update") => {
            let arg = args
                .get(3)
                .ok_or("marketplace add precisa de dono/repo, URL git ou pasta")?;
            // `update <nome>` refetches from where it was added.
            let src = market::list(lib)?
                .into_iter()
                .find(|m| &m.name == arg)
                .map_or(arg.clone(), |m| m.source);
            let m = market::add(lib, &src)?;
            let n = market::offers(lib)?
                .iter()
                .filter(|o| o.market == m.name)
                .count();
            println!("marketplace {} pronto ({n} plugins)", m.name);
            println!("instale com `zerith plugin install <plugin>@{}`", m.name);
        }
        Some("remove") => {
            let n = args.get(3).ok_or("marketplace remove precisa do nome")?;
            market::remove(lib, n)?;
            println!("marketplace {n} removido");
        }
        Some(o) => return Err(format!("comando desconhecido: plugin marketplace {o}").into()),
    }
    Ok(())
}

/// What `plugin enable` tells the person. Skills and agents load as soon as
/// the plugin is enabled; only code it runs (MCP, hooks, a command) waits
/// for `trust`. The old line said "nothing loads until trust" for every
/// untrusted plugin, which was false for a skills-only one.
fn enable_notice(p: &zerith_plugins::Plugin) -> Vec<String> {
    let m = &p.manifest;
    let mut out = Vec::new();
    if !m.skills.is_empty() || !m.agents.is_empty() {
        out.push("a IA já usa as skills e os agentes dele quando o assunto aparece".to_string());
    }
    if !p.trusted && zerith_plugins::market::runs_code(p) {
        out.push(format!(
            "ele também roda código (MCP/hooks): essa parte fica desligada até `zerith plugin trust {} --yes`",
            m.name
        ));
    }
    out
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
    let choice = secrets::active();
    let backend = format!("backend {}: {}", choice.backend.label(), choice.reason);
    let (ok, detail) = match secrets::get_secret("zerith-doctor-probe") {
        Ok(_) | Err(secrets::SecretError::NotFound(_)) => (true, format!("disponível; {backend}")),
        Err(secrets::SecretError::Unavailable(why)) => (
            false,
            format!("indisponível ({why}); {backend}; use ZERITH_API_KEY"),
        ),
    };
    Check {
        name: "cofre de credenciais",
        ok,
        detail,
    }
}

pub fn doctor(paths: &Paths, args: &[String]) -> Res {
    let offline = parse_doctor(args)?;
    let mut checks: Vec<Check> = Vec::new();
    // First, the part that decides whether anything else can work: can every
    // data folder be created and written? Reported per folder, not as one
    // "Acesso negado" from whichever step happened to fail first.
    let folders = paths.check_writable();
    for f in &folders {
        checks.push(Check {
            name: "pasta de dados",
            ok: f.ok(),
            detail: match &f.problem {
                None => f.dir.display().to_string(),
                Some(p) => format!("{}: {p}", f.dir.display()),
            },
        });
    }
    if let Some(bad) = folders.iter().find(|f| !f.ok()) {
        for c in &checks {
            println!(
                "{:<24} {}  {}",
                c.name,
                if c.ok { "ok  " } else { "FALHA" },
                c.detail
            );
        }
        println!();
        println!("Para corrigir, revise e rode você mesmo (o Zerith não altera permissões):");
        for step in zerith_core::diagnose::repair_steps(&paths.root) {
            println!("  {step}");
        }
        return Err(format!(
            "a pasta {} não é gravável; nada mais foi verificado",
            bad.dir.display()
        )
        .into());
    }
    paths.ensure()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use zerith_sessions::{NewUsageRecord, SessionSource};

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
            std::fs::create_dir_all(&root).unwrap();
            let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = root.join(format!("cli-cmds-{}-{seq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn paths(&self) -> Paths {
            Paths::with_root(&self.0)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A skills-only plugin is in use as soon as it is enabled: the notice
    /// must not say "nothing loads until trust" (the old text did).
    #[test]
    fn enabling_a_skills_only_plugin_says_its_skills_are_in_use() {
        let t = TestDir::new();
        let src = t.0.join("src").join("zerith-demo");
        std::fs::create_dir_all(src.join(".claude-plugin")).unwrap();
        std::fs::write(
            src.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"zerith-demo","version":"1","description":"d"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(src.join("skills").join("demo")).unwrap();
        std::fs::write(
            src.join("skills").join("demo").join("SKILL.md"),
            "---\nname: demo\ndescription: \"d\"\n---\n\ncorpo\n",
        )
        .unwrap();
        let lib = zerith_plugins::Library::open(&t.0.join("plugins")).unwrap();
        let p = lib.install(&src).unwrap();
        let text = enable_notice(&p).join("\n");
        assert!(text.contains("já usa as skills"), "{text}");
        assert!(!text.contains("trust"), "{text}");
        assert!(!text.contains("nada dele carrega"), "{text}");
    }

    #[test]
    fn usage_hours_parser_handles_defaults_zero_and_errors() {
        assert_eq!(parse_usage_hours(&v(&["usage"])).unwrap(), 24);
        assert_eq!(
            parse_usage_hours(&v(&["quota", "--hours", "0"])).unwrap(),
            0
        );
        assert_eq!(parse_usage_hours(&v(&["usage", "--hours=72"])).unwrap(), 72);
        assert!(parse_usage_hours(&v(&["usage", "--hours"])).is_err());
        assert!(parse_usage_hours(&v(&["usage", "--hours", "abc"])).is_err());
        assert!(parse_usage_hours(&v(&["usage", "--desconhecido"])).is_err());
    }

    #[test]
    fn format_usage_report_crosses_accounts_quotas_models_and_token_saver() {
        let store = Store::open_in_memory().unwrap();
        let sess = store.create_session("uso", SessionSource::Native).unwrap();

        store
            .record_usage(&NewUsageRecord {
                session_id: Some(sess.id.clone()),
                account_id: "acct-main".into(),
                provider: "openai".into(),
                model: "gpt-4o".into(),
                role_kind: "main".into(),
                prompt_tokens: 1000,
                completion_tokens: 250,
                saved_rtk_tokens: 200,
                saved_ponytail_tokens: 120,
                saved_caveman_tokens: 80,
                saved_tokens: 400,
                status: "ok".into(),
                latency_ms: 100,
                ..Default::default()
            })
            .unwrap();
        store
            .record_usage(&NewUsageRecord {
                session_id: Some(sess.id),
                account_id: "acct-exhausted".into(),
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                role_kind: "subagent".into(),
                prompt_tokens: 500,
                completion_tokens: 100,
                saved_rtk_tokens: 50,
                saved_ponytail_tokens: 30,
                saved_caveman_tokens: 20,
                saved_tokens: 100,
                status: "ok".into(),
                latency_ms: 80,
                ..Default::default()
            })
            .unwrap();

        let base_now = now_ms();
        store
            .set_account_cooldown("acct-cd", base_now + 45_000, Some("429"))
            .unwrap();

        let settings = Settings {
            accounts: vec![
                AccountEntry {
                    id: "acct-main".into(),
                    provider: "openai".into(),
                    priority: 1,
                    enabled: true,
                    quota_tokens: Some(10_000),
                    quota_requests: Some(100),
                    ..Default::default()
                },
                AccountEntry {
                    id: "acct-cd".into(),
                    provider: "anthropic".into(),
                    priority: 2,
                    enabled: true,
                    ..Default::default()
                },
                AccountEntry {
                    id: "acct-exhausted".into(),
                    provider: "deepseek".into(),
                    priority: 3,
                    enabled: true,
                    quota_tokens: Some(600),
                    quota_requests: Some(10),
                    ..Default::default()
                },
                AccountEntry {
                    id: "acct-zero".into(),
                    provider: "openrouter".into(),
                    priority: 4,
                    enabled: true,
                    quota_tokens: Some(5000),
                    ..Default::default()
                },
                AccountEntry {
                    id: "acct-off".into(),
                    provider: "ollama".into(),
                    priority: 5,
                    enabled: false,
                    ..Default::default()
                },
            ],
            token_saver: TokenSaverSettings {
                ponytail: PonytailLevel::Full,
                caveman: CavemanLevel::Lite,
                rtk_enabled: true,
            },
            ..Default::default()
        };

        let summary = store.usage_summary(24).unwrap();
        let report = format_usage_report(&settings, &summary, 24, base_now);

        assert!(report.contains("conta ativa:"), "{report}");
        assert!(report.contains("Contas & Cotas:"), "{report}");
        assert!(report.contains("acct-main"), "{report}");
        assert!(report.contains("1250/10000"), "{report}");
        assert!(report.contains("1/100"), "{report}");
        assert!(report.contains("87.5%"), "{report}");
        assert!(report.contains("acct-cd"), "{report}");
        assert!(report.contains("cooldown 45 s"), "{report}");
        assert!(report.contains("acct-exhausted"), "{report}");
        assert!(report.contains("cota esgotada"), "{report}");
        assert!(report.contains("0.0%"), "{report}");
        assert!(report.contains("acct-zero"), "{report}");
        assert!(report.contains("0/5000"), "{report}");
        assert!(report.contains("100.0%"), "{report}");
        assert!(report.contains("acct-off"), "{report}");
        assert!(report.contains("desativada"), "{report}");

        assert!(report.contains("Modelos & Delegação:"), "{report}");
        assert!(report.contains("gpt-4o"), "{report}");
        assert!(report.contains("main"), "{report}");
        assert!(report.contains("deepseek-chat"), "{report}");
        assert!(report.contains("subagent"), "{report}");

        assert!(
            report.contains("Token Saver — Economia por Protocolo:"),
            "{report}"
        );
        assert!(report.contains("[ativo]:"), "{report}");
        assert!(
            report.contains("250 tokens  (50.0% da economia)"),
            "{report}"
        );
        assert!(report.contains("[full]:"), "{report}");
        assert!(
            report.contains("150 tokens  (30.0% da economia)"),
            "{report}"
        );
        assert!(report.contains("[lite]:"), "{report}");
        assert!(
            report.contains("100 tokens  (20.0% da economia)"),
            "{report}"
        );
        // total_saved = 500; gross = 1500 + 350 + 500 = 2350 -> 21.3%
        assert!(
            report.contains("Total economizado: 500 tokens (-21.3% sobre o consumo bruto)"),
            "{report}"
        );
    }

    #[test]
    fn format_usage_report_handles_empty_store() {
        let store = Store::open_in_memory().unwrap();
        let summary = store.usage_summary(0).unwrap();
        let mut settings = Settings::default();
        settings.token_saver.rtk_enabled = false;
        let report = format_usage_report(&settings, &summary, 0, now_ms());

        assert!(report.contains("(nenhum registro ainda)"), "{report}");
        assert!(report.contains("todo histórico"), "{report}");
        assert!(
            report.contains("(nenhuma conta configurada ou registrada)"),
            "{report}"
        );
        assert!(report.contains("[desligado]:"), "{report}");
        assert!(report.contains("Total economizado: 0 tokens"), "{report}");
    }

    #[test]
    fn accounts_cmd_adds_replaces_toggles_resets_cooldown_and_removes() {
        let dir = TestDir::new();
        let paths = dir.paths();

        accounts_cmd(
            &paths,
            &v(&[
                "accounts",
                "add",
                "work",
                "openai",
                "--label",
                "Trabalho",
                "--model",
                "gpt-4o",
                "--url",
                "https://api.openai.com/v1",
                "--priority",
                "-5",
                "--quota-tokens",
                "50000",
                "--quota-requests",
                "200",
                "--window-hours",
                "12",
                "--key-ref",
                "openai_work",
                "--protocol",
                "openai",
            ]),
        )
        .unwrap();

        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert_eq!(loaded.accounts.len(), 1);
        let a = &loaded.accounts[0];
        assert_eq!(a.id, "work");
        assert_eq!(a.provider, "openai");
        assert_eq!(a.label.as_deref(), Some("Trabalho"));
        assert_eq!(a.default_model.as_deref(), Some("gpt-4o"));
        assert_eq!(a.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(a.priority, -5);
        assert_eq!(a.quota_tokens, Some(50_000));
        assert_eq!(a.quota_requests, Some(200));
        assert_eq!(a.window_hours, 12);
        assert_eq!(a.api_key_ref.as_deref(), Some("openai_work"));
        assert_eq!(a.protocol, Some(WireProtocol::OpenAi));
        assert!(a.enabled);

        let listed = format_accounts_list(&loaded);
        assert!(listed.contains("work"), "{listed}");
        assert!(listed.contains("ativa"), "{listed}");
        assert!(listed.contains("prio=-5"), "{listed}");
        assert!(listed.contains("modelo=gpt-4o"), "{listed}");
        assert!(listed.contains("cota_tok=50000"), "{listed}");
        assert!(listed.contains("cota_req=200"), "{listed}");
        assert!(listed.contains("janela=12h"), "{listed}");

        // Re-adding same id replaces instead of duplicating.
        accounts_cmd(
            &paths,
            &v(&[
                "accounts",
                "add",
                "work",
                "anthropic",
                "--protocol",
                "anthropic",
                "--priority",
                "1",
            ]),
        )
        .unwrap();
        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].provider, "anthropic");
        assert_eq!(loaded.accounts[0].protocol, Some(WireProtocol::Anthropic));

        accounts_cmd(&paths, &v(&["accounts", "disable", "work"])).unwrap();
        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert!(!loaded.accounts[0].enabled);

        accounts_cmd(&paths, &v(&["accounts", "enable", "work"])).unwrap();
        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert!(loaded.accounts[0].enabled);

        paths.ensure().unwrap();
        let store = Store::open(paths.db()).unwrap();
        store
            .set_account_cooldown("work", now_ms() + 60_000, Some("429"))
            .unwrap();
        drop(store);
        accounts_cmd(&paths, &v(&["accounts", "reset-cooldown", "work"])).unwrap();
        let store = Store::open(paths.db()).unwrap();
        assert_eq!(
            store
                .account_window_stats("work", 24)
                .unwrap()
                .cooldown_until,
            0
        );
        drop(store);

        accounts_cmd(&paths, &v(&["accounts", "remove", "work"])).unwrap();
        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert!(loaded.accounts.is_empty());

        // Validation errors.
        assert!(accounts_cmd(&paths, &v(&["accounts", "add", "", "openai"])).is_err());
        assert!(accounts_cmd(&paths, &v(&["accounts", "add", "id1"])).is_err());
        assert!(accounts_cmd(
            &paths,
            &v(&["accounts", "add", "id1", "openai", "--protocol", "grpc"])
        )
        .is_err());
        assert!(accounts_cmd(&paths, &v(&["accounts", "remove", "missing"])).is_err());
        assert!(accounts_cmd(&paths, &v(&["accounts", "enable", "missing"])).is_err());
        assert!(accounts_cmd(&paths, &v(&["accounts", "bogus"])).is_err());
    }

    #[test]
    fn routing_cmd_persists_models_roles_and_combos() {
        let dir = TestDir::new();
        let paths = dir.paths();

        routing_cmd(&paths, &v(&["routing", "main", "gpt-4o"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "subagent", "gpt-4o-mini"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "explore", "haiku"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "plan", "opus"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "role", "reviewer", "sonnet-4"])).unwrap();
        routing_cmd(
            &paths,
            &v(&[
                "routing",
                "combo",
                "fast",
                "openai/gpt-4o-mini,deepseek/deepseek-chat",
            ]),
        )
        .unwrap();
        routing_cmd(
            &paths,
            &v(&[
                "routing",
                "combo",
                "fast",
                "anthropic/claude-haiku-4,gpt-4o-mini",
            ]),
        )
        .unwrap();

        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert_eq!(loaded.routing.main_model.as_deref(), Some("gpt-4o"));
        assert_eq!(
            loaded.routing.subagent_model.as_deref(),
            Some("gpt-4o-mini")
        );
        assert_eq!(loaded.routing.explore_model.as_deref(), Some("haiku"));
        assert_eq!(loaded.routing.plan_model.as_deref(), Some("opus"));
        assert_eq!(
            loaded
                .routing
                .role_models
                .get("reviewer")
                .map(String::as_str),
            Some("sonnet-4")
        );
        assert_eq!(loaded.routing.combos.len(), 1);
        assert_eq!(
            loaded.routing.combos[0].models,
            vec!["anthropic/claude-haiku-4", "gpt-4o-mini"]
        );

        let shown = format_routing(&loaded.routing);
        assert!(shown.contains("main_model      gpt-4o"), "{shown}");
        assert!(shown.contains("subagent_model  gpt-4o-mini"), "{shown}");
        assert!(shown.contains("explore_model   haiku"), "{shown}");
        assert!(shown.contains("plan_model      opus"), "{shown}");
        assert!(shown.contains("reviewer"), "{shown}");
        assert!(shown.contains("sonnet-4"), "{shown}");
        assert!(shown.contains("fast"), "{shown}");
        assert!(
            shown.contains("anthropic/claude-haiku-4, gpt-4o-mini"),
            "{shown}"
        );

        routing_cmd(&paths, &v(&["routing", "main", "clear"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "explore", "none"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "role", "reviewer", "clear"])).unwrap();
        routing_cmd(&paths, &v(&["routing", "combo", "remove", "fast"])).unwrap();

        let cleared = Settings::load_with_env(&paths, None, |_| None).settings;
        assert_eq!(cleared.routing.main_model, None);
        assert_eq!(cleared.routing.explore_model, None);
        assert!(cleared.routing.role_models.is_empty());
        assert!(cleared.routing.combos.is_empty());

        assert!(routing_cmd(&paths, &v(&["routing", "combo", "empty", " , "])).is_err());
        assert!(routing_cmd(&paths, &v(&["routing", "combo", "remove", "missing"])).is_err());
        assert!(routing_cmd(&paths, &v(&["routing", "bogus"])).is_err());
    }

    #[test]
    fn tokensaver_cmd_validates_saves_and_formats() {
        let dir = TestDir::new();
        let paths = dir.paths();

        // `show` works before any DB exists and does not create one unnecessarily.
        tokensaver_cmd(&paths, &v(&["tokensaver", "show"])).unwrap();
        assert!(!paths.db().exists());

        tokensaver_cmd(&paths, &v(&["tokensaver", "ponytail", "ultra"])).unwrap();
        tokensaver_cmd(&paths, &v(&["tokensaver", "caveman", "wenyan"])).unwrap();
        tokensaver_cmd(&paths, &v(&["tokensaver", "rtk", "off"])).unwrap();

        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert_eq!(loaded.token_saver.ponytail, PonytailLevel::Ultra);
        assert_eq!(loaded.token_saver.caveman, CavemanLevel::Wenyan);
        assert!(!loaded.token_saver.rtk_enabled);

        tokensaver_cmd(&paths, &v(&["tokensaver", "rtk", "true"])).unwrap();
        let loaded = Settings::load_with_env(&paths, None, |_| None).settings;
        assert!(loaded.token_saver.rtk_enabled);

        let store = Store::open_in_memory().unwrap();
        store
            .record_usage(&NewUsageRecord {
                account_id: "a1".into(),
                provider: "openai".into(),
                model: "gpt-4o".into(),
                prompt_tokens: 400,
                completion_tokens: 100,
                saved_rtk_tokens: 150,
                saved_ponytail_tokens: 100,
                saved_caveman_tokens: 50,
                saved_tokens: 300,
                ..Default::default()
            })
            .unwrap();
        let summary = store.usage_summary(24).unwrap();
        let shown = format_tokensaver_show(&loaded.token_saver, Some(&summary));
        assert!(shown.contains("ponytail  ultra"), "{shown}");
        assert!(shown.contains("caveman   wenyan"), "{shown}");
        assert!(shown.contains("rtk       on"), "{shown}");
        assert!(shown.contains("Total economizado: 300 tokens"), "{shown}");

        assert!(tokensaver_cmd(&paths, &v(&["tokensaver", "ponytail", "mega"])).is_err());
        assert!(tokensaver_cmd(&paths, &v(&["tokensaver", "caveman", "turbo"])).is_err());
        assert!(tokensaver_cmd(&paths, &v(&["tokensaver", "rtk", "maybe"])).is_err());
        assert!(tokensaver_cmd(&paths, &v(&["tokensaver", "bogus"])).is_err());
    }

    #[test]
    fn usage_cmd_runs_against_disk_store() {
        let dir = TestDir::new();
        let paths = dir.paths();
        paths.ensure().unwrap();
        let store = Store::open(paths.db()).unwrap();
        store
            .record_usage(&NewUsageRecord {
                account_id: "local-acc".into(),
                provider: "ollama".into(),
                model: "qwen3:8b".into(),
                prompt_tokens: 200,
                completion_tokens: 50,
                saved_rtk_tokens: 40,
                ..Default::default()
            })
            .unwrap();
        drop(store);

        usage_cmd(&paths, &v(&["usage", "--hours", "24"])).unwrap();
        usage_cmd(&paths, &v(&["quota", "--hours", "0"])).unwrap();
    }
}

// ---------------------------------------------------------------------------
// login (OAuth)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct LoginArgs {
    tool: zerith_providers::OAuthTool,
    account: String,
    import: bool,
    from_9router: bool,
    no_browser: bool,
}

fn login_args(args: &[String]) -> Result<LoginArgs, String> {
    let tool = zerith_providers::OAuthTool::parse(args.get(1).map(String::as_str).unwrap_or(""))
        .ok_or_else(|| {
            "uso: zerith login <claude-code|codex|antigravity> [--account <id>] [--import | --from-9router] [--no-browser]"
                .to_string()
        })?;
    let mut account = tool.as_str().to_string();
    let mut import = false;
    let mut no_browser = false;
    let mut from_9router = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--account" => {
                account = args
                    .get(i + 1)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("--account precisa de um valor")?
                    .clone();
                i += 2;
                continue;
            }
            "--import" => import = true,
            "--no-browser" => no_browser = true,
            "--from-9router" => from_9router = true,
            other => return Err(format!("flag desconhecida: {other}")),
        }
        i += 1;
    }
    Ok(LoginArgs {
        tool,
        account,
        import,
        no_browser,
        from_9router,
    })
}

/// The provider an OAuth tool speaks. Codex and Antigravity have their own
/// preset (own base URL and protocol); Claude Code reuses the Anthropic one.
fn oauth_provider(tool: zerith_providers::OAuthTool) -> &'static str {
    match tool {
        zerith_providers::OAuthTool::ClaudeCode => "anthropic",
        zerith_providers::OAuthTool::Codex => "codex",
        zerith_providers::OAuthTool::Antigravity => "antigravity",
    }
}

pub fn login_cmd(paths: &Paths, args: &[String], home: &Path) -> Res {
    login_cmd_with(paths, args, home, &zerith_core::secrets::Configured)
}

/// Same as `login_cmd` with the cofre injected (tests use a map).
pub fn login_cmd_with(
    paths: &Paths,
    args: &[String],
    home: &Path,
    store: &dyn zerith_core::secrets::SecretStore,
) -> Res {
    use zerith_providers::oauth;

    let a = login_args(args)?;

    let cred = if a.from_9router {
        let db = std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .ok_or("não sei a pasta do usuário")?
            .join("9router/db/data.sqlite");
        oauth::import_from_9router(a.tool, &db).ok_or_else(|| {
            format!(
                "o 9router não tem conexão ativa de {} em {}",
                a.tool.as_str(),
                db.display()
            )
        })?
    } else if a.import {
        // ponytail: an imported credential is stored as-is; the runtime renews
        // it on the next turn, where a failed refresh falls back to failover.
        oauth::import_local(a.tool, home).ok_or_else(|| {
            format!(
                "nenhuma credencial local de {} encontrada em {}",
                a.tool.as_str(),
                home.display()
            )
        })?
    } else {
        let pkce = oauth::Pkce::generate();
        let state = oauth::random_state();
        if let Err(why) = oauth::ready(a.tool) {
            return Err(why.into());
        }
        // bind before the browser: a busy fixed port fails here, not after consent
        let (listener, port) = oauth::bind(a.tool)?;
        let url = oauth::authorize_url(a.tool, &pkce, &state, port);
        println!("Abra no navegador:\n  {url}\n");
        if !a.no_browser {
            let _ = oauth::open_browser(&url);
        }
        let code = if oauth::needs_pasted_code(a.tool) {
            // Claude shows the code on its own page: no callback will arrive.
            drop(listener);
            print!("Depois de autorizar, cole aqui o código que o Claude mostrou: ");
            std::io::Write::flush(&mut std::io::stdout())?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let (code, pasted_state) = oauth::pasted_code(&line);
            if pasted_state.is_some_and(|s| s != state) {
                return Err("o código é de outra tentativa (state não confere)".into());
            }
            code.to_string()
        } else {
            println!("Aguardando o retorno em http://127.0.0.1:{port} …");
            oauth::wait_for_code(listener, oauth::redirect_path(a.tool), &state)?
        };
        oauth::exchange(
            a.tool,
            &code,
            &pkce.verifier,
            &oauth::redirect_uri(a.tool, port),
            &state,
        )?
    };

    zerith_core::secrets::save_oauth(store, &a.account, &cred)?;

    let id = a.account.clone();
    let provider = oauth_provider(a.tool).to_string();
    let tool_name = a.tool.as_str().to_string();
    Settings::save_user(paths, |s| {
        let entry = AccountEntry {
            id: id.clone(),
            provider: provider.clone(),
            auth: zerith_core::settings::AuthMode::Oauth,
            oauth_tool: Some(tool_name.clone()),
            ..Default::default()
        };
        match s.accounts.iter_mut().find(|x| x.id == id) {
            Some(slot) => *slot = entry,
            None => s.accounts.push(entry),
        }
    })?;

    let minutes = (cred.expires_at - oauth::now_secs()).max(0) / 60;
    println!(
        "conta {} conectada via {} (expira em {} min)",
        a.account,
        a.tool.as_str(),
        minutes
    );
    Ok(())
}

#[cfg(test)]
mod login_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Mem(RefCell<HashMap<String, String>>);

    impl zerith_core::secrets::SecretStore for Mem {
        fn get(&self, n: &str) -> Result<String, zerith_core::secrets::SecretError> {
            self.0
                .borrow()
                .get(n)
                .cloned()
                .ok_or_else(|| zerith_core::secrets::SecretError::NotFound(n.into()))
        }
        fn set(&self, n: &str, v: &str) -> Result<(), zerith_core::secrets::SecretError> {
            self.0.borrow_mut().insert(n.into(), v.into());
            Ok(())
        }
        fn delete(&self, n: &str) -> Result<(), zerith_core::secrets::SecretError> {
            self.0
                .borrow_mut()
                .remove(n)
                .map(|_| ())
                .ok_or_else(|| zerith_core::secrets::SecretError::NotFound(n.into()))
        }
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn login_flags_parse_tool_account_import_and_no_browser() {
        let a = login_args(&v(&[
            "login",
            "codex",
            "--account",
            "codex-work",
            "--import",
            "--no-browser",
        ]))
        .unwrap();
        assert_eq!(a.tool, zerith_providers::OAuthTool::Codex);
        assert_eq!(a.account, "codex-work");
        assert!(a.import);
        assert!(a.no_browser);

        let d = login_args(&v(&["login", "claude-code"])).unwrap();
        assert_eq!(d.account, "claude-code");
        assert!(!d.import && !d.no_browser);
    }

    #[test]
    fn login_rejects_an_unknown_tool_and_a_dangling_account_flag() {
        let e = login_args(&v(&["login", "kiro"])).unwrap_err();
        assert!(e.contains("claude-code"), "{e}");
        assert!(login_args(&v(&["login", "codex", "--account"])).is_err());
        assert!(login_args(&v(&["login", "codex", "--wat"])).is_err());
    }

    #[test]
    fn import_writes_the_credential_and_an_oauth_account() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/login-import");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/auth.json"),
            r#"{"tokens":{"access_token":"AT","refresh_token":"RT","account_id":"acc"}}"#,
        )
        .unwrap();

        let paths = Paths::with_root(root.join("z"));
        let store = Mem(RefCell::default());
        login_cmd_with(
            &paths,
            &v(&["login", "codex", "--account", "codex-work", "--import"]),
            &home,
            &store,
        )
        .unwrap();

        let cred = zerith_core::secrets::load_oauth(&store, "codex-work")
            .unwrap()
            .expect("credencial gravada");
        assert_eq!(cred.access_token, "AT");
        assert_eq!(cred.tool, zerith_providers::OAuthTool::Codex);

        let settings = Settings::load(&paths, None).settings;
        let acc = settings
            .accounts
            .iter()
            .find(|a| a.id == "codex-work")
            .expect("conta criada");
        assert_eq!(acc.auth, zerith_core::settings::AuthMode::Oauth);
        assert_eq!(acc.oauth_tool.as_deref(), Some("codex"));
        assert_eq!(acc.provider, "codex");
        // the token never lands in settings.toml
        let text = std::fs::read_to_string(paths.settings()).unwrap();
        assert!(!text.contains("AT"));
    }

    #[test]
    fn import_without_a_local_credential_says_so_and_writes_nothing() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/login-nothing");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();

        let paths = Paths::with_root(root.join("z"));
        let store = Mem(RefCell::default());
        let e = login_cmd_with(&paths, &v(&["login", "codex", "--import"]), &home, &store)
            .unwrap_err()
            .to_string();
        assert!(e.contains("nenhuma credencial local"), "{e}");
        assert!(zerith_core::secrets::load_oauth(&store, "codex")
            .unwrap()
            .is_none());
    }
}
