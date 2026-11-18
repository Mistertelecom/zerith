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
    add_server, approve_server, load_servers, parse_add, remove_server, user_file,
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

// ------------------------------------------------------------ usage & quota

fn parse_usage_hours(args: &[String]) -> Result<u32, String> {
    let mut hours = 24u32;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--hours" {
            let raw = args
                .get(i + 1)
                .ok_or("--hours precisa de um número de horas (ex: 24 ou 0)")?;
            hours = raw
                .trim()
                .parse()
                .map_err(|_| format!("--hours inválido: {raw} (use um inteiro >= 0)"))?;
            i += 2;
        } else if let Some(raw) = a.strip_prefix("--hours=") {
            hours = raw
                .trim()
                .parse()
                .map_err(|_| format!("--hours inválido: {raw} (use um inteiro >= 0)"))?;
            i += 1;
        } else {
            return Err(format!("argumento desconhecido: {a}"));
        }
    }
    Ok(hours)
}

fn pct_share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 * 100.0) / total as f64
    }
}

fn format_limit_col(used: u64, limit: Option<u64>) -> String {
    match limit {
        Some(q) => format!("{used}/{q}"),
        None => used.to_string(),
    }
}

fn account_status_and_remaining(
    enabled: bool,
    cooldown_until: i64,
    now: i64,
    used_tokens: u64,
    used_reqs: u64,
    quota_tokens: Option<u64>,
    quota_requests: Option<u64>,
) -> (String, String) {
    let tok_pct = quota_tokens.map(|q| {
        if q == 0 {
            0.0
        } else {
            (q.saturating_sub(used_tokens) as f64 * 100.0) / q as f64
        }
    });
    let req_pct = quota_requests.map(|q| {
        if q == 0 {
            0.0
        } else {
            (q.saturating_sub(used_reqs) as f64 * 100.0) / q as f64
        }
    });
    let remaining = match (tok_pct, req_pct) {
        (Some(t), Some(r)) => format!("{:.1}%", t.min(r)),
        (Some(t), None) => format!("{t:.1}%"),
        (None, Some(r)) => format!("{r:.1}%"),
        (None, None) => "-".to_string(),
    };

    let exhausted = quota_tokens.is_some_and(|q| used_tokens >= q)
        || quota_requests.is_some_and(|q| used_reqs >= q);
    let status = if !enabled {
        "desativada".to_string()
    } else if cooldown_until > now {
        let secs = (cooldown_until - now).saturating_add(999) / 1000;
        format!("cooldown {secs} s")
    } else if exhausted {
        "cota esgotada".to_string()
    } else {
        "ativa".to_string()
    };
    (status, remaining)
}

pub fn format_token_saver_savings(ts: &TokenSaverSettings, summary: &UsageSummary) -> String {
    let total = summary.total_saved_tokens;
    let rtk = summary.total_saved_rtk_tokens;
    let ponytail = summary.total_saved_ponytail_tokens;
    let caveman = summary.total_saved_caveman_tokens;
    let gross = summary
        .total_prompt_tokens
        .saturating_add(summary.total_completion_tokens)
        .saturating_add(total);
    let pct_bruto = if gross == 0 {
        0.0
    } else {
        (total as f64 * 100.0) / gross as f64
    };

    let rtk_tag = format!("[{}]:", if ts.rtk_enabled { "ativo" } else { "desligado" });
    let pt_tag = format!("[{}]:", ts.ponytail.as_str());
    let cv_tag = format!("[{}]:", ts.caveman.as_str());

    let mut out = String::new();
    out.push_str("Token Saver — Economia por Protocolo:\n");
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "RTK (compressão de ferramentas)",
        rtk_tag,
        rtk,
        pct_share(rtk, total),
    ));
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "Ponytail (lazy senior dev)",
        pt_tag,
        ponytail,
        pct_share(ponytail, total),
    ));
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "Caveman (respostas concisas)",
        cv_tag,
        caveman,
        pct_share(caveman, total),
    ));
    out.push_str(&format!(
        "  Total economizado: {total} tokens (-{pct_bruto:.1}% sobre o consumo bruto)"
    ));
    out
}

pub fn format_usage_report(
    settings: &Settings,
    summary: &UsageSummary,
    hours: u32,
    now: i64,
) -> String {
    let mut out = String::new();
    let window_label = if hours == 0 {
        "todo histórico".to_string()
    } else {
        format!("últimas {hours}h")
    };
    match (&summary.active_account, &summary.active_model) {
        (Some(acc), Some(model)) => {
            out.push_str(&format!(
                "conta ativa: {acc}   modelo em uso: {model}   (janela: {window_label})\n\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "conta ativa: (nenhum registro ainda)   (janela: {window_label})\n\n"
            ));
        }
    }

    // Cross-reference configured accounts (preserving those with 0 usage) and DB stats.
    struct Row {
        id: String,
        provider: String,
        priority: String,
        status: String,
        tokens: String,
        reqs: String,
        remaining: String,
    }

    let configured = effective_accounts(settings);
    let mut configured_ids: HashSet<&str> = HashSet::with_capacity(configured.len());
    let mut rows: Vec<Row> = Vec::new();

    for acc in configured {
        let id = acc.id.trim();
        configured_ids.insert(id);
        let stats: Option<&AccountWindowStats> =
            summary.accounts.iter().find(|s| s.account_id == id);
        let used_tokens = stats.map_or(0, |s| s.total_tokens);
        let used_reqs = stats.map_or(0, |s| s.requests);
        let cooldown_until = stats.map_or(0, |s| s.cooldown_until);
        let provider = if !acc.provider.trim().is_empty() {
            acc.provider.trim().to_string()
        } else {
            stats
                .map(|s| s.provider.trim())
                .filter(|p| !p.is_empty())
                .unwrap_or("-")
                .to_string()
        };
        let (status, remaining) = account_status_and_remaining(
            acc.enabled,
            cooldown_until,
            now,
            used_tokens,
            used_reqs,
            acc.quota_tokens,
            acc.quota_requests,
        );
        rows.push(Row {
            id: id.to_string(),
            provider,
            priority: acc.priority.to_string(),
            status,
            tokens: format_limit_col(used_tokens, acc.quota_tokens),
            reqs: format_limit_col(used_reqs, acc.quota_requests),
            remaining,
        });
    }

    for s in &summary.accounts {
        if configured_ids.contains(s.account_id.as_str()) {
            continue;
        }
        let provider = if s.provider.trim().is_empty() {
            "-".to_string()
        } else {
            s.provider.clone()
        };
        let (status, remaining) = account_status_and_remaining(
            true,
            s.cooldown_until,
            now,
            s.total_tokens,
            s.requests,
            None,
            None,
        );
        rows.push(Row {
            id: s.account_id.clone(),
            provider,
            priority: "-".to_string(),
            status,
            tokens: s.total_tokens.to_string(),
            reqs: s.requests.to_string(),
            remaining,
        });
    }

    out.push_str("Contas & Cotas:\n");
    if rows.is_empty() {
        out.push_str("  (nenhuma conta configurada ou registrada)\n\n");
    } else {
        out.push_str(&format!(
            "  {:<18} {:<12} {:>5}  {:<15} {:<19} {:<12} {:>8}\n",
            "id", "provider", "prio", "status", "tokens", "reqs", "restante"
        ));
        for r in &rows {
            out.push_str(&format!(
                "  {:<18} {:<12} {:>5}  {:<15} {:<19} {:<12} {:>8}\n",
                r.id, r.provider, r.priority, r.status, r.tokens, r.reqs, r.remaining
            ));
        }
        out.push('\n');
    }

    out.push_str("Modelos & Delegação:\n");
    if summary.models.is_empty() {
        out.push_str("  (nenhum modelo registrado nesta janela)\n\n");
    } else {
        out.push_str(&format!(
            "  {:<26} {:<12} {:<10} {:>6} {:>10} {:>10} {:>10}\n",
            "model", "provider", "role_kind", "reqs", "prompt", "completion", "total"
        ));
        for m in &summary.models {
            out.push_str(&format!(
                "  {:<26} {:<12} {:<10} {:>6} {:>10} {:>10} {:>10}\n",
                m.model,
                m.provider,
                m.role_kind,
                m.requests,
                m.prompt_tokens,
                m.completion_tokens,
                m.total_tokens
            ));
        }
        out.push('\n');
    }

    out.push_str(&format_token_saver_savings(&settings.token_saver, summary));
    out
}

pub fn usage_cmd(paths: &Paths, args: &[String]) -> Res {
    let hours = parse_usage_hours(args)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let settings = load_settings(paths);
    let summary = store.usage_summary(hours)?;
    println!(
        "{}",
        format_usage_report(&settings, &summary, hours, now_ms())
    );
    Ok(())
}

// ------------------------------------------------------------------ accounts

fn parse_wire_protocol(raw: &str) -> Result<WireProtocol, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(WireProtocol::OpenAi),
        "anthropic" => Ok(WireProtocol::Anthropic),
        other => Err(format!(
            "--protocol inválido: {other} (use openai ou anthropic)"
        )),
    }
}

fn flag_val(rest: &[String], i: usize, flag: &str) -> Result<String, String> {
    rest.get(i + 1)
        .cloned()
        .ok_or_else(|| format!("{flag} precisa de um valor"))
}

fn flag_non_empty(rest: &[String], i: usize, flag: &str) -> Result<String, String> {
    let v = flag_val(rest, i, flag)?;
    let trimmed = v.trim().to_string();
    if trimmed.is_empty() {
        return Err(format!("{flag} não pode ser vazio"));
    }
    Ok(trimmed)
}

fn parse_account_add(rest: &[String]) -> Result<AccountEntry, String> {
    let mut entry = AccountEntry::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            "--label" => {
                entry.label = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--model" => {
                entry.default_model = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--url" => {
                entry.base_url = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--priority" => {
                let raw = flag_val(rest, i, a)?;
                entry.priority = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--priority inválido: {raw}"))?;
                i += 2;
            }
            "--quota-tokens" => {
                let raw = flag_val(rest, i, a)?;
                entry.quota_tokens = Some(
                    raw.trim()
                        .parse()
                        .map_err(|_| format!("--quota-tokens inválido: {raw}"))?,
                );
                i += 2;
            }
            "--quota-requests" => {
                let raw = flag_val(rest, i, a)?;
                entry.quota_requests = Some(
                    raw.trim()
                        .parse()
                        .map_err(|_| format!("--quota-requests inválido: {raw}"))?,
                );
                i += 2;
            }
            "--window-hours" => {
                let raw = flag_val(rest, i, a)?;
                entry.window_hours = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--window-hours inválido: {raw}"))?;
                i += 2;
            }
            "--key-ref" => {
                entry.api_key_ref = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--protocol" => {
                let raw = flag_val(rest, i, a)?;
                entry.protocol = Some(parse_wire_protocol(&raw)?);
                i += 2;
            }
            s if s.starts_with("--") => return Err(format!("argumento desconhecido: {s}")),
            s => {
                positional.push(s.to_string());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err("accounts add precisa de <id> <provider>".into());
    }
    if positional.len() > 2 {
        return Err(format!("argumento inesperado: {}", positional[2]));
    }
    let id = positional[0].trim();
    let provider = positional[1].trim();
    if id.is_empty() {
        return Err("id da conta não pode ser vazio".into());
    }
    if provider.is_empty() {
        return Err("provedor da conta não pode ser vazio".into());
    }
    entry.id = id.to_string();
    entry.provider = provider.to_string();
    Ok(entry)
}

fn require_account_id<'a>(arg: Option<&'a String>, sub: &str) -> Result<&'a str, String> {
    arg.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("accounts {sub} precisa de um <id>"))
}

pub fn format_accounts_list(settings: &Settings) -> String {
    let accounts = effective_accounts(settings);
    if accounts.is_empty() {
        return "(nenhuma conta configurada; use `zerith accounts add <id> <provider>`)"
            .to_string();
    }
    let mut out = String::new();
    for (idx, acc) in accounts.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{:<16} {:<12} {:<10} prio={:<4} modelo={:<18} cota_tok={:<10} cota_req={:<8} janela={}h  url={}",
            acc.id,
            acc.provider,
            if acc.enabled { "ativa" } else { "desativada" },
            acc.priority,
            acc.default_model.as_deref().unwrap_or("-"),
            acc.quota_tokens
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            acc.quota_requests
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            acc.window_hours,
            acc.base_url.as_deref().unwrap_or("-"),
        ));
        if let Some(ref key_ref) = acc.api_key_ref {
            out.push_str(&format!("  key_ref={key_ref}"));
        }
        if let Some(ref label) = acc.label {
            out.push_str(&format!("  label={label:?}"));
        }
    }
    out
}

pub fn accounts_cmd(paths: &Paths, args: &[String]) -> Res {
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            let settings = load_settings(paths);
            println!("{}", format_accounts_list(&settings));
        }
        Some("add") => {
            let entry = parse_account_add(args.get(2..).unwrap_or(&[]))?;
            let id = entry.id.clone();
            let secret_name = entry
                .api_key_ref
                .as_deref()
                .unwrap_or(&entry.id)
                .to_string();
            let mut replaced = false;
            Settings::save_user(paths, |s| {
                if let Some(slot) = s.accounts.iter_mut().find(|a| a.id == id) {
                    *slot = entry.clone();
                    replaced = true;
                } else {
                    s.accounts.push(entry.clone());
                }
            })?;
            println!(
                "conta {id} {} em {}",
                if replaced { "atualizada" } else { "adicionada" },
                paths.settings().display()
            );
            println!(
                "aviso: salve a chave desta conta no cofre com `zerith secret set {secret_name}`"
            );
        }
        Some("remove") => {
            let id = require_account_id(args.get(2), "remove")?;
            let mut removed = false;
            Settings::save_user(paths, |s| {
                let before = s.accounts.len();
                s.accounts.retain(|a| a.id != id);
                removed = s.accounts.len() != before;
            })?;
            if !removed {
                return Err(format!(
                    "conta '{id}' não encontrada em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!("conta {id} removida");
        }
        Some(sub @ ("enable" | "disable")) => {
            let id = require_account_id(args.get(2), sub)?;
            let enable = sub == "enable";
            let mut found = false;
            Settings::save_user(paths, |s| {
                for a in &mut s.accounts {
                    if a.id == id {
                        a.enabled = enable;
                        found = true;
                    }
                }
            })?;
            if !found {
                return Err(format!(
                    "conta '{id}' não encontrada em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!(
                "conta {id} {}",
                if enable { "ativada" } else { "desativada" }
            );
        }
        Some("reset-cooldown") => {
            let id = require_account_id(args.get(2), "reset-cooldown")?;
            paths.ensure()?;
            Store::open(paths.db())?.clear_account_cooldown(id)?;
            println!("cooldown da conta {id} limpo");
        }
        Some(other) => return Err(format!("comando desconhecido: accounts {other}").into()),
    }
    Ok(())
}

// ------------------------------------------------------------------- routing

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoutingField {
    Main,
    Subagent,
    Explore,
    Plan,
}

impl RoutingField {
    fn label(self) -> &'static str {
        match self {
            Self::Main => "main_model",
            Self::Subagent => "subagent_model",
            Self::Explore => "explore_model",
            Self::Plan => "plan_model",
        }
    }

    fn slot(self, r: &mut RoutingSettings) -> &mut Option<String> {
        match self {
            Self::Main => &mut r.main_model,
            Self::Subagent => &mut r.subagent_model,
            Self::Explore => &mut r.explore_model,
            Self::Plan => &mut r.plan_model,
        }
    }
}

#[derive(Debug, PartialEq)]
enum RoutingAction {
    Show,
    SetField {
        field: RoutingField,
        value: Option<String>,
    },
    SetRole {
        agent: String,
        model: Option<String>,
    },
    SetCombo {
        name: String,
        models: Vec<String>,
    },
    RemoveCombo {
        name: String,
    },
}

fn parse_model_or_clear(raw: &str, what: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{what} não pode ser vazio (use `clear` para limpar)"
        ));
    }
    if trimmed.eq_ignore_ascii_case("clear") || trimmed.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn parse_routing(args: &[String]) -> Result<RoutingAction, String> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Ok(RoutingAction::Show);
    };
    match sub {
        "show" => {
            if let Some(extra) = args.get(2) {
                return Err(format!("argumento inesperado em routing show: {extra}"));
            }
            Ok(RoutingAction::Show)
        }
        "main" | "subagent" | "explore" | "plan" => {
            let field = match sub {
                "main" => RoutingField::Main,
                "subagent" => RoutingField::Subagent,
                "explore" => RoutingField::Explore,
                "plan" => RoutingField::Plan,
                _ => unreachable!(),
            };
            let raw = args
                .get(2)
                .ok_or_else(|| format!("routing {sub} precisa de <modelo> (ou `clear`)"))?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            Ok(RoutingAction::SetField {
                field,
                value: parse_model_or_clear(raw, &format!("routing {sub}"))?,
            })
        }
        "role" => {
            let agent = args
                .get(2)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("routing role precisa de <agente> <modelo|clear>")?;
            let raw = args
                .get(3)
                .ok_or("routing role precisa de <modelo> (ou `clear`)")?;
            if let Some(extra) = args.get(4) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            Ok(RoutingAction::SetRole {
                agent: agent.to_string(),
                model: parse_model_or_clear(raw, "modelo da role")?,
            })
        }
        "combo" => match args.get(2).map(String::as_str) {
            Some("remove") => {
                let name = args
                    .get(3)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or("routing combo remove precisa de <nome>")?;
                if let Some(extra) = args.get(4) {
                    return Err(format!("argumento inesperado: {extra}"));
                }
                Ok(RoutingAction::RemoveCombo {
                    name: name.to_string(),
                })
            }
            Some(raw_name) => {
                let name = raw_name.trim();
                if name.is_empty() {
                    return Err("nome do combo não pode ser vazio".into());
                }
                let tail = args.get(3..).unwrap_or(&[]);
                if tail.is_empty() {
                    return Err("routing combo precisa de <nome> <m1,m2,...>".into());
                }
                let joined = tail.join(",");
                let models: Vec<String> = joined
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if models.is_empty() {
                    return Err(
                        "routing combo precisa de pelo menos um modelo em <m1,m2,...>".into(),
                    );
                }
                Ok(RoutingAction::SetCombo {
                    name: name.to_string(),
                    models,
                })
            }
            None => Err("routing combo precisa de <nome> <m1,m2,...> ou `remove <nome>`".into()),
        },
        other => Err(format!("comando desconhecido: routing {other}")),
    }
}

pub fn format_routing(r: &RoutingSettings) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "main_model      {}\n",
        r.main_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "subagent_model  {}\n",
        r.subagent_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "explore_model   {}\n",
        r.explore_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "plan_model      {}\n",
        r.plan_model.as_deref().unwrap_or("(padrão)")
    ));
    if r.role_models.is_empty() {
        out.push_str("role_models     (nenhum)\n");
    } else {
        out.push_str("role_models:\n");
        for (agent, model) in &r.role_models {
            out.push_str(&format!("  {agent:<16} -> {model}\n"));
        }
    }
    let mut seen = HashSet::new();
    let mut combos = Vec::new();
    for c in r.combos.iter().rev() {
        let name = c.name.trim();
        if !name.is_empty() && seen.insert(name) {
            combos.push(c);
        }
    }
    combos.reverse();
    if combos.is_empty() {
        out.push_str("combos          (nenhum)");
    } else {
        out.push_str("combos:\n");
        for (idx, c) in combos.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&format!("  {:<16} -> {}", c.name, c.models.join(", ")));
        }
    }
    out
}

pub fn routing_cmd(paths: &Paths, args: &[String]) -> Res {
    match parse_routing(args)? {
        RoutingAction::Show => {
            let settings = load_settings(paths);
            println!("{}", format_routing(&settings.routing));
        }
        RoutingAction::SetField { field, value } => {
            Settings::save_user(paths, |s| {
                *field.slot(&mut s.routing) = value.clone();
            })?;
            match value {
                Some(m) => println!("routing.{} definido para {m}", field.label()),
                None => println!("routing.{} limpo", field.label()),
            }
        }
        RoutingAction::SetRole { agent, model } => {
            Settings::save_user(paths, |s| match &model {
                Some(m) => {
                    s.routing.role_models.insert(agent.clone(), m.clone());
                }
                None => {
                    s.routing.role_models.remove(&agent);
                }
            })?;
            match model {
                Some(m) => println!("routing.role_models.{agent} definido para {m}"),
                None => println!("routing.role_models.{agent} removido"),
            }
        }
        RoutingAction::SetCombo { name, models } => {
            let entry = ComboEntry {
                name: name.clone(),
                models: models.clone(),
                subagent_model: None,
            };
            let mut replaced = false;
            Settings::save_user(paths, |s| {
                if let Some(slot) = s.routing.combos.iter_mut().find(|c| c.name == name) {
                    *slot = entry.clone();
                    replaced = true;
                } else {
                    s.routing.combos.push(entry.clone());
                }
            })?;
            println!(
                "combo {name} {}: {}",
                if replaced { "atualizado" } else { "criado" },
                models.join(", ")
            );
        }
        RoutingAction::RemoveCombo { name } => {
            let mut removed = false;
            Settings::save_user(paths, |s| {
                let before = s.routing.combos.len();
                s.routing.combos.retain(|c| c.name != name);
                removed = s.routing.combos.len() != before;
            })?;
            if !removed {
                return Err(format!(
                    "combo '{name}' não encontrado em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!("combo {name} removido");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- tokensaver

#[derive(Debug, PartialEq)]
enum TokenSaverAction {
    Show,
    SetPonytail(PonytailLevel),
    SetCaveman(CavemanLevel),
    SetRtk(bool),
}

fn parse_tokensaver(args: &[String]) -> Result<TokenSaverAction, String> {
    match args.get(1).map(String::as_str) {
        None | Some("show") => {
            if let Some(extra) = args.get(2) {
                return Err(format!("argumento inesperado em tokensaver show: {extra}"));
            }
            Ok(TokenSaverAction::Show)
        }
        Some("ponytail") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver ponytail precisa de <off|lite|full|ultra>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let level = PonytailLevel::parse(raw).ok_or_else(|| {
                format!("nível ponytail inválido: {raw} (use off, lite, full ou ultra)")
            })?;
            Ok(TokenSaverAction::SetPonytail(level))
        }
        Some("caveman") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver caveman precisa de <off|lite|full|ultra|wenyan>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let level = CavemanLevel::parse(raw).ok_or_else(|| {
                format!("nível caveman inválido: {raw} (use off, lite, full, ultra ou wenyan)")
            })?;
            Ok(TokenSaverAction::SetCaveman(level))
        }
        Some("rtk") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver rtk precisa de <on|off|true|false>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let enabled = match raw.trim().to_ascii_lowercase().as_str() {
                "on" | "true" => true,
                "off" | "false" => false,
                _ => {
                    return Err(format!(
                        "valor rtk inválido: {raw} (use on, off, true ou false)"
                    ))
                }
            };
            Ok(TokenSaverAction::SetRtk(enabled))
        }
        Some(other) => Err(format!("comando desconhecido: tokensaver {other}")),
    }
}

pub fn format_tokensaver_show(ts: &TokenSaverSettings, summary: Option<&UsageSummary>) -> String {
    let mut out = String::new();
    out.push_str(&format!("ponytail  {}\n", ts.ponytail.as_str()));
    out.push_str(&format!("caveman   {}\n", ts.caveman.as_str()));
    out.push_str(&format!(
        "rtk       {}\n",
        if ts.rtk_enabled { "on" } else { "off" }
    ));
    match summary {
        Some(s) => {
            out.push_str("\nEstatísticas acumuladas (últimas 24h):\n");
            out.push_str(&format_token_saver_savings(ts, s));
        }
        None => {
            out.push_str("\nEstatísticas acumuladas (últimas 24h): (sem banco de uso ainda)");
        }
    }
    out
}

pub fn tokensaver_cmd(paths: &Paths, args: &[String]) -> Res {
    match parse_tokensaver(args)? {
        TokenSaverAction::Show => {
            let settings = load_settings(paths);
            let summary = if paths.db().exists() {
                Store::open(paths.db())
                    .and_then(|s| s.usage_summary(24))
                    .ok()
            } else {
                None
            };
            println!(
                "{}",
                format_tokensaver_show(&settings.token_saver, summary.as_ref())
            );
        }
        TokenSaverAction::SetPonytail(level) => {
            Settings::save_user(paths, |s| s.token_saver.ponytail = level)?;
            println!("token_saver.ponytail definido para {}", level.as_str());
        }
        TokenSaverAction::SetCaveman(level) => {
            Settings::save_user(paths, |s| s.token_saver.caveman = level)?;
            println!("token_saver.caveman definido para {}", level.as_str());
        }
        TokenSaverAction::SetRtk(enabled) => {
            Settings::save_user(paths, |s| s.token_saver.rtk_enabled = enabled)?;
            println!(
                "token_saver.rtk_enabled definido para {}",
                if enabled {
                    "ativo (true)"
                } else {
                    "desligado (false)"
                }
            );
        }
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
    login_cmd_with(paths, args, home, &zerith_core::secrets::OsKeyring)
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
        println!("Aguardando o retorno em http://127.0.0.1:{port} …");
        let code = oauth::wait_for_code(listener, oauth::redirect_path(a.tool), &state)?;
        oauth::exchange(
            a.tool,
            &code,
            &pkce.verifier,
            &oauth::redirect_uri(a.tool, port),
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
        assert!(zerith_core::secrets::load_oauth(&store, "codex").unwrap().is_none());
    }
}
