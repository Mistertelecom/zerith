//! `zerith import hermes --all`: a migração inteira do Hermes num plano só.
//!
//! Cada etapa roda duas vezes com o mesmo código: no plano (`apply = false`)
//! só lê e registra o que faria; na aplicação faz e registra o que fez. Assim
//! o plano mostrado é exatamente o que será aplicado. Nada sob o home do
//! Hermes é escrito; valores de segredo nunca vão para o plano, o relatório
//! ou o log (só nomes).
//!
//! Base em [`crate::plan`]; ideias do `hermes import-agent` / `claw migrate`
//! do Hermes (MIT).

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use zerith_core::secrets::SecretStore;
use zerith_core::Paths;
use zerith_sessions::Store;

use crate::plan::{self, ImportPlan, Ledger, Status};

/// Tipos de item, na ordem em que aparecem no plano.
pub const KINDS: [&str; 8] = [
    "sessao",
    "memoria",
    "persona",
    "credencial",
    "skill",
    "mcp",
    "cron",
    "telegram",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub dry_run: bool,
    /// Substitui o que já existe e difere (bloco do AGENTS.md, dispositivo
    /// do inventário, token do Telegram). Sem ela: `conflict`.
    pub overwrite: bool,
    /// Senhas de `creds/` e o token do Telegram vão para o cofre. Sem ela, o
    /// plano só lista os nomes.
    pub migrate_secrets: bool,
    /// Só este perfil (`default` = o home).
    pub profile: Option<String>,
    pub only_pinned_visible: bool,
    /// "Hermes" → "Zerith" na persona.
    pub rebrand: bool,
    /// Importa também skills embutidas que a pessoa alterou.
    pub customized_skills: bool,
    /// Tipos a migrar ([`KINDS`]); `None` = todos.
    pub kinds: Option<BTreeSet<String>>,
    /// Outras pastas `creds/` além de `<home>/creds` (ex.: `~/.hermes/creds`).
    pub extra_creds: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dry_run: true,
            overwrite: false,
            migrate_secrets: false,
            profile: None,
            only_pinned_visible: false,
            rebrand: true,
            customized_skills: false,
            kinds: None,
            extra_creds: Vec::new(),
        }
    }
}

impl Options {
    fn wants(&self, kind: &str) -> bool {
        self.kinds.as_ref().is_none_or(|k| k.contains(kind))
    }
}

/// O estado de uma rodada: plano sendo montado, ledger e a pasta da migração.
pub(crate) struct Ctx<'a> {
    pub paths: &'a Paths,
    pub home: &'a Path,
    pub opts: &'a Options,
    /// O cofre: lido no plano, escrito só na aplicação.
    pub secrets: Option<&'a dyn SecretStore>,
    pub plan: ImportPlan,
    pub ledger: Ledger,
    /// `<raiz>/migrations/<stamp>/` (só na aplicação).
    pub dir: Option<PathBuf>,
}

impl Ctx<'_> {
    pub fn apply(&self) -> bool {
        !self.opts.dry_run
    }

    /// Banco do Zerith só para leitura (plano): `None` se ainda não existe.
    pub fn read_db(&self) -> Option<rusqlite::Connection> {
        rusqlite::Connection::open_with_flags(
            self.paths.db(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()
    }

    /// Caminho relativo ao home do Hermes, com `/` (absoluto se estiver fora).
    pub fn rel(&self, p: &Path) -> String {
        p.strip_prefix(self.home)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Onde um arquivo arquivado iria: `archive/<rel>`.
    pub fn archive_path(&self, rel: &str) -> String {
        format!("archive/{rel}")
    }

    /// Grava em `archive/<rel>` (só na aplicação).
    pub fn archive(&self, rel: &str, content: &[u8]) -> std::io::Result<()> {
        self.write_under("archive", rel, content)
    }

    pub fn overflow(&self, rel: &str, content: &[u8]) -> std::io::Result<()> {
        self.write_under("overflow", rel, content)
    }

    fn write_under(&self, sub: &str, rel: &str, content: &[u8]) -> std::io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let path = dir.join(sub).join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, content)
    }
}

/// Monta o plano (com `opts.dry_run`) ou aplica: backup antes, relatório e
/// ledger depois. `secrets` é o cofre (o do sistema em produção, um mapa nos
/// testes); só é usado na aplicação.
pub fn run(
    home: &Path,
    paths: &Paths,
    opts: &Options,
    secrets: &dyn SecretStore,
) -> Result<ImportPlan, String> {
    if !home.is_dir() {
        return Err(format!("home do Hermes não encontrado: {}", home.display()));
    }
    let mut ctx = Ctx {
        paths,
        home,
        opts,
        secrets: Some(secrets),
        plan: ImportPlan::new("hermes", home, opts.dry_run),
        ledger: Ledger::load(paths),
        dir: None,
    };
    let stamp = plan::stamp();
    if ctx.apply() {
        paths.ensure().map_err(|e| e.to_string())?;
        let b = plan::backup(paths, &stamp).map_err(|e| format!("backup: {e}"))?;
        ctx.plan.backup_dir = Some(b.display().to_string());
        let dir = plan::migration_dir(paths, &stamp).map_err(|e| e.to_string())?;
        ctx.plan.report_dir = Some(dir.display().to_string());
        ctx.dir = Some(dir);
    }
    if opts.wants("sessao") {
        sessions(&mut ctx);
    }
    if opts.wants("memoria") {
        memories(&mut ctx);
    }
    if opts.wants("persona") {
        persona(&mut ctx);
    }
    if opts.wants("credencial") {
        credentials(&mut ctx);
    }
    if opts.wants("skill") {
        skills(&mut ctx);
    }
    if opts.wants("mcp") {
        mcp(&mut ctx);
    }
    if opts.wants("cron") {
        cron(&mut ctx);
    }
    if opts.wants("telegram") {
        telegram(&mut ctx);
    }
    if ctx.apply() {
        ctx.ledger.save(paths).map_err(|e| format!("ledger: {e}"))?;
        if let Some(dir) = &ctx.dir {
            plan::write_report(dir, &ctx.plan).map_err(|e| format!("relatório: {e}"))?;
        }
    }
    Ok(ctx.plan)
}

/// `(perfil, state.db)` deste home: `state.db` é `default`;
/// `profiles/<nome>/state.db` é `<nome>`. Filtrado por `opts.profile`.
pub(crate) fn profile_dirs(home: &Path, only: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut out = vec![("default".to_string(), home.to_path_buf())];
    if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
        let mut v: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        v.sort();
        out.extend(v);
    }
    out.retain(|(p, _)| only.is_none_or(|o| o == p));
    out
}

// ---------------------------------------------------------------- conversas

fn sessions(ctx: &mut Ctx) {
    for (profile, dir) in profile_dirs(ctx.home, ctx.opts.profile.as_deref()) {
        let db = dir.join("state.db");
        if !db.is_file() {
            continue;
        }
        let src = ctx.rel(&db);
        let preview = match crate::preview(&db) {
            Ok(p) => p,
            Err(e) => {
                ctx.plan
                    .push("sessao", &src, "zerith.db", Status::Error, e.to_string());
                continue;
            }
        };
        let known: HashSet<String> = ctx
            .read_db()
            .and_then(|c| {
                let mut st = c
                    .prepare("SELECT source_id FROM sessions WHERE source = 'hermes' AND source_profile IS ?1")
                    .ok()?;
                let rows = st
                    .query_map([&profile], |r| r.get::<_, String>(0))
                    .ok()?;
                Some(rows.flatten().collect())
            })
            .unwrap_or_default();
        let new: Vec<_> = preview
            .sessions
            .iter()
            .filter(|s| !known.contains(&s.id))
            .collect();
        let pinned = new.iter().filter(|s| s.pinned).count();
        let archived = new
            .iter()
            .filter(|s| s.archived || (ctx.opts.only_pinned_visible && !s.pinned))
            .count();
        let dest = format!("zerith.db (perfil {profile})");
        if !ctx.apply() {
            if new.is_empty() {
                ctx.plan.push(
                    "sessao",
                    &src,
                    dest,
                    Status::Skipped,
                    format!("{} conversas já importadas", preview.sessions.len()),
                );
            } else {
                ctx.plan.push(
                    "sessao",
                    &src,
                    dest,
                    Status::Imported,
                    format!(
                        "{} conversas novas ({pinned} fixadas, {archived} arquivadas), {} já importadas",
                        new.len(),
                        known.len()
                    ),
                );
            }
            continue;
        }
        let opts = crate::SessionOptions {
            include_archived: true,
            only_ids: None,
            only_pinned_visible: ctx.opts.only_pinned_visible,
        };
        let r = Store::open(ctx.paths.db())
            .map_err(crate::ImportError::from)
            .and_then(|store| crate::import_with(&db, &profile, &store, &opts));
        match r {
            Ok(r) if r.sessions_imported + r.sessions_resumed == 0 && r.failed.is_empty() => {
                ctx.plan.push(
                    "sessao",
                    &src,
                    dest,
                    Status::Skipped,
                    format!("{} conversas já importadas", r.sessions_skipped),
                )
            }
            Ok(r) => ctx.plan.push(
                "sessao",
                &src,
                dest,
                if r.failed.is_empty() {
                    Status::Imported
                } else {
                    Status::Error
                },
                format!(
                    "{} novas ({pinned} fixadas, {archived} arquivadas), {} retomadas, {} mensagens{}",
                    r.sessions_imported,
                    r.sessions_resumed,
                    r.messages_imported,
                    if r.failed.is_empty() {
                        String::new()
                    } else {
                        format!(", {} com erro", r.failed.len())
                    }
                ),
            ),
            Err(e) => ctx
                .plan
                .push("sessao", &src, dest, Status::Error, e.to_string()),
        }
    }
}

// ------------------------------------------------------------------ memória

/// Entradas de um `MEMORY.md`/`USER.md` do Hermes: separadas por uma linha
/// só com `§` (o `ENTRY_DELIMITER = "\n§\n"` do Hermes).
pub fn memory_entries(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim() == "§" {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Espaços colapsados e caixa baixa: a chave de deduplicação.
pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Um valor secreto tirado do texto: o nome no cofre e o valor (que só sai
/// daqui para o cofre).
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    pub name: String,
    pub value: String,
}

/// Parece senha: 4+ caracteres com dígito ou símbolo (não `TCP/UDP`).
fn looks_like_password(v: &str) -> bool {
    v.chars().count() >= 4
        && v.chars()
            .any(|c| c.is_ascii_digit() || (!c.is_alphanumeric() && c != '-' && c != '.'))
}

/// Tira valores secretos de um texto livre e os troca por
/// "valor no cofre (<nome>)", com nomes `<prefixo>-<8 hex do valor>`
/// (determinístico: rodar de novo dá o mesmo nome). Depois troca as palavras
/// que o filtro da memória recusa (`senha`, `password`, `community`…) por
/// sinônimos neutros. O texto devolvido pode ainda ser recusado (uma chave
/// `sk-…` solta, por exemplo); quem chama decide.
pub fn extract_secrets(text: &str, prefix: &str) -> (String, Vec<Extracted>) {
    let (out, found) = extract_values(text, prefix);
    (neutral_words(&out), found)
}

/// [`extract_secrets`] sem a troca de palavras (para o inventário, que não
/// passa pelo filtro da memória).
pub fn extract_values(text: &str, prefix: &str) -> (String, Vec<Extracted>) {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<[Regex; 4]> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            // SNMPv2c <valor>=WRITE
            Regex::new(r"(?i)\b(snmpv[123]c?(?:\s+community)?)(\s+)([^\s=,;()]+)(=\w+)?").unwrap(),
            // senha: x / password = x / senha do roteador é x
            Regex::new(r"(?i)\b(senha|password|passwd|pwd|secret|community|token)\b([^\n:=]{0,40}?\s*(?:[:=]|\bé\b|\beh\b|\bis\b)\s*)([^\s,;()]+)").unwrap(),
            // senha x (sem separador): só se x parece senha
            Regex::new(r"(?i)\b(senha|password|passwd|pwd|community)(\s+)([^\s,;()]+)").unwrap(),
            // (usuario/senha)
            Regex::new(r"\((\s*[\w.@-]+\s*)/(\s*)([^\s/()]+)\s*\)").unwrap(),
        ]
    });
    let mut found: Vec<Extracted> = Vec::new();
    let mut out = text.to_string();
    for (i, re) in res.iter().enumerate() {
        let mut next = String::with_capacity(out.len());
        let mut last = 0;
        for c in re.captures_iter(&out) {
            let whole = c.get(0).unwrap();
            let value = c.get(3).unwrap().as_str();
            let already = value.starts_with("valor")
                || value.contains("cofre")
                || c.get(2).is_some_and(|g| g.as_str().contains("cofre"));
            // Depois de `snmpv2c` ou de `senha:` qualquer valor conta; sem
            // separador só o que parece senha (não "senha forte", "TCP/UDP").
            let marked = i == 1 || (i == 0 && c.get(4).is_some());
            let ok = !already && (marked || looks_like_password(value));
            if !ok {
                continue;
            }
            let name = format!("{prefix}-{}", &plan::sha256_hex(value.as_bytes())[..8]);
            next.push_str(&out[last..whole.start()]);
            let repl = if i == 3 {
                format!(
                    "(usuário {}, valor no cofre ({name}))",
                    c.get(1).unwrap().as_str().trim()
                )
            } else {
                format!(
                    "{}{}valor no cofre ({name})",
                    c.get(1).unwrap().as_str(),
                    c.get(2).unwrap().as_str()
                )
            };
            next.push_str(&repl);
            next.push_str(c.get(4).map_or("", |m| m.as_str()));
            last = whole.end();
            if !found.iter().any(|f| f.name == name) {
                found.push(Extracted {
                    name,
                    value: value.to_string(),
                });
            }
        }
        next.push_str(&out[last..]);
        out = next;
    }
    (out, found)
}

/// As palavras que `mentions_secret` recusa, trocadas por sinônimos.
fn neutral_words(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)token de acesso|chave de api|api key|senhas?|passwords?|passwd|communit(?:y|ies)|secret[ _]").unwrap()
    });
    re.replace_all(text, |c: &regex::Captures| {
        let low = c.get(0).unwrap().as_str().to_lowercase();
        let r = if low.starts_with("token") {
            "credencial de acesso"
        } else if low.starts_with("chave") || low.starts_with("api") {
            "chave da API"
        } else if low.starts_with("communit") {
            "comunidade SNMP"
        } else if low == "secret_" {
            "chave_"
        } else if low == "secret " {
            "chave "
        } else if low.ends_with('s') && !low.ends_with("ss") {
            "credenciais"
        } else {
            "credencial"
        };
        r.to_string()
    })
    .into_owned()
}

/// Tipo do fato: `USER.md` com regra → instrução, resto de `USER.md` →
/// preferência; `MEMORY.md` → entidade.
fn memory_kind(is_user: bool, text: &str) -> zerith_memory::FactKind {
    use zerith_memory::FactKind;
    let low = text.to_lowercase();
    if !is_user {
        FactKind::Entity
    } else if ["regra", "sempre", "nunca", "antes de"]
        .iter()
        .any(|w| low.contains(w))
    {
        FactKind::Instruction
    } else {
        FactKind::Preference
    }
}

fn memories(ctx: &mut Ctx) {
    use zerith_memory::{Confidence, MemoryBackend};
    let scope = zerith_memory::default_scope();
    let mut seen: HashSet<String> = ctx
        .read_db()
        .and_then(|c| {
            let mut st = c
                .prepare("SELECT text FROM memory_facts WHERE deleted = 0")
                .ok()?;
            let rows = st.query_map([], |r| r.get::<_, String>(0)).ok()?;
            Some(rows.flatten().map(|t| normalize(&t)).collect())
        })
        .unwrap_or_default();
    let mem = if ctx.apply() {
        match zerith_memory::SqliteMemory::open(ctx.paths.db()) {
            Ok(m) => Some(m),
            Err(e) => {
                ctx.plan
                    .push("memoria", "", "memory_facts", Status::Error, e.to_string());
                return;
            }
        }
    } else {
        None
    };
    for (profile, dir) in profile_dirs(ctx.home, ctx.opts.profile.as_deref()) {
        for file in ["MEMORY.md", "USER.md"] {
            let path = dir.join("memories").join(file);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let src = format!("{profile}/memories/{file}");
            let sha = plan::sha256_hex(raw.as_bytes());
            if ctx.ledger.unchanged(&path, &sha) {
                ctx.plan.push(
                    "memoria",
                    &src,
                    "memory_facts",
                    Status::Skipped,
                    "arquivo igual ao da última migração",
                );
                continue;
            }
            let is_user = file == "USER.md";
            let mut errors = 0;
            let mut overflow = String::new();
            for (n, entry) in memory_entries(&raw).into_iter().enumerate() {
                let item_src = format!("{src}#{}", n + 1);
                let (text, values) = extract_secrets(&entry, "hermes-memoria");
                let kind = memory_kind(is_user, &text);
                let dest = format!("memory_facts ({})", kind.as_str());
                if !seen.insert(normalize(&text)) {
                    ctx.plan
                        .push("memoria", item_src, dest, Status::Skipped, "já na memória");
                    continue;
                }
                if zerith_security::mentions_secret(&text) {
                    // Vai o texto já sem os valores extraídos; o que o filtro
                    // ainda viu, a pessoa revisa.
                    overflow.push_str(&text);
                    overflow.push_str("\n§\n");
                    ctx.plan.push(
                        "memoria",
                        item_src,
                        format!("overflow/memoria-{profile}-{file}"),
                        Status::Archived,
                        "o filtro de segredos da memória recusou o texto; revise e guarde o valor com zerith secret set",
                    );
                    continue;
                }
                let names: Vec<&str> = values.iter().map(|v| v.name.as_str()).collect();
                let note = if names.is_empty() {
                    String::new()
                } else {
                    format!("valor(es) no cofre: {}", names.join(", "))
                };
                if let (Some(mem), Some(vault), true) = (&mem, ctx.secrets, ctx.apply()) {
                    let source = format!("hermes:{profile}/{file}");
                    let done = values
                        .iter()
                        .try_for_each(|v| vault.set(&v.name, &v.value))
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            mem.remember(&scope, kind, &text, Confidence::Confirmed, Some(&source))
                                .map_err(|e| e.to_string())
                        });
                    if let Err(e) = done {
                        errors += 1;
                        ctx.plan.push("memoria", item_src, dest, Status::Error, e);
                        continue;
                    }
                }
                ctx.plan
                    .push("memoria", item_src, dest, Status::Imported, note);
            }
            if !overflow.is_empty() {
                let _ = ctx.overflow(&format!("memoria-{profile}-{file}"), overflow.as_bytes());
            }
            if ctx.apply() && errors == 0 {
                ctx.ledger.record(&path, &sha, "memory_facts");
            }
        }
    }
}

// ------------------------------------------------------------------ persona

/// Acima disso, um `AGENTS.md` cheio de tabelas é guia de pacote, não do usuário.
const GENERIC_AGENTS_BYTES: usize = 6 * 1024;

/// `AGENTS.md` que veio de um pacote (ECC, guia de agentes genérico), e não
/// instruções da pessoa. `Some(motivo)` quando é.
pub fn generic_agents_md(text: &str) -> Option<&'static str> {
    if text.contains("Everything Claude Code (ECC)") {
        return Some("guia genérico do pacote Everything Claude Code (ECC)");
    }
    let table_rows = text
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .count();
    let low = text.to_lowercase();
    (text.len() > GENERIC_AGENTS_BYTES
        && table_rows >= 10
        && (low.contains("available agents") || low.contains("| agent")))
    .then_some("guia genérico de agentes (grande, com tabelas de agentes)")
}

/// Troca o bloco `id` do `AGENTS.md` (entre comentários marcadores) ou o
/// acrescenta no fim, sem mexer no resto. Devolve o texto novo e o corpo
/// antigo do bloco, se havia.
pub fn upsert_block(doc: &str, id: &str, body: &str) -> (String, Option<String>) {
    let start = format!("<!-- zerith:import {id} -->");
    let end = format!("<!-- /zerith:import {id} -->");
    let block = format!("{start}\n{}\n{end}", body.trim());
    if let (Some(a), Some(b)) = (doc.find(&start), doc.find(&end)) {
        if b > a {
            let old = doc[a + start.len()..b].trim().to_string();
            let new = format!("{}{block}{}", &doc[..a], &doc[b + end.len()..]);
            return (new, Some(old));
        }
    }
    let sep = if doc.is_empty() || doc.ends_with("\n\n") {
        ""
    } else if doc.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    (format!("{doc}{sep}{block}\n"), None)
}

fn persona(ctx: &mut Ctx) {
    let dest_path = ctx.paths.instructions();
    let dest = "AGENTS.md";
    let bundled = ctx.home.join("hermes-agent");
    let sources = [
        ("SOUL.md", "hermes:soul", "## Estilo (importado do Hermes)"),
        (
            "AGENTS.md",
            "hermes:agents",
            "## Instruções (importadas do Hermes)",
        ),
    ];
    for (file, id, title) in sources {
        let path = ctx.home.join(file);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let sha = plan::sha256_hex(raw.as_bytes());
        if ctx.ledger.unchanged(&path, &sha) {
            ctx.plan.push(
                "persona",
                file,
                dest,
                Status::Skipped,
                "arquivo igual ao da última migração",
            );
            continue;
        }
        if std::fs::read_to_string(bundled.join(file)).is_ok_and(|b| b == raw) {
            ctx.plan.push(
                "persona",
                file,
                dest,
                Status::Skipped,
                "é o texto padrão que vem com o Hermes",
            );
            continue;
        }
        if file == "AGENTS.md" {
            if let Some(why) = generic_agents_md(&raw) {
                let rel = "persona/AGENTS.md";
                if ctx.apply() && ctx.archive(rel, raw.as_bytes()).is_ok() {
                    ctx.ledger.record(&path, &sha, rel);
                }
                ctx.plan.push(
                    "persona",
                    file,
                    ctx.archive_path(rel),
                    Status::Archived,
                    format!("{why}; copie para um projeto se quiser usá-lo"),
                );
                continue;
            }
        }
        let text = if ctx.opts.rebrand {
            plan::rebrand(&raw, "Hermes", "Zerith")
        } else {
            raw.clone()
        };
        let body = format!("{title}\n\n{}", text.trim());
        let current = std::fs::read_to_string(&dest_path).unwrap_or_default();
        let (next, old) = upsert_block(&current, id, &body);
        let (status, reason) = match old {
            Some(o) if o == body.trim() => (Status::Skipped, "seção já presente".to_string()),
            Some(_) if !ctx.opts.overwrite => (
                Status::Conflict,
                "a seção importada foi editada no Zerith; use --overwrite para substituir".into(),
            ),
            Some(_) => (Status::Imported, "seção substituída".into()),
            None => (
                Status::Imported,
                "seção acrescentada (o resto do arquivo fica como está)".into(),
            ),
        };
        if status == Status::Imported && ctx.apply() {
            if let Err(e) = std::fs::write(&dest_path, next) {
                ctx.plan
                    .push("persona", file, dest, Status::Error, e.to_string());
                continue;
            }
        }
        if status != Status::Conflict && ctx.apply() {
            ctx.ledger.record(&path, &sha, dest);
        }
        ctx.plan.push("persona", file, dest, status, reason);
    }
}

// -------------------------------------------------------------- credenciais

/// Um `creds/*.txt` do Hermes: linhas `chave: valor` e `# comentário`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CredFile {
    pub fields: Vec<(String, String)>,
    pub comments: Vec<String>,
    /// Linhas sem `chave:` (listas de segredos).
    pub loose: Vec<String>,
}

impl CredFile {
    pub fn parse(text: &str) -> Self {
        let mut f = CredFile::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(c) = line.strip_prefix('#') {
                f.comments.push(c.trim().to_string());
                continue;
            }
            match line.split_once(':') {
                Some((k, v))
                    if !k.trim().is_empty()
                        && k.trim().chars().all(|c| c.is_alphanumeric() || c == '_') =>
                {
                    f.fields
                        .push((k.trim().to_lowercase(), v.trim().to_string()));
                }
                _ => f.loose.push(line.to_string()),
            }
        }
        f
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

/// O que um arquivo de credencial vira: um dispositivo (com os segredos que
/// iriam para o cofre) ou um segredo só (arquivo sem host).
#[derive(Debug, Clone, PartialEq)]
pub enum CredTarget {
    Device {
        dev: Box<zerith_tools::network::DeviceEntry>,
        /// `(nome no cofre, valor)`; o valor só vai para o cofre.
        secrets: Vec<(String, String)>,
    },
    Secret {
        name: String,
        value: String,
    },
}

/// `cliente-nome.txt` → (`Some("cliente")`, `"nome"`); sem `-`, sem cliente.
fn client_and_name(stem: &str) -> (Option<String>, String) {
    match stem.split_once('-') {
        Some((c, n)) if !c.is_empty() && !n.is_empty() => (Some(c.to_string()), n.to_string()),
        _ => (None, stem.to_string()),
    }
}

/// Converte um `creds/*.txt` (o `stem` é o nome do arquivo sem `.txt`).
pub fn cred_target(stem: &str, f: &CredFile) -> CredTarget {
    use zerith_tools::network::{DeviceEntry, DeviceRole, DeviceTransport, Vendor};
    let (client, name) = client_and_name(stem);
    let base_ref = match &client {
        Some(c) => format!("{c}-{name}"),
        None => name.clone(),
    };
    let hosts: Vec<String> = f
        .get("host")
        .or(f.get("hosts"))
        .unwrap_or("")
        .split([',', ' ', ';'])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .collect();
    let Some(host) = hosts.first().cloned() else {
        let mut lines: Vec<String> = f.fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        lines.extend(f.loose.iter().cloned());
        return CredTarget::Secret {
            name: base_ref,
            value: lines.join(" | "),
        };
    };
    let get = |k: &str| f.get(k).unwrap_or("");
    let hay = format!(
        "{stem} {} {} {} {}",
        get("plataforma"),
        get("observacao"),
        get("protocolo"),
        f.comments.join(" ")
    )
    .to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| hay.contains(x));
    let vendor = if has(&["mikrotik", "routeros"]) || name.starts_with("mk-") {
        Vendor::MikrotikRouterOs
    } else if has(&["ne20", "huawei", "vrp"]) {
        Vendor::HuaweiVrp
    } else if has(&["nexus", "nx-os", "nxos"]) {
        Vendor::CiscoNxos
    } else if has(&["airos", "ubiquiti"]) {
        Vendor::Linux
    } else {
        Vendor::Generic
    };
    let role = if has(&["borda", "ne20", "concentrador"]) {
        DeviceRole::Border
    } else if has(&["swcore", "sw-core"]) {
        DeviceRole::Core
    } else {
        DeviceRole::Access
    };
    let proto = get("protocolo").to_lowercase();
    let transport = if proto.contains("telnet") || proto.contains("tl1") {
        DeviceTransport::Telnet
    } else {
        DeviceTransport::Ssh
    };
    let port = get("port").parse().unwrap_or(match transport {
        DeviceTransport::Telnet => 23,
        _ => 22,
    });
    let mut tags: Vec<String> = client.iter().cloned().collect();
    if get("observacao").to_lowercase().contains("produ") {
        tags.push("producao".into());
    }
    for t in ["datacom", "parks", "fiberhome", "olt"] {
        if hay.contains(t) {
            tags.push(t.into());
        }
    }

    let mut secrets: Vec<(String, String)> = Vec::new();
    if let Some(p) = f.get("password") {
        secrets.push((base_ref.clone(), p.to_string()));
    }
    let mut desc_parts: Vec<String> = f.comments.clone();
    if hosts.len() > 1 {
        desc_parts.push(format!("outros hosts: {}", hosts[1..].join(", ")));
    }
    for k in ["observacao", "plataforma", "protocolo"] {
        desc_parts.extend(f.get(k).map(str::to_string));
    }
    if let Some(v) = f.get("acesso_alternativo") {
        desc_parts.push(format!("acesso alternativo: {v}"));
    }
    if let Some(cl) = f.get("cli_login") {
        let pw = f.get("cli_password").map(|v| {
            secrets.push((format!("{base_ref}-cli"), v.to_string()));
            format!(", senha no cofre: {base_ref}-cli")
        });
        desc_parts.push(format!("CLI: usuário {cl}{}", pw.unwrap_or_default()));
    }
    // Pares `(usuario/senha)` e `senha: x` no texto livre: `<ref>-altN`.
    let (mut desc, found) = extract_values(&desc_parts.join(" | "), "tmp");
    for (n, e) in found.into_iter().enumerate() {
        let alt = format!("{base_ref}-alt{}", n + 1);
        desc = desc.replace(
            &format!("valor no cofre ({})", e.name),
            &format!("senha no cofre: {alt}"),
        );
        secrets.push((alt, e.value));
    }
    let dev = DeviceEntry {
        name,
        client,
        host,
        port,
        user: get("login").to_string(),
        vendor,
        role,
        transport,
        secret_ref: f.get("password").map(|_| base_ref.clone()),
        legacy_ssh: hay.contains("ssh-rsa"),
        tags,
        description: (!desc.is_empty()).then_some(desc),
        ..Default::default()
    };
    CredTarget::Device {
        dev: Box::new(dev),
        secrets,
    }
}

fn credentials(ctx: &mut Ctx) {
    let inv_path = ctx.paths.root().join("inventory.toml");
    let mut dirs = vec![ctx.home.join("creds")];
    dirs.extend(ctx.opts.extra_creds.iter().cloned());
    let mut seen_dirs = HashSet::new();
    dirs.retain(|d| seen_dirs.insert(d.canonicalize().unwrap_or_else(|_| d.clone())));
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")))
            .collect();
        files.sort();
        for path in files {
            credential(ctx, &path, &inv_path);
        }
    }
}

fn credential(ctx: &mut Ctx, path: &Path, inv_path: &Path) {
    let src = ctx.rel(path);
    let Ok(raw) = std::fs::read_to_string(path) else {
        ctx.plan
            .push("credencial", src, "", Status::Error, "não foi possível ler");
        return;
    };
    let sha = plan::sha256_hex(raw.as_bytes());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let target = cred_target(&stem, &CredFile::parse(&raw));
    // Pula se o arquivo é o mesmo de antes, a menos que agora os segredos
    // vão para o cofre e da outra vez não foram.
    let ledger_id = if ctx.opts.migrate_secrets {
        "com-cofre"
    } else {
        "sem-cofre"
    };
    let done_before = ctx.ledger.entry(path).is_some_and(|e| {
        e.sha256 == sha && (!ctx.opts.migrate_secrets || e.target_id.ends_with("com-cofre"))
    });
    if done_before {
        ctx.plan.push(
            "credencial",
            src,
            "",
            Status::Skipped,
            "arquivo igual ao da última migração",
        );
        return;
    }
    let (dest, secrets, dev) = match target {
        CredTarget::Secret { name, value } => {
            (format!("cofre ({name})"), vec![(name, value)], None)
        }
        CredTarget::Device { dev, secrets } => (
            format!("inventory.toml ({})", dev.display_qualified_name()),
            secrets,
            Some(*dev),
        ),
    };
    let names: Vec<String> = secrets.iter().map(|(n, _)| n.clone()).collect();
    let mut reason = Vec::new();
    let mut status = Status::Imported;
    // O dispositivo e o cofre são decididos à parte: um dispositivo já no
    // inventário ainda pode ter as senhas levadas ao cofre numa rodada com
    // --migrate-secrets.
    let mut write_dev = dev.is_some();
    let write_vault = ctx.opts.migrate_secrets && !secrets.is_empty();
    if let Some(dev) = &dev {
        let existing = std::fs::read_to_string(inv_path)
            .ok()
            .and_then(|t| zerith_tools::network::parse_inventory(&t).ok())
            .and_then(|l| {
                l.into_iter()
                    .find(|d| d.composite_key() == dev.composite_key())
            });
        match existing {
            Some(e) if &e == dev => {
                write_dev = false;
                reason.push("já no inventário".to_string());
            }
            Some(_) if !ctx.opts.overwrite => {
                write_dev = false;
                status = Status::Conflict;
                reason.push("já existe no inventário com outros dados; use --overwrite".into());
            }
            Some(_) => reason.push("substitui o dispositivo do inventário".into()),
            None => {}
        }
    }
    if !names.is_empty() {
        if ctx.opts.migrate_secrets {
            reason.push(format!("senha(s) no cofre: {}", names.join(", ")));
        } else {
            reason.push(format!(
                "senha(s) fora: re-adicione com zerith secret set {}",
                names.join(" / ")
            ));
            for n in &names {
                ctx.plan.strip(format!("credencial:{n}"));
            }
        }
    }
    if status == Status::Imported && !write_dev && !write_vault {
        // Já no inventário, ou arquivo só de segredos sem a flag.
        status = Status::Skipped;
    }
    if ctx.apply() && status == Status::Imported {
        let vault_err = match ctx.secrets {
            Some(vault) if write_vault => secrets
                .iter()
                .try_for_each(|(n, v)| vault.set(n, v))
                .err()
                .map(|e| e.to_string()),
            _ => None,
        };
        let inv_err = match (&vault_err, dev) {
            (None, Some(d)) if write_dev => {
                zerith_tools::network::upsert_inventory_device(inv_path, d).err()
            }
            _ => None,
        };
        if let Some(e) = vault_err.or(inv_err) {
            ctx.plan.push("credencial", src, dest, Status::Error, e);
            return;
        }
    }
    if ctx.apply() && status != Status::Conflict {
        ctx.ledger
            .record(path, &sha, &format!("{dest} {ledger_id}"));
    }
    ctx.plan
        .push("credencial", src, dest, status, reason.join("; "));
}

// ------------------------------------------------------------------- skills

/// De onde veio uma skill do home do Hermes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Sem registro de origem: criada pela pessoa. Importada por padrão.
    User,
    /// Embutida (ou do hub) e alterada pela pessoa. Só com
    /// `customized_skills`.
    Customized,
    /// Embutida e igual à original: o Zerith tem as suas.
    Bundled,
}

/// Arquivos de uma skill que contam para o hash, como o Hermes faz
/// (`tools/skills_sync.py::_dir_hash`): pula `__pycache__` e afins e o `.pyc`
/// que tem `.py` ao lado.
fn skill_files(dir: &Path) -> Vec<(String, PathBuf)> {
    const CACHE: [&str; 4] = ["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache"];
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !CACHE.contains(&name.as_str()) {
                    walk(root, &p, out);
                }
                continue;
            }
            let pyc = p.extension().is_some_and(|x| x == "pyc" || x == "pyo")
                && p.with_extension("py").is_file();
            if !pyc {
                let rel = p.strip_prefix(root).unwrap_or(&p);
                out.push((rel.to_string_lossy().replace('\\', "/"), p.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// Os hashes md5 que o `.bundled_manifest` pode ter para esta pasta: o
/// caminho relativo como o Python escreve no Linux/macOS (`a/b`, ordem
/// binária) e no Windows (`a\b`, ordem sem caixa).
pub fn bundled_hashes(dir: &Path) -> [String; 2] {
    use md5::{Digest, Md5};
    let mut files = skill_files(dir);
    let digest = |files: &[(String, PathBuf)], sep: &str| {
        let mut h = Md5::new();
        for (rel, p) in files {
            h.update(rel.replace('/', sep).as_bytes());
            h.update(std::fs::read(p).unwrap_or_default());
        }
        plan::hex(&h.finalize())
    };
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let posix = digest(&files, "/");
    files.sort_by_key(|a| a.0.to_lowercase());
    [posix, digest(&files, "\\")]
}

/// O `content_hash` do `skills/.hub/lock.json` (`tools/skills_guard.py`):
/// `sha256:` + 16 hex de sha256 sobre (caminho posix, `\0`, bytes) em ordem
/// de string.
pub fn hub_hash(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut files = skill_files(dir);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel, p) in &files {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(std::fs::read(p).unwrap_or_default());
    }
    format!("sha256:{}", &plan::hex(&h.finalize())[..16])
}

/// Classifica cada skill do home pela origem: `.bundled_manifest`
/// (`nome:md5`), `.hub/lock.json` (`installed.<nome>.content_hash`) e o que
/// existe em `hermes-agent/skills` e `hermes-agent/optional-skills`.
pub fn classify_skills(home: &Path) -> Vec<(crate::hermes_setup::HermesSkill, SkillOrigin)> {
    let skills_dir = home.join("skills");
    let manifest: std::collections::HashMap<String, String> =
        std::fs::read_to_string(skills_dir.join(".bundled_manifest"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (n, h) = l.trim().split_once(':').unwrap_or((l.trim(), ""));
                (!n.is_empty()).then(|| (n.trim().to_string(), h.trim().to_string()))
            })
            .collect();
    let hub: serde_json::Value = std::fs::read_to_string(skills_dir.join(".hub/lock.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let shipped: std::collections::HashMap<String, PathBuf> = ["skills", "optional-skills"]
        .iter()
        .flat_map(|d| crate::hermes_setup::scan_skills(&home.join("hermes-agent").join(d)))
        .map(|s| (s.name, s.dir))
        .collect();
    let ecc = ecc_files(home);
    crate::hermes_setup::scan_skills(&skills_dir)
        .into_iter()
        .map(|s| {
            let origin = if let Some(h) = manifest.get(&s.name) {
                // v1 do manifesto (sem hash): não dá para provar que é igual.
                if !h.is_empty() && bundled_hashes(&s.dir).contains(h) {
                    SkillOrigin::Bundled
                } else {
                    SkillOrigin::Customized
                }
            } else if let Some(h) = hub["installed"][&s.name]["content_hash"].as_str() {
                if hub_hash(&s.dir) == h {
                    SkillOrigin::Bundled
                } else {
                    SkillOrigin::Customized
                }
            } else if let Some(orig) = shipped.get(&s.name) {
                if hub_hash(&s.dir) == hub_hash(orig) {
                    SkillOrigin::Bundled
                } else {
                    SkillOrigin::Customized
                }
            } else if let Some(changed) = ecc_changed(&ecc, &s.dir) {
                if changed {
                    SkillOrigin::Customized
                } else {
                    SkillOrigin::Bundled
                }
            } else {
                SkillOrigin::User
            };
            (s, origin)
        })
        .collect()
}

/// Arquivos que o pacote ECC (Everything Claude Code) instalou no home:
/// `ecc-install-state.json`, `operations[].destinationPath` → `contentSha256`.
/// O caminho é o do computador onde o ECC rodou; casa pelo trecho a partir
/// de `skills/`.
fn ecc_files(home: &Path) -> std::collections::HashMap<String, String> {
    let v: serde_json::Value = std::fs::read_to_string(home.join("ecc-install-state.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    v["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|op| {
            let dest = op["destinationPath"].as_str()?.replace('\\', "/");
            let rel = &dest[dest.find("/skills/")? + 1..];
            Some((rel.to_string(), op["contentSha256"].as_str()?.to_string()))
        })
        .collect()
}

/// `Some(alterada?)` se a pasta foi instalada pelo ECC; `None` se não.
fn ecc_changed(ecc: &std::collections::HashMap<String, String>, dir: &Path) -> Option<bool> {
    let comps: Vec<String> = dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let at = comps.iter().rposition(|c| c == "skills")?;
    let prefix = format!("{}/", comps[at..].join("/"));
    let mine: Vec<(&String, &String)> =
        ecc.iter().filter(|(k, _)| k.starts_with(&prefix)).collect();
    if mine.is_empty() {
        return None;
    }
    let root = dir.ancestors().nth(comps.len() - at)?;
    Some(mine.iter().any(|(rel, sha)| {
        std::fs::read(root.join(rel)).map_or(true, |b| &plan::sha256_hex(&b) != *sha)
    }))
}

fn skills(ctx: &mut Ctx) {
    let installed: HashSet<String> =
        std::fs::read_to_string(ctx.paths.skills().join("manifest.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<zerith_skills::install::Manifest>(&t).ok())
            .map(|m| m.skills.into_iter().map(|s| s.name).collect())
            .unwrap_or_default();
    let lib = if ctx.apply() {
        match zerith_skills::Library::open(ctx.paths.skills()) {
            Ok(l) => Some(l),
            Err(e) => {
                ctx.plan
                    .push("skill", "", "skills/", Status::Error, e.to_string());
                return;
            }
        }
    } else {
        None
    };
    let mut done: HashSet<String> = HashSet::new();
    for (s, origin) in classify_skills(ctx.home) {
        let src = if s.category.is_empty() {
            s.name.clone()
        } else {
            format!("{}/{}", s.category, s.name)
        };
        let dest = format!("skills/{}", s.name);
        let skip = |why: &str| (Status::Skipped, why.to_string());
        let (status, reason) = if let Some(e) = &s.error {
            (Status::Error, format!("SKILL.md inválido: {e}"))
        } else if installed.contains(&s.name) || done.contains(&s.name) {
            skip("já existe no Zerith")
        } else {
            match origin {
                SkillOrigin::Bundled => skip("embutida no Hermes e sem alteração"),
                SkillOrigin::Customized if !ctx.opts.customized_skills => {
                    skip("embutida no Hermes, mas alterada; importe com --customized-skills")
                }
                SkillOrigin::Customized => (Status::Imported, "embutida e alterada".into()),
                SkillOrigin::User => (Status::Imported, "criada no Hermes".into()),
            }
        };
        let (status, reason) = match (&lib, status) {
            (Some(lib), Status::Imported) => {
                match lib.install(&s.dir, &format!("hermes:{}", s.dir.display())) {
                    Ok(_) => (status, reason),
                    Err(zerith_skills::install::InstallError::NameConflict(_)) => {
                        skip("já existe no Zerith")
                    }
                    Err(e) => (Status::Error, e.to_string()),
                }
            }
            _ => (status, reason),
        };
        if status == Status::Imported {
            done.insert(s.name.clone());
        }
        ctx.plan.push("skill", src, dest, status, reason);
    }
}

// ---------------------------------------------------------------------- mcp

fn mcp(ctx: &mut Ctx) {
    use crate::hermes_setup::{hermes_mcp, import_hermes_mcp_with, McpStatus};
    if !ctx.home.join("config.yaml").is_file() {
        return;
    }
    let found = match hermes_mcp(ctx.home, ctx.paths) {
        Ok(f) => f,
        Err(e) => {
            ctx.plan
                .push("mcp", "config.yaml", "mcp.toml", Status::Error, e);
            return;
        }
    };
    let keep = ctx.opts.migrate_secrets;
    let mut importable = Vec::new();
    for (m, st) in &found {
        let gone: Vec<String> = plan::strip_secrets(&m.env)
            .1
            .into_iter()
            .map(|k| format!("env.{k}"))
            .chain(
                plan::strip_secrets(&m.headers)
                    .1
                    .into_iter()
                    .map(|k| format!("headers.{k}")),
            )
            .collect();
        let (status, mut reason) = match st {
            McpStatus::Exists => (Status::Skipped, "já existe no Zerith".to_string()),
            McpStatus::NotFound => (
                Status::Error,
                "comando não encontrado neste computador".to_string(),
            ),
            McpStatus::Invalid => (
                Status::Error,
                "precisa de `command` ou `url` (http/https)".to_string(),
            ),
            McpStatus::OAuth => (
                Status::Imported,
                "OAuth: entre depois com zerith mcp login".to_string(),
            ),
            McpStatus::Importable => (Status::Imported, String::new()),
        };
        if status == Status::Imported {
            importable.push(m.name.clone());
            if !gone.is_empty() {
                let what = if keep {
                    "copiado(s) para mcp.toml (--migrate-secrets)"
                } else {
                    "deixado(s) de fora"
                };
                let sep = if reason.is_empty() { "" } else { "; " };
                reason = format!("{reason}{sep}{} {what}", gone.join(", "));
                if !keep {
                    for g in &gone {
                        ctx.plan.strip(format!("mcp:{}.{g}", m.name));
                    }
                }
            }
        }
        ctx.plan.push(
            "mcp",
            format!("config.yaml: {}", m.name),
            format!("mcp.toml ({})", m.name),
            status,
            reason,
        );
    }
    if !ctx.apply() || importable.is_empty() {
        return;
    }
    let failed = match import_hermes_mcp_with(ctx.home, ctx.paths, &importable, keep) {
        Ok((r, _)) => r.failed,
        Err(e) => importable.iter().map(|n| format!("{n}: {e}")).collect(),
    };
    for f in failed {
        let name = f.split(':').next().unwrap_or("").to_string();
        if let Some(i) = ctx
            .plan
            .items
            .iter_mut()
            .find(|i| i.kind == "mcp" && i.dest == format!("mcp.toml ({name})"))
        {
            i.status = Status::Error;
            i.reason = f;
        }
    }
}

// --------------------------------------------------------------------- cron

/// `jobs.json` do Hermes: `{"jobs": [...]}`, uma lista solta ou um mapa por id.
fn cron_jobs(text: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let jobs = if v.is_object() && v.get("jobs").is_some() {
        v["jobs"].clone()
    } else {
        v
    };
    match jobs {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(m) => m
            .into_iter()
            .map(|(k, mut j)| {
                if j.get("id").is_none() {
                    j["id"] = k.into();
                }
                j
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Expressão cron (UTC, como o daemon do Zerith avalia) equivalente ao
/// agendamento do Hermes, ou o motivo de não haver.
///
/// O Hermes avalia cron no fuso dele; aqui as horas são deslocadas por
/// `offset_min` (minutos a leste de UTC). Só quando é claro: horas numéricas,
/// deslocamento em horas cheias e, se a hora muda de dia, dia livre.
pub fn cron_to_utc(schedule: &serde_json::Value, offset_min: i32) -> Result<String, String> {
    let kind = schedule["kind"].as_str().unwrap_or("");
    match kind {
        "interval" => {
            let m = schedule["minutes"].as_i64().unwrap_or(0);
            match m {
                1..=59 if 60 % m == 0 => Ok(format!("*/{m} * * * *")),
                60 => Ok("0 * * * *".into()),
                _ if m > 0 && m % 60 == 0 && 24 % (m / 60) == 0 => {
                    Ok(format!("0 */{} * * *", m / 60))
                }
                _ => Err(format!("intervalo de {m} min não cabe em cron")),
            }
        }
        "cron" => {
            let expr = schedule["expr"].as_str().unwrap_or("");
            let f: Vec<&str> = expr.split_whitespace().collect();
            if f.len() != 5 {
                return Err(format!("cron com {} campos", f.len()));
            }
            if f[1] == "*" || f[1].starts_with("*/") || offset_min == 0 {
                return Ok(f.join(" "));
            }
            if offset_min % 60 != 0 {
                return Err("fuso com fração de hora".into());
            }
            let shift = offset_min / 60;
            let mut crosses = false;
            let mut hours = Vec::new();
            for h in f[1].split(',') {
                let h: i32 = h.parse().map_err(|_| format!("hora `{}`", f[1]))?;
                let u = h - shift;
                crosses |= !(0..24).contains(&u);
                hours.push(u.rem_euclid(24).to_string());
            }
            if crosses && (f[2] != "*" || f[4] != "*") {
                return Err("a hora em UTC muda de dia e o dia está restrito".into());
            }
            Ok(format!(
                "{} {} {} {} {}",
                f[0],
                hours.join(","),
                f[2],
                f[3],
                f[4]
            ))
        }
        "once" => Err("execução única (once)".into()),
        other => Err(format!("tipo de agendamento `{other}`")),
    }
}

/// Fuso local em minutos a leste de UTC agora.
fn local_offset_min() -> i32 {
    zerith_workflows::local_offset_minutes(zerith_workflows::unix_secs())
}

fn cron(ctx: &mut Ctx) {
    let offset = local_offset_min();
    let existing: HashSet<String> = ctx
        .read_db()
        .and_then(|c| {
            let mut st = c.prepare("SELECT name FROM schedules").ok()?;
            let rows = st.query_map([], |r| r.get::<_, String>(0)).ok()?;
            Some(rows.flatten().collect())
        })
        .unwrap_or_default();
    for (profile, dir) in profile_dirs(ctx.home, ctx.opts.profile.as_deref()) {
        let path = dir.join("cron").join("jobs.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for job in cron_jobs(&text) {
            let id = job["id"].as_str().unwrap_or("").to_string();
            let label = job["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&id)
                .to_string();
            let name = format!("hermes-{profile}-{label}");
            let src = format!("{profile}/cron/jobs.json: {label}");
            if existing.contains(&name) {
                ctx.plan.push(
                    "cron",
                    src,
                    format!("agendamento {name}"),
                    Status::Skipped,
                    "já existe",
                );
                continue;
            }
            let prompt = job["prompt"].as_str().unwrap_or("").trim().to_string();
            let scripted = job["script"].as_str().is_some_and(|s| !s.is_empty())
                || job["no_agent"].as_bool() == Some(true)
                || job["monitor_script"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty());
            let expr = cron_to_utc(&job["schedule"], offset);
            let why_not = match (&expr, prompt.is_empty(), scripted) {
                (Err(e), _, _) => Some(e.clone()),
                (_, true, _) => Some("sem prompt".into()),
                (_, _, true) => Some("usa script do Hermes".into()),
                _ => None,
            };
            if let Some(why) = why_not {
                let rel = format!("cron/{profile}-{}.json", safe_name(&label));
                let key = path.join(&id);
                let sha = plan::sha256_hex(job.to_string().as_bytes());
                if ctx.ledger.unchanged(&key, &sha) {
                    ctx.plan.push(
                        "cron",
                        src,
                        ctx.archive_path(&rel),
                        Status::Skipped,
                        "já arquivado",
                    );
                    continue;
                }
                if ctx.apply() {
                    ctx.ledger.record(&key, &sha, &rel);
                    let body = serde_json::json!({
                        "instrucao": format!("Sem equivalente direto ({why}). Recrie com `zerith schedule add` ou um workflow."),
                        "job": job,
                    });
                    let _ = ctx.archive(
                        &rel,
                        serde_json::to_string_pretty(&body)
                            .unwrap_or_default()
                            .as_bytes(),
                    );
                }
                ctx.plan.push(
                    "cron",
                    src,
                    ctx.archive_path(&rel),
                    Status::Archived,
                    format!("sem equivalente direto: {why}; recrie com zerith schedule add"),
                );
                continue;
            }
            let expr = expr.unwrap_or_default();
            let mut notes = vec![format!(
                "cron `{expr}` (UTC); entra desligado: ative com zerith schedule enable {name} depois de desligar no Hermes"
            )];
            if let Some(d) = job["deliver"]
                .as_str()
                .filter(|d| !matches!(*d, "" | "local" | "origin"))
            {
                notes.push(format!("entrega `{d}` não migrada"));
            }
            if ctx.apply() {
                let n = zerith_sessions::NewSchedule {
                    name: name.clone(),
                    cron_expr: expr,
                    workspace: job["workdir"].as_str().unwrap_or("").to_string(),
                    prompt: Some(prompt),
                    command: None,
                    no_agent: false,
                    model: job["model"].as_str().map(str::to_string),
                    enabled: false,
                };
                let r = Store::open(ctx.paths.db())
                    .map_err(|e| e.to_string())
                    .and_then(|s| s.schedule_add(&n).map_err(|e| e.to_string()));
                if let Err(e) = r {
                    ctx.plan
                        .push("cron", src, format!("agendamento {name}"), Status::Error, e);
                    continue;
                }
            }
            ctx.plan.push(
                "cron",
                src,
                format!("agendamento {name}"),
                Status::Imported,
                notes.join("; "),
            );
        }
    }
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ----------------------------------------------------------------- telegram

/// O nome no cofre que o gateway do Zerith lê (`zerith-cli/src/telegram.rs`,
/// `TELEGRAM_SECRET_KEY`).
pub const TELEGRAM_SECRET: &str = "telegram_bot_token";

/// `CHAVE=valor` de um `.env`, sem comentários nem aspas.
fn dotenv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            Some((k.trim().to_string(), v.to_string()))
        })
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

fn telegram(ctx: &mut Ctx) {
    let env = dotenv(&std::fs::read_to_string(ctx.home.join(".env")).unwrap_or_default());
    let var = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let yaml = std::fs::read_to_string(ctx.home.join("config.yaml")).unwrap_or_default();

    // Token: só com --migrate-secrets.
    if let Some(token) = var("TELEGRAM_BOT_TOKEN") {
        let src = ".env: TELEGRAM_BOT_TOKEN";
        let dest = format!("cofre ({TELEGRAM_SECRET})");
        if !ctx.opts.migrate_secrets {
            ctx.plan.strip(format!("telegram:{TELEGRAM_SECRET}"));
            ctx.plan.push(
                "telegram",
                src,
                dest,
                Status::Skipped,
                "token fora: rode com --migrate-secrets ou use zerith gateway telegram setup",
            );
        } else {
            let (status, reason) = match ctx.secrets.map(|v| v.get(TELEGRAM_SECRET)) {
                Some(Ok(cur)) if cur == token => (Status::Skipped, "já no cofre".to_string()),
                Some(Ok(_)) if !ctx.opts.overwrite => (
                    Status::Conflict,
                    "o cofre já tem outro token; use --overwrite".to_string(),
                ),
                _ if !ctx.apply() => (Status::Imported, String::new()),
                _ => match ctx.secrets.map(|v| v.set(TELEGRAM_SECRET, &token)) {
                    Some(Err(e)) => (Status::Error, e.to_string()),
                    _ => (Status::Imported, String::new()),
                },
            };
            ctx.plan.push("telegram", src, dest, status, reason);
        }
    }

    // Allowlist: TELEGRAM_ALLOWED_USERS e telegram.allowed_chats.
    let mut chats: Vec<i64> = var("TELEGRAM_ALLOWED_USERS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_string)
        .chain(crate::hermes_setup::telegram_allowed_chats(&yaml))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    chats.sort_unstable();
    chats.dedup();
    if !chats.is_empty() {
        let have = zerith_core::Settings::load(ctx.paths, None)
            .settings
            .gateway
            .telegram
            .allowed_chats;
        let new: Vec<i64> = chats
            .iter()
            .copied()
            .filter(|c| !have.contains(c))
            .collect();
        let dest = "settings.toml (gateway.telegram.allowed_chats)";
        let src = "allowlist do Telegram";
        if new.is_empty() {
            ctx.plan
                .push("telegram", src, dest, Status::Skipped, "chats já liberados");
        } else {
            let mut status = Status::Imported;
            let mut reason = format!("{} chat(s) liberado(s)", new.len());
            if ctx.apply() {
                if let Err(e) = zerith_core::Settings::save_user(ctx.paths, |s| {
                    s.gateway.telegram.allowed_chats.extend(&new);
                }) {
                    status = Status::Error;
                    reason = e.to_string();
                }
            }
            ctx.plan.push("telegram", src, dest, status, reason);
        }
    }

    // O resto (canal padrão, webhook, tópico do cron): só os nomes.
    let rest: Vec<&str> = env
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| {
            k.starts_with("TELEGRAM_")
                && !matches!(*k, "TELEGRAM_BOT_TOKEN" | "TELEGRAM_ALLOWED_USERS")
        })
        .collect();
    if !rest.is_empty() {
        let names = rest.join(", ");
        let rel = "telegram/LEIA.md";
        let sha = plan::sha256_hex(names.as_bytes());
        let key = ctx.home.join(".env#telegram");
        if ctx.ledger.unchanged(&key, &sha) {
            ctx.plan.push(
                "telegram",
                ".env",
                ctx.archive_path(rel),
                Status::Skipped,
                "já arquivado",
            );
            return;
        }
        if ctx.apply() {
            let text = format!(
                "# Telegram do Hermes\n\nVariáveis sem equivalente no gateway do Zerith (valores não copiados): {names}.\n\nConfigure com `zerith gateway telegram setup`.\n"
            );
            if ctx.archive(rel, text.as_bytes()).is_ok() {
                ctx.ledger.record(&key, &sha, rel);
            }
        }
        ctx.plan.push(
            "telegram",
            ".env",
            ctx.archive_path(rel),
            Status::Archived,
            format!("sem equivalente: {names}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_splits_on_the_section_sign() {
        assert_eq!(
            memory_entries("a\nb\n§\nc\n  §  \n\n§\nd"),
            ["a\nb", "c", "d"]
        );
    }

    #[test]
    fn secret_values_leave_the_text_and_forbidden_words_go() {
        let cases = [
            "core: SNMPv2c Ab12cd=WRITE em 10.0.0.1",
            "a senha do roteador é Qq77!x",
            "password: hunter22",
            "acesso alternativo (admin/Zx9!pp)",
            "community s3gr3d0 no switch",
        ];
        for c in cases {
            let (text, found) = extract_secrets(c, "t");
            assert_eq!(found.len(), 1, "{c} -> {text}");
            assert!(!text.contains(&found[0].value), "{c} -> {text}");
            assert!(text.contains(&format!("valor no cofre ({})", found[0].name)));
            assert!(!zerith_security::mentions_secret(&text), "{c} -> {text}");
        }
        let (text, found) = extract_secrets("SNMPv2c Ab12cd=WRITE", "t");
        assert!(text.ends_with("=WRITE"), "{text}");
        // Mesmo valor, mesmo nome (rodar de novo não cria outro).
        assert_eq!(found, extract_secrets("SNMPv2c Ab12cd=WRITE", "t").1);
        // Prosa sem valor fica como está, só sem as palavras proibidas.
        let (text, found) = extract_secrets("prefere senha forte e TCP/UDP", "t");
        assert!(found.is_empty());
        assert_eq!(text, "prefere credencial forte e TCP/UDP");
    }

    #[test]
    fn generic_agents_guides_are_recognized_and_blocks_are_idempotent() {
        assert!(generic_agents_md("# Everything Claude Code (ECC) — x").is_some());
        assert!(generic_agents_md("# Minhas regras\nusar pt-BR\n").is_none());
        let (doc, old) = upsert_block("# Eu\n", "x", "corpo");
        assert!(old.is_none());
        let (again, old) = upsert_block(&doc, "x", "corpo");
        assert_eq!(again, doc);
        assert_eq!(old.as_deref(), Some("corpo"));
        let (changed, _) = upsert_block(&doc, "x", "novo");
        assert!(
            changed.starts_with("# Eu\n") && changed.contains("novo") && !changed.contains("corpo")
        );
    }
}
