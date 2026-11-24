//! Importar do Claude Code: sessões (`~/.claude/projects/*/*.jsonl`) e
//! configuração (instruções, MCP, skills, agentes, comandos, permissões,
//! hooks e memória).
//!
//! Tudo é lido; nada sob `~/.claude` é escrito. O leitor devolve um plano
//! (`Vec<Item>`: tipo, arquivo de origem, descrição, ação) e as funções
//! `apply_*` executam só os itens com ação `Import`/`Append`. O módulo
//! `codex` usa os mesmos tipos.
//!
//! Ideias (nenhum código copiado): Goose `session/import_formats/claude_code.rs`
//! (aaif-goose/goose, Apache-2.0) para o mapeamento de blocos; Codex
//! `external-agent-migration` (openai/codex, Apache-2.0) para título,
//! `continued-in`, MCP com `${VAR}` e reescrita de termos.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use zerith_core::Paths;
use zerith_sessions::{NewMessage, Role, Session, SessionSource, Store};

// ------------------------------------------------------------ plano comum

/// O que um item do plano importa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Session,
    Instructions,
    Mcp,
    Skill,
    Command,
    Agent,
    Permission,
    Hook,
    Memory,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Session => "sessões",
            Kind::Instructions => "instruções",
            Kind::Mcp => "mcp",
            Kind::Skill => "skills",
            Kind::Command => "comandos",
            Kind::Agent => "agentes",
            Kind::Permission => "permissões",
            Kind::Hook => "hooks",
            Kind::Memory => "memória",
        }
    }
}

/// O que aplicar o item faria.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Novo aqui.
    Import,
    /// Já importado antes; a origem cresceu e só o novo entra.
    Append,
    /// Nada a fazer (já existe, vazio, desabilitado, não mapeado...).
    Skip,
    /// Mesmo nome já existe no Zerith com outro conteúdo; não sobrescreve.
    Conflict,
    /// Origem ilegível ou inválida.
    Error,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Import => "importar",
            Action::Append => "acrescentar",
            Action::Skip => "pular",
            Action::Conflict => "conflito",
            Action::Error => "erro",
        }
    }
}

/// Uma linha do plano. `payload` carrega o que `apply_*` precisa.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
    pub source_path: PathBuf,
    pub description: String,
    pub action: Action,
    pub(crate) payload: Payload,
}

#[derive(Debug, Clone)]
pub(crate) enum Payload {
    None,
    Session { origin: Origin, archived: bool },
}

/// De qual ferramenta veio uma sessão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    ClaudeCode,
    Codex,
}

impl Item {
    pub(crate) fn new(kind: Kind, path: &Path, description: String, action: Action) -> Self {
        Item {
            kind,
            source_path: path.to_path_buf(),
            description,
            action,
            payload: Payload::None,
        }
    }

    pub(crate) fn with(mut self, payload: Payload) -> Self {
        self.payload = payload;
        self
    }
}

/// Contadores de um tipo depois de aplicar.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub imported: usize,
    pub skipped: usize,
    pub conflict: usize,
    pub error: usize,
}

/// Resultado de `apply`: contadores por tipo e mensagens de erro (nunca
/// conteúdo de conversa, memória ou segredo).
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub counts: BTreeMap<Kind, Counts>,
    pub errors: Vec<String>,
}

impl Report {
    pub(crate) fn tally(&mut self, kind: Kind, action: Action) {
        let c = self.counts.entry(kind).or_default();
        match action {
            Action::Import | Action::Append => c.imported += 1,
            Action::Skip => c.skipped += 1,
            Action::Conflict => c.conflict += 1,
            Action::Error => c.error += 1,
        }
    }

    pub(crate) fn fail(&mut self, item: &Item, why: impl std::fmt::Display) {
        self.tally(item.kind, Action::Error);
        self.errors.push(format!(
            "{} {}: {why}",
            item.kind.as_str(),
            item.source_path.display()
        ));
    }

    /// `tipo  importados N  pulados N  conflitos N  erros N`, uma linha por tipo.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (k, c) in &self.counts {
            s.push_str(&format!(
                "{:<12} importados {:>4}  pulados {:>4}  conflitos {:>3}  erros {:>3}\n",
                k.as_str(),
                c.imported,
                c.skipped,
                c.conflict,
                c.error
            ));
        }
        for e in &self.errors {
            s.push_str(&format!("erro: {e}\n"));
        }
        s
    }
}

/// O plano por tipo: contagem de cada ação e uma linha por item.
pub fn render_plan(items: &[Item]) -> String {
    let mut by: BTreeMap<Kind, Vec<&Item>> = BTreeMap::new();
    for i in items {
        by.entry(i.kind).or_default().push(i);
    }
    let mut s = String::new();
    for (k, list) in by {
        let n = |a: Action| list.iter().filter(|i| i.action == a).count();
        s.push_str(&format!(
            "== {} ({}): importar {}  acrescentar {}  pular {}  conflito {}  erro {}\n",
            k.as_str(),
            list.len(),
            n(Action::Import),
            n(Action::Append),
            n(Action::Skip),
            n(Action::Conflict),
            n(Action::Error),
        ));
        for i in list {
            s.push_str(&format!(
                "  [{}] {}  ({})\n",
                i.action.as_str(),
                i.description,
                i.source_path.display()
            ));
        }
    }
    if s.is_empty() {
        s.push_str("(nada encontrado)\n");
    }
    s
}

/// Quais partes importar.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub sessions: bool,
    pub config: bool,
    /// Sem o filtro padrão (últimos 30 dias, no máximo 50 sessões).
    pub all_sessions: bool,
    /// Sessões entram na lista; sem isso entram arquivadas (buscáveis).
    pub visible: bool,
    /// "Agora", em ms (o filtro de 30 dias é relativo a ele).
    pub now_ms: i64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            sessions: true,
            config: true,
            all_sessions: false,
            visible: false,
            now_ms: zerith_sessions::migrations::now_ms(),
        }
    }
}

/// Executa os itens do plano, tipo a tipo. Itens `Skip`/`Conflict`/`Error`
/// só entram nos contadores.
pub fn apply(items: &[Item], paths: &Paths) -> Report {
    let mut r = Report::default();
    apply_sessions(items, paths, &mut r);
    r
}

// ---------------------------------------------------------------- origem

/// Onde estão os arquivos do Claude Code.
#[derive(Debug, Clone)]
pub struct ClaudeSource {
    /// `~/.claude` (ou `CLAUDE_CONFIG_DIR`).
    pub home: PathBuf,
    /// `~/.claude.json`: MCP global e por projeto.
    pub claude_json: PathBuf,
    /// `claude_desktop_config.json` do Claude Desktop, se houver.
    pub desktop_config: Option<PathBuf>,
    /// Pasta do projeto atual (`.mcp.json`, `.claude/skills`, `.claude/agents`).
    pub project: Option<PathBuf>,
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

impl ClaudeSource {
    /// `CLAUDE_CONFIG_DIR` ou `~/.claude`; `None` se não existir.
    pub fn detect(project: Option<PathBuf>) -> Option<Self> {
        let home = home_dir()?;
        let (dir, json) = match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(d) => {
                let d = PathBuf::from(d);
                (d.clone(), d.join(".claude.json"))
            }
            None => (home.join(".claude"), home.join(".claude.json")),
        };
        if !dir.is_dir() {
            return None;
        }
        let desktop = if cfg!(windows) {
            std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Claude"))
        } else if cfg!(target_os = "macos") {
            Some(home.join("Library/Application Support/Claude"))
        } else {
            Some(home.join(".config/Claude"))
        }
        .map(|d| d.join("claude_desktop_config.json"))
        .filter(|p| p.is_file());
        Some(ClaudeSource {
            home: dir,
            claude_json: json,
            desktop_config: desktop,
            project,
        })
    }
}

/// O plano completo do Claude Code.
pub fn plan(src: &ClaudeSource, paths: &Paths, opt: &Options) -> Vec<Item> {
    let mut items = Vec::new();
    if opt.sessions {
        let files = claude_session_files(&src.home);
        items.extend(plan_sessions(Origin::ClaudeCode, files, paths, opt));
    }
    items
}

// --------------------------------------------------------------- sessões

/// Uma sessão já convertida para mensagens do Zerith.
#[derive(Debug, Clone, Default)]
pub struct ParsedSession {
    pub id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Mensagem e o instante dela (ms; 0 se a origem não disse).
    pub messages: Vec<(NewMessage, i64)>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// `continued-in`: a sessão foi substituída por outra que traz o
    /// histórico dela; importar esta duplicaria a conversa.
    pub continued_in: Option<String>,
}

const TITLE_MAX: usize = 120;

/// `2026-01-02T03:04:05.678Z` / `+01:00` em ms desde a época.
pub(crate) fn parse_ts(s: &str) -> Option<i64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = s.get(19..)?;
    let mut ms = 0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.chars().take_while(char::is_ascii_digit).count();
        let frac = format!("{:0<3}", &r[..digits.min(3)]);
        ms = frac.parse().unwrap_or(0);
        rest = &r[digits..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let hh: i64 = o.get(1..3)?.parse().ok()?;
            let mm: i64 = o.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
            sign * (hh * 60 + mm)
        }
    };
    // Dias desde 1970-01-01 (algoritmo civil de Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se - offset_min * 60;
    Some(secs * 1000 + ms)
}

/// Mensagens de controle do Claude Code que abrem o texto do usuário
/// (comandos `/x`, saída de comando local, lembretes). Não servem de título.
const CONTROL_TAGS: [&str; 10] = [
    "command-message",
    "command-name",
    "command-args",
    "local-command-caveat",
    "local-command-stderr",
    "local-command-stdout",
    "task-notification",
    "system-reminder",
    "ide_opened_file",
    "ide_selection",
];

/// Primeira linha não vazia depois de tirar os blocos de controle do
/// começo, cortada em 120 caracteres. `None` se só havia controle.
pub(crate) fn title_from_user_text(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    'outer: loop {
        for tag in CONTROL_TAGS {
            let open = format!("<{tag}>");
            if let Some(after) = rest.strip_prefix(&open) {
                let close = format!("</{tag}>");
                let end = after.find(&close)?;
                rest = after[end + close.len()..].trim_start();
                continue 'outer;
            }
        }
        break;
    }
    let line = rest.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(TITLE_MAX).collect())
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// O texto de um `tool_result` (string ou lista de blocos).
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match str_of(b, "type") {
                Some("text") => str_of(b, "text").unwrap_or("").to_string(),
                Some("image") => "[imagem omitida]".to_string(),
                _ => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn join_text(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Acrescenta texto, raciocínio e chamadas à última mensagem do assistente.
/// O Claude Code grava um bloco por linha (texto, depois cada `tool_use`) e o
/// Codex um item por linha; juntar é o que mantém cada chamada seguida do seu
/// resultado na reprodução do histórico.
pub(crate) fn push_assistant(
    msgs: &mut Vec<(NewMessage, i64)>,
    merge: bool,
    ts: i64,
    text: Vec<String>,
    reasoning: Vec<String>,
    calls: Vec<Value>,
    tokens: i64,
) {
    if text.iter().all(|t| t.trim().is_empty()) && reasoning.is_empty() && calls.is_empty() {
        return;
    }
    let last = msgs
        .last_mut()
        .filter(|(m, _)| merge && m.role == Role::Assistant);
    let (m, _) = match last {
        Some(l) => l,
        None => {
            msgs.push((NewMessage::new(Role::Assistant, ""), ts));
            msgs.last_mut().expect("acabou de entrar")
        }
    };
    let mut content = vec![std::mem::take(&mut m.content)];
    content.extend(text);
    m.content = join_text(&content);
    if !reasoning.is_empty() {
        let mut r: Vec<String> = m.reasoning.take().into_iter().collect();
        r.extend(reasoning);
        m.reasoning = Some(join_text(&r)).filter(|s| !s.is_empty());
    }
    if !calls.is_empty() {
        let mut all = match m.tool_calls.take() {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        };
        all.extend(calls);
        m.tool_calls = Some(Value::Array(all));
    }
    if tokens > 0 {
        m.token_count = Some(m.token_count.unwrap_or(0) + tokens);
    }
}

/// Uma mensagem `Role::Tool` pareada com a chamada `id`.
pub(crate) fn tool_message(
    id: &str,
    name: Option<&String>,
    text: String,
    is_error: bool,
) -> NewMessage {
    NewMessage {
        tool_name: name.cloned(),
        tool_calls: Some(json!({ "tool_call_id": id })),
        raw: is_error.then(|| json!({ "is_error": true })),
        ..NewMessage::new(Role::Tool, text)
    }
}

/// Converte um transcript `.jsonl` do Claude Code.
///
/// Só linhas `user`/`assistant` (fora `isMeta` e `isSidechain`), mais
/// `custom-title`, `ai-title` e `continued-in`. Blocos: `text` → texto,
/// `thinking` → raciocínio, `tool_use` → `tool_calls` (id, nome, input),
/// `tool_result` → mensagem `Tool` pareada (`is_error` vai no `raw`),
/// `image` → marcador `[imagem omitida]`. Tokens somados por `message.id`
/// (o Claude Code repete o `usage` em cada linha da mesma resposta).
pub fn parse_claude_session(text: &str) -> Option<ParsedSession> {
    let mut p = ParsedSession::default();
    let (mut custom, mut ai, mut first_user) = (None, None, None);
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut usage_seen: HashSet<String> = HashSet::new();
    let mut merge = false;
    let mut saw_line = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if p.id.is_empty() {
            if let Some(id) = str_of(&v, "sessionId") {
                p.id = id.to_string();
            }
        }
        let ty = str_of(&v, "type").unwrap_or("");
        match ty {
            "custom-title" => custom = str_of(&v, "customTitle").map(str::to_string),
            "ai-title" => ai = str_of(&v, "aiTitle").map(str::to_string),
            "continued-in" => {
                p.continued_in = str_of(&v, "continuedInSessionId")
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            }
            _ => {}
        }
        if !matches!(ty, "user" | "assistant")
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        saw_line = true;
        if p.cwd.is_none() {
            p.cwd = str_of(&v, "cwd").map(str::to_string);
        }
        let ts = str_of(&v, "timestamp").and_then(parse_ts).unwrap_or(0);
        if ts > 0 {
            if p.created_at == 0 {
                p.created_at = ts;
            }
            p.updated_at = p.updated_at.max(ts);
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let content = msg.get("content");
        if ty == "assistant" {
            if p.model.is_none() {
                p.model = str_of(msg, "model")
                    .filter(|m| !m.starts_with('<'))
                    .map(str::to_string);
            }
            let mut tokens = 0;
            let mid = str_of(msg, "id").unwrap_or("").to_string();
            if let Some(u) = msg.get("usage").filter(|_| usage_seen.insert(mid)) {
                let n = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
                p.input_tokens += n("input_tokens");
                p.cache_read_tokens += n("cache_read_input_tokens");
                p.cache_write_tokens += n("cache_creation_input_tokens");
                p.output_tokens += n("output_tokens");
                tokens = n("output_tokens");
            }
            let (mut text, mut reasoning, mut calls) = (Vec::new(), Vec::new(), Vec::new());
            match content {
                Some(Value::String(s)) => text.push(s.clone()),
                Some(Value::Array(blocks)) => {
                    for b in blocks {
                        match str_of(b, "type") {
                            Some("text") => text.push(str_of(b, "text").unwrap_or("").into()),
                            Some("thinking") => {
                                let t = str_of(b, "thinking").unwrap_or("");
                                if !t.trim().is_empty() {
                                    reasoning.push(t.to_string());
                                }
                            }
                            Some("tool_use") => {
                                let id = str_of(b, "id").unwrap_or("").to_string();
                                let name = str_of(b, "name").unwrap_or("unknown_tool").to_string();
                                tool_names.insert(id.clone(), name.clone());
                                let input = b.get("input").cloned().unwrap_or(json!({}));
                                calls.push(json!({
                                    "id": id,
                                    "name": name,
                                    "arguments": input.to_string(),
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            push_assistant(&mut p.messages, merge, ts, text, reasoning, calls, tokens);
            merge = true;
            continue;
        }
        // user: resultados de ferramenta primeiro (respondem ao assistente
        // anterior), depois o texto que a pessoa escreveu.
        let mut texts = Vec::new();
        match content {
            Some(Value::String(s)) => texts.push(s.clone()),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    match str_of(b, "type") {
                        Some("text") => texts.push(str_of(b, "text").unwrap_or("").into()),
                        Some("image") => texts.push("[imagem omitida]".into()),
                        Some("tool_result") => {
                            let id = str_of(b, "tool_use_id").unwrap_or("");
                            let err = b.get("is_error").and_then(Value::as_bool) == Some(true);
                            let out = tool_result_text(b.get("content"));
                            p.messages
                                .push((tool_message(id, tool_names.get(id), out, err), ts));
                            merge = false;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let text = join_text(&texts);
        if !text.is_empty() {
            if first_user.is_none() {
                first_user = title_from_user_text(&text);
            }
            p.messages.push((NewMessage::new(Role::User, text), ts));
            merge = false;
        }
    }
    if !saw_line && p.continued_in.is_none() {
        return None;
    }
    p.title = custom
        .or(ai)
        .or(first_user)
        .map(|t| t.chars().take(TITLE_MAX).collect())
        .unwrap_or_else(|| "Sessão importada".to_string());
    if p.id.is_empty() {
        return None;
    }
    Some(p)
}

/// O arquivo parece um transcript do Claude Code: alguma das primeiras
/// linhas tem `sessionId` e `type` ou `uuid`.
pub fn looks_like_claude(text: &str) -> bool {
    text.lines().take(20).any(|l| {
        serde_json::from_str::<Value>(l.trim()).is_ok_and(|v| {
            v.get("sessionId").is_some() && (v.get("type").is_some() || v.get("uuid").is_some())
        })
    })
}

/// `~/.claude/projects/<pasta>/*.jsonl` (sem descer em `subagents/`).
pub fn claude_session_files(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(home.join("projects")) else {
        return out;
    };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl") && p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const RECENT_DAYS: i64 = 30;
const RECENT_MAX: usize = 50;

/// Mais recentes primeiro; com o filtro padrão, só as modificadas nos
/// últimos 30 dias e no máximo 50.
pub(crate) fn select_recent(files: Vec<PathBuf>, opt: &Options) -> Vec<PathBuf> {
    let mut v: Vec<(i64, PathBuf)> = files.into_iter().map(|p| (mtime_ms(&p), p)).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.0));
    if !opt.all_sessions {
        let cutoff = opt.now_ms - RECENT_DAYS * 86_400_000;
        v.retain(|(t, _)| *t >= cutoff);
        v.truncate(RECENT_MAX);
    }
    v.into_iter().map(|(_, p)| p).collect()
}

/// O banco do Zerith aberto só para leitura (o plano não escreve nada, nem
/// migração). `None` se ainda não existe.
pub(crate) fn open_ro(paths: &Paths) -> Option<Connection> {
    let db = paths.db();
    if !db.is_file() {
        return None;
    }
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

impl Origin {
    fn source(self) -> SessionSource {
        match self {
            Origin::ClaudeCode => SessionSource::ClaudeCode,
            Origin::Codex => SessionSource::Codex,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Origin::ClaudeCode => "claude",
            Origin::Codex => "codex",
        }
    }

    fn parse(self, path: &Path) -> Result<Option<ParsedSession>, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(match self {
            Origin::ClaudeCode => {
                if !looks_like_claude(&text) {
                    return Ok(None);
                }
                parse_claude_session(&text)
            }
            // O leitor do Codex entra no próximo commit.
            Origin::Codex => None,
        })
    }
}

/// Id local de uma sessão já importada e o cursor (maior `import_seq`
/// gravado; -1 se nenhuma mensagem).
fn existing(conn: &Connection, origin: Origin, source_id: &str) -> Option<(String, i64)> {
    let id: String = conn
        .query_row(
            "SELECT id FROM sessions WHERE source = ?1 AND source_id = ?2 AND source_profile IS NULL",
            rusqlite::params![origin.source().as_str(), source_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()?;
    let cur = cursor(conn, &id);
    Some((id, cur))
}

fn cursor(conn: &Connection, local: &str) -> i64 {
    conn.query_row(
        "SELECT COALESCE(MAX(CAST(json_extract(raw, '$.import_seq') AS INTEGER)), -1) FROM messages WHERE session_id = ?1",
        [local],
        |r| r.get(0),
    )
    .unwrap_or(-1)
}

/// Um item por arquivo de sessão: importar (nova), acrescentar (cresceu) ou
/// pular (em dia, vazia, continuada em outra).
pub(crate) fn plan_sessions(
    origin: Origin,
    files: Vec<PathBuf>,
    paths: &Paths,
    opt: &Options,
) -> Vec<Item> {
    let conn = open_ro(paths);
    let payload = Payload::Session {
        origin,
        archived: !opt.visible,
    };
    let mut items = Vec::new();
    for path in select_recent(files, opt) {
        let item = |d: String, a: Action| Item::new(Kind::Session, &path, d, a);
        let p = match origin.parse(&path) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                items.push(item(format!("ilegível: {e}"), Action::Error));
                continue;
            }
        };
        if let Some(next) = &p.continued_in {
            items.push(item(
                format!("{}: continuada em {next}", p.title),
                Action::Skip,
            ));
            continue;
        }
        let total = p.messages.len() as i64;
        let cur = conn
            .as_ref()
            .and_then(|c| existing(c, origin, &p.id))
            .map(|(_, c)| c);
        let tokens = format!(
            "{} tokens entrada, {} saída",
            p.input_tokens + p.cache_read_tokens + p.cache_write_tokens,
            p.output_tokens
        );
        let (desc, action) = match cur {
            None if total == 0 => (format!("{}: sem mensagens", p.title), Action::Skip),
            None => (
                format!("{}: {total} mensagens, {tokens}", p.title),
                Action::Import,
            ),
            Some(c) if c + 1 < total => (
                format!("{}: +{} mensagens novas", p.title, total - c - 1),
                Action::Append,
            ),
            Some(_) => (format!("{}: já importada", p.title), Action::Skip),
        };
        items.push(item(desc, action).with(payload.clone()));
    }
    items
}

/// Grava as sessões do plano. Idempotente: a sessão é achada por
/// `(origem, id de origem)` e só entram mensagens com `import_seq` acima do
/// que já foi gravado.
// ponytail: o cursor é o índice da mensagem convertida. Se a origem crescer
// com blocos que se juntam à última mensagem do assistente já gravada (uma
// resposta interrompida no meio), esses blocos não entram; reimportar do
// zero (apagar a sessão) resolve.
pub fn apply_sessions(items: &[Item], paths: &Paths, r: &mut Report) {
    let mine: Vec<&Item> = items.iter().filter(|i| i.kind == Kind::Session).collect();
    if mine.is_empty() {
        return;
    }
    let store = match paths
        .ensure()
        .map_err(|e| e.to_string())
        .and_then(|_| Store::open(paths.db()).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => {
            for i in mine {
                r.fail(i, &e);
            }
            return;
        }
    };
    for item in mine {
        let Payload::Session { origin, archived } = item.payload else {
            r.tally(item.kind, item.action);
            continue;
        };
        if !matches!(item.action, Action::Import | Action::Append) {
            r.tally(item.kind, item.action);
            continue;
        }
        match import_session(&store, origin, &item.source_path, archived) {
            Ok(true) => r.tally(item.kind, item.action),
            Ok(false) => r.tally(item.kind, Action::Skip),
            Err(e) => r.fail(item, e),
        }
    }
}

/// `Ok(true)`: criou a sessão ou acrescentou mensagens.
fn import_session(
    store: &Store,
    origin: Origin,
    path: &Path,
    archived: bool,
) -> Result<bool, String> {
    let Some(p) = origin.parse(path)? else {
        return Ok(false);
    };
    let session = Session {
        id: format!("{}-{}", origin.prefix(), p.id),
        title: p.title.clone(),
        source: origin.source(),
        source_id: Some(p.id.clone()),
        source_profile: None,
        model: p.model.clone(),
        provider: None,
        created_at: p.created_at,
        updated_at: p.updated_at,
        archived,
    };
    let err = |e: zerith_sessions::store::StoreError| e.to_string();
    let (local, created) = store.upsert_imported(&session).map_err(err)?;
    if created {
        if let Some(cwd) = &p.cwd {
            store.set_workspace(&local, cwd).map_err(err)?;
        }
    }
    let cur = cursor(store.conn(), &local);
    let mut n = 0;
    for (seq, (m, ts)) in p.messages.iter().enumerate() {
        if seq as i64 <= cur {
            continue;
        }
        let mut m = m.clone();
        let mut raw = match m.raw.take() {
            Some(Value::Object(o)) => o,
            _ => serde_json::Map::new(),
        };
        raw.insert("import_seq".into(), json!(seq));
        raw.insert("import_source".into(), json!(origin.source().as_str()));
        m.raw = Some(Value::Object(raw));
        let id = store.append(&local, &m).map_err(err)?;
        if *ts > 0 {
            let _ = store.conn().execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![ts, id],
            );
        }
        n += 1;
    }
    // `append` marca a sessão como mexida agora; ela é de quando foi na origem.
    if p.updated_at > 0 {
        let _ = store.conn().execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![p.updated_at, local],
        );
    }
    Ok(created || n > 0)
}

// ------------------------------------------------------------------ testes

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let t = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t
    }

    pub(crate) fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub(crate) const CC_SESSION: &str = r#"{"type":"queue-operation","sessionId":"s1","timestamp":"2026-11-20T10:00:00.000Z"}
{"type":"user","sessionId":"s1","uuid":"u0","isMeta":true,"timestamp":"2026-11-20T10:00:00.000Z","cwd":"/w/proj","message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"}}
{"type":"user","sessionId":"s1","uuid":"u1","timestamp":"2026-11-20T10:00:01.000Z","cwd":"/w/proj","message":{"role":"user","content":"<command-name>/init</command-name>\n<command-args></command-args>"}}
{"type":"user","sessionId":"s1","uuid":"u2","timestamp":"2026-11-20T10:00:02.000Z","cwd":"/w/proj","message":{"role":"user","content":[{"type":"text","text":"liste os arquivos"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}]}}
{"type":"assistant","sessionId":"s1","uuid":"a1","timestamp":"2026-11-20T10:00:03.000Z","message":{"id":"m1","model":"claude-x","role":"assistant","content":[{"type":"thinking","thinking":"vou usar ls","signature":"sig"}],"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":7}}}
{"type":"assistant","sessionId":"s1","uuid":"a2","timestamp":"2026-11-20T10:00:03.500Z","message":{"id":"m1","model":"claude-x","role":"assistant","content":[{"type":"text","text":"Vou listar."}],"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":7}}}
{"type":"assistant","sessionId":"s1","uuid":"a3","timestamp":"2026-11-20T10:00:04.000Z","message":{"id":"m1","model":"claude-x","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"assistant","sessionId":"s1","uuid":"a4","timestamp":"2026-11-20T10:00:04.100Z","message":{"id":"m1","model":"claude-x","role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"x"}}]}}
{"type":"user","sessionId":"s1","uuid":"u3","timestamp":"2026-11-20T10:00:05.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"a.txt"}]},{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,"content":"não existe"}]}}
{"type":"assistant","sessionId":"s1","uuid":"a5","isSidechain":true,"timestamp":"2026-11-20T10:00:05.500Z","message":{"id":"m9","role":"assistant","content":[{"type":"text","text":"subagente"}]}}
{"type":"assistant","sessionId":"s1","uuid":"a6","timestamp":"2026-11-20T10:00:06.000Z","message":{"id":"m2","model":"claude-x","role":"assistant","content":[{"type":"text","text":"Há a.txt."}],"usage":{"input_tokens":3,"output_tokens":2}}}
{"type":"ai-title","sessionId":"s1","aiTitle":"Listar arquivos"}
"#;

    #[test]
    fn claude_blocks_map_and_calls_pair_with_results() {
        let p = parse_claude_session(CC_SESSION).unwrap();
        assert_eq!(p.id, "s1");
        assert_eq!(p.title, "Listar arquivos");
        assert_eq!(p.cwd.as_deref(), Some("/w/proj"));
        assert_eq!(p.model.as_deref(), Some("claude-x"));
        let roles: Vec<Role> = p.messages.iter().map(|(m, _)| m.role).collect();
        use Role::*;
        assert_eq!(roles, [User, User, Assistant, Tool, Tool, Assistant]);
        assert_eq!(
            p.messages[1].0.content,
            "liste os arquivos\n\n[imagem omitida]"
        );
        let a = &p.messages[2].0;
        assert_eq!(a.content, "Vou listar.");
        assert_eq!(a.reasoning.as_deref(), Some("vou usar ls"));
        let calls = a.tool_calls.as_ref().unwrap().as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "toolu_1");
        assert_eq!(calls[0]["name"], "Bash");
        assert_eq!(calls[0]["arguments"], r#"{"command":"ls"}"#);
        let t1 = &p.messages[3].0;
        assert_eq!(t1.content, "a.txt");
        assert_eq!(t1.tool_name.as_deref(), Some("Bash"));
        assert_eq!(t1.tool_calls.as_ref().unwrap()["tool_call_id"], "toolu_1");
        assert!(t1.raw.is_none());
        let t2 = &p.messages[4].0;
        assert_eq!(t2.raw.as_ref().unwrap()["is_error"], true);
        // Uso contado uma vez por message.id.
        assert_eq!(p.input_tokens, 13);
        assert_eq!(p.cache_read_tokens, 100);
        assert_eq!(p.output_tokens, 9);
        assert_eq!(p.created_at, parse_ts("2026-11-20T10:00:01Z").unwrap());
    }

    #[test]
    fn titles_prefer_custom_then_ai_then_first_real_message() {
        let custom = format!(
            "{CC_SESSION}{}\n",
            r#"{"type":"custom-title","sessionId":"s1","customTitle":"Meu título"}"#
        );
        assert_eq!(parse_claude_session(&custom).unwrap().title, "Meu título");
        let no_ai: String = CC_SESSION
            .lines()
            .filter(|l| !l.contains("ai-title"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            parse_claude_session(&no_ai).unwrap().title,
            "liste os arquivos"
        );
        assert_eq!(
            title_from_user_text("<command-name>/x</command-name>"),
            None
        );
        let long = "a".repeat(300);
        assert_eq!(title_from_user_text(&long).unwrap().chars().count(), 120);
        let cont = format!(
            "{CC_SESSION}{}\n",
            r#"{"type":"continued-in","sessionId":"s1","continuedInSessionId":"s2"}"#
        );
        assert_eq!(
            parse_claude_session(&cont).unwrap().continued_in.as_deref(),
            Some("s2")
        );
    }

    #[test]
    fn timestamps_parse_with_zone() {
        assert_eq!(parse_ts("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_ts("2000-03-01T00:00:00Z"), Some(951_868_800_000));
        assert_eq!(parse_ts("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_ts("lixo"), None);
    }

    #[test]
    fn claude_sessions_import_once_and_append_only_what_grew() {
        let t = scratch("cc-sessions");
        let home = t.join("claude");
        let file = home.join("projects/-w-proj/s1.jsonl");
        write(&file, CC_SESSION);
        let paths = Paths::with_root(t.join("zerith"));
        let src = ClaudeSource {
            home: home.clone(),
            claude_json: t.join("none.json"),
            desktop_config: None,
            project: None,
        };
        let opt = Options {
            config: false,
            ..Options::default()
        };
        let plan1 = plan(&src, &paths, &opt);
        assert_eq!(plan1.len(), 1);
        assert_eq!(plan1[0].action, Action::Import);
        assert!(!paths.db().exists(), "o plano não cria o banco");
        let r = apply(&plan1, &paths);
        assert_eq!(r.counts[&Kind::Session].imported, 1, "{:?}", r.errors);

        let store = Store::open(paths.db()).unwrap();
        let s = store.session("claude-s1").unwrap().unwrap();
        assert!(s.archived);
        assert_eq!(s.source, SessionSource::ClaudeCode);
        assert_eq!(s.updated_at, parse_ts("2026-11-20T10:00:06Z").unwrap());
        assert_eq!(
            store.workspace("claude-s1").unwrap().as_deref(),
            Some("/w/proj")
        );
        assert_eq!(store.message_count("claude-s1").unwrap(), 6);

        // De novo: nada muda.
        let plan2 = plan(&src, &paths, &opt);
        assert_eq!(plan2[0].action, Action::Skip);
        let r = apply(&plan2, &paths);
        assert_eq!(r.counts[&Kind::Session].skipped, 1);
        assert_eq!(store.message_count("claude-s1").unwrap(), 6);

        // Cresceu: só o novo entra.
        let grown = format!(
            "{CC_SESSION}{}\n{}\n",
            r#"{"type":"user","sessionId":"s1","uuid":"u9","timestamp":"2026-11-21T10:00:00Z","message":{"role":"user","content":"e agora?"}}"#,
            r#"{"type":"assistant","sessionId":"s1","uuid":"a9","timestamp":"2026-11-21T10:00:01Z","message":{"id":"m3","role":"assistant","content":[{"type":"text","text":"pronto"}]}}"#
        );
        write(&file, &grown);
        let plan3 = plan(&src, &paths, &opt);
        assert_eq!(plan3[0].action, Action::Append);
        apply(&plan3, &paths);
        let msgs = store.messages("claude-s1", 100, 0).unwrap();
        assert_eq!(msgs.len(), 8);
        assert_eq!(msgs[6].content, "e agora?");
        assert_eq!(msgs[7].content, "pronto");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn default_filter_keeps_recent_sessions_only() {
        let t = scratch("cc-recent");
        let f = t.join("a.jsonl");
        write(&f, "x");
        let opt = Options::default();
        assert_eq!(select_recent(vec![f.clone()], &opt).len(), 1);
        let future = Options {
            now_ms: opt.now_ms + 31 * 86_400_000,
            ..opt
        };
        assert!(select_recent(vec![f.clone()], &future).is_empty());
        let all = Options {
            all_sessions: true,
            ..future
        };
        assert_eq!(select_recent(vec![f], &all).len(), 1);
        let _ = std::fs::remove_dir_all(&t);
    }
}
