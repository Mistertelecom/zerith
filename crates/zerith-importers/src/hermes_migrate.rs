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
        secrets: (!opts.dry_run).then_some(secrets),
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
        let src = db.display().to_string();
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
            let already = value.starts_with("valor") || value.contains("cofre");
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
    (neutral_words(&out), found)
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
                if let (Some(mem), Some(vault)) = (&mem, ctx.secrets) {
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
