//! Base comum das migrações de outros agentes: plano, relatório, backup,
//! ledger de idempotência, segredos fora por padrão e troca de marca.
//!
//! O fluxo é sempre o mesmo, seja qual for a origem:
//!
//! 1. o leitor da origem monta um [`ImportPlan`] **sem escrever nada**
//!    (cada item diz o que aconteceria e por quê);
//! 2. a pessoa vê o plano e confirma;
//! 3. a aplicação faz [`backup`] primeiro, aplica, grava o [`Ledger`] e o
//!    relatório em `<raiz>/migrations/<data-hora>/` ([`write_report`]).
//!
//! Ideias (e nomes de status) vindas do `hermes import-agent` do Hermes
//! (`hermes_cli/agent_import.py`, `backup.py::create_pre_migration_backup`;
//! MIT) e do `external-agent-migration` do Codex (ledger por sha256;
//! Apache-2.0). Código reescrito, nada copiado.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use zerith_core::Paths;

/// O que aconteceu (ou aconteceria, no dry-run) com um item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Imported,
    Skipped,
    /// Já existe algo diferente no destino; só com `--overwrite`.
    Conflict,
    /// Sem equivalente no Zerith: copiado para `archive/` com instrução.
    Archived,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Imported => "imported",
            Status::Skipped => "skipped",
            Status::Conflict => "conflict",
            Status::Archived => "archived",
            Status::Error => "error",
        }
    }
}

/// Uma linha do plano. `reason` nunca carrega valor de segredo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportItem {
    /// `sessao`, `memoria`, `persona`, `credencial`, `skill`, `mcp`, `cron`, `telegram`…
    pub kind: String,
    pub source: String,
    pub dest: String,
    pub status: Status,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportPlan {
    /// `hermes`, `claude-code`, `codex`.
    pub origin: String,
    pub source_root: String,
    pub dry_run: bool,
    pub items: Vec<ImportItem>,
    /// Nomes (nunca valores) de segredos deixados de fora:
    /// `mcp:github.env.GITHUB_TOKEN`, `credencial:cliente-borda`.
    pub stripped_secrets: Vec<String>,
    /// Onde o backup e o relatório foram gravados (só na aplicação).
    pub backup_dir: Option<String>,
    pub report_dir: Option<String>,
}

impl ImportPlan {
    pub fn new(origin: &str, source_root: &Path, dry_run: bool) -> Self {
        Self {
            origin: origin.into(),
            source_root: source_root.display().to_string(),
            dry_run,
            ..Default::default()
        }
    }

    pub fn push(
        &mut self,
        kind: &str,
        source: impl Into<String>,
        dest: impl Into<String>,
        status: Status,
        reason: impl Into<String>,
    ) {
        self.items.push(ImportItem {
            kind: kind.into(),
            source: source.into(),
            dest: dest.into(),
            status,
            reason: reason.into(),
        });
    }

    pub fn strip(&mut self, name: impl Into<String>) {
        let name = name.into();
        if !self.stripped_secrets.contains(&name) {
            self.stripped_secrets.push(name);
        }
    }

    /// Contagem por tipo e status: `{"skill": {"imported": 3, "skipped": 40}}`.
    pub fn summary(&self) -> BTreeMap<String, BTreeMap<&'static str, usize>> {
        let mut out: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
        for i in &self.items {
            *out.entry(i.kind.clone())
                .or_default()
                .entry(i.status.as_str())
                .or_default() += 1;
        }
        out
    }

    /// O resumo em Markdown (o `summary.md` e a saída do terminal).
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Migração de {} {}\n\nOrigem: `{}`\n\n| tipo | importar | pular | conflito | arquivar | erro |\n|---|---|---|---|---|---|\n",
            self.origin,
            if self.dry_run { "(plano, nada gravado)" } else { "(aplicada)" },
            self.source_root
        );
        for (kind, c) in self.summary() {
            let n = |k: &str| c.get(k).copied().unwrap_or(0);
            s.push_str(&format!(
                "| {kind} | {} | {} | {} | {} | {} |\n",
                n("imported"),
                n("skipped"),
                n("conflict"),
                n("archived"),
                n("error")
            ));
        }
        let notable: Vec<&ImportItem> = self
            .items
            .iter()
            .filter(|i| i.status != Status::Skipped)
            .collect();
        if !notable.is_empty() {
            s.push_str("\n## Itens\n\n");
            for i in notable {
                s.push_str(&format!(
                    "- [{}] {} `{}` → {}{}\n",
                    i.status.as_str(),
                    i.kind,
                    i.source,
                    i.dest,
                    if i.reason.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", i.reason)
                    }
                ));
            }
        }
        if !self.stripped_secrets.is_empty() {
            s.push_str("\n## Segredos deixados de fora\n\nRe-adicione com `zerith secret set <nome>` ou rode de novo com `--migrate-secrets`:\n\n");
            for n in &self.stripped_secrets {
                s.push_str(&format!("- {n}\n"));
            }
        }
        if let Some(b) = &self.backup_dir {
            s.push_str(&format!("\nBackup antes da migração: `{b}`\n"));
        }
        s
    }
}

// ------------------------------------------------------------------ tempo

/// `20261124-153000` (UTC): nome de pasta que ordena por data.
pub fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    stamp_of(secs)
}

fn stamp_of(secs: i64) -> String {
    // Howard Hinnant, civil_from_days.
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

// ----------------------------------------------------------------- backup

/// Quantos backups `pre-migracao-*` ficam guardados.
pub const KEEP_BACKUPS: usize = 5;

/// Copia `zerith.db` (por `VACUUM INTO`, consistente mesmo com o app aberto),
/// `settings.toml`, `inventory.toml`, `mcp.toml`, `AGENTS.md` e `skills/` para
/// `<raiz>/backups/pre-migracao-<stamp>/`, e apaga os mais antigos além de
/// [`KEEP_BACKUPS`]. O que não existe é pulado.
pub fn backup(paths: &Paths, stamp: &str) -> std::io::Result<PathBuf> {
    let dir = paths.backups().join(format!("pre-migracao-{stamp}"));
    std::fs::create_dir_all(&dir)?;
    if paths.db().is_file() {
        let conn = rusqlite::Connection::open_with_flags(
            paths.db(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(std::io::Error::other)?;
        let dest = dir.join("zerith.db");
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])
            .map_err(std::io::Error::other)?;
    }
    for f in ["settings.toml", "inventory.toml", "mcp.toml", "AGENTS.md"] {
        let src = paths.root().join(f);
        if src.is_file() {
            std::fs::copy(&src, dir.join(f))?;
        }
    }
    if paths.skills().is_dir() {
        copy_dir(&paths.skills(), &dir.join("skills"))?;
    }
    prune_backups(&paths.backups(), KEEP_BACKUPS)?;
    Ok(dir)
}

fn prune_backups(root: &Path, keep: usize) -> std::io::Result<()> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("pre-migracao-"))
        .map(|e| e.path())
        .collect();
    found.sort();
    let extra = found.len().saturating_sub(keep);
    for old in &found[..extra] {
        std::fs::remove_dir_all(old)?;
    }
    Ok(())
}

pub fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------- relatório

/// `<raiz>/migrations/<stamp>/` com `archive/` e `overflow/` já criadas.
pub fn migration_dir(paths: &Paths, stamp: &str) -> std::io::Result<PathBuf> {
    let dir = paths.root().join("migrations").join(stamp);
    std::fs::create_dir_all(dir.join("archive"))?;
    std::fs::create_dir_all(dir.join("overflow"))?;
    Ok(dir)
}

/// Grava `report.json` e `summary.md` na pasta da migração.
pub fn write_report(dir: &Path, plan: &ImportPlan) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(plan).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("report.json"), json)?;
    std::fs::write(dir.join("summary.md"), plan.markdown())
}

// ------------------------------------------------------------------ ledger

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub source_path: String,
    pub sha256: String,
    pub target_id: String,
    /// Segundos desde a época, do arquivo de origem.
    pub mtime: i64,
}

/// `<raiz>/import-ledger.json`: o que já foi migrado, por arquivo de origem.
/// Hash igual ao da última vez: o item é pulado (rodar duas vezes não
/// duplica nada nem desfaz edições feitas no Zerith depois).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: BTreeMap<String, LedgerEntry>,
}

impl Ledger {
    pub fn path(paths: &Paths) -> PathBuf {
        paths.root().join("import-ledger.json")
    }

    /// Ledger ausente ou ilegível começa vazio (pior caso: reimporta e o
    /// destino deduplica).
    pub fn load(paths: &Paths) -> Self {
        std::fs::read_to_string(Self::path(paths))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) -> std::io::Result<()> {
        let path = Self::path(paths);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }

    /// Mesmo conteúdo já migrado?
    pub fn unchanged(&self, source: &Path, sha: &str) -> bool {
        self.entries
            .get(&key(source))
            .is_some_and(|e| e.sha256 == sha)
    }

    pub fn record(&mut self, source: &Path, sha: &str, target_id: &str) {
        let mtime = std::fs::metadata(source)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.entries.insert(
            key(source),
            LedgerEntry {
                source_path: key(source),
                sha256: sha.into(),
                target_id: target_id.into(),
                mtime,
            },
        );
    }
}

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ----------------------------------------------------------------- segredos

/// Nome de variável/cabeçalho que parece credencial: contém API_KEY, TOKEN,
/// SECRET, PASSWORD, AUTH ou termina em KEY (o critério do Hermes).
pub fn is_secret_key(name: &str) -> bool {
    let up = name.to_ascii_uppercase().replace('-', "_");
    [
        "API_KEY",
        "APIKEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "AUTH",
        "CREDENTIAL",
    ]
    .iter()
    .any(|w| up.contains(w))
        || up.ends_with("KEY")
}

/// Separa o que parece segredo: devolve o mapa sem ele e os nomes removidos.
pub fn strip_secrets(map: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut kept = BTreeMap::new();
    let mut gone = Vec::new();
    for (k, v) in map {
        if is_secret_key(k) {
            gone.push(k.clone());
        } else {
            kept.insert(k.clone(), v.clone());
        }
    }
    (kept, gone)
}

// ------------------------------------------------------------------- marca

/// Troca `from` por `to` (inteira, com a mesma caixa inicial) em texto de
/// persona, sem mexer em URLs, caminhos, `código` ou nomes com `-`/`_`/`.`
/// colados (`hermes-agent`, `~/.hermes`, `HERMES_HOME`).
pub fn rebrand(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push_str(line);
            continue;
        }
        if in_code {
            out.push_str(line);
            continue;
        }
        let mut tick = false;
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String, tick: bool| {
            out.push_str(&swap_word(word, from, to, tick));
            word.clear();
        };
        for c in line.chars() {
            if c.is_whitespace() || c == '`' {
                flush(&mut word, &mut out, tick);
                if c == '`' {
                    tick = !tick;
                }
                out.push(c);
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut out, tick);
    }
    out
}

/// Um "token" separado por espaço: só troca quando é a palavra pura, talvez
/// com pontuação de frase em volta.
fn swap_word(token: &str, from: &str, to: &str, in_tick: bool) -> String {
    if in_tick || token.contains("://") || token.contains('/') || token.contains('\\') {
        return token.to_string();
    }
    let start = token
        .find(|c: char| c.is_alphanumeric())
        .unwrap_or(token.len());
    let end = token
        .char_indices()
        .rfind(|(_, c)| c.is_alphanumeric())
        .map_or(start, |(i, c)| i + c.len_utf8());
    let core = &token[start..end];
    let mut trail = token[end..].chars();
    // `Hermes.` no fim da frase troca; `hermes.db` não.
    let trail_ok = match (trail.next(), trail.next()) {
        (None, _) => true,
        (Some(c), None) => ".,;:!?)\"'".contains(c),
        (Some(c), Some(d)) => ".,;:!?)\"'".contains(c) && ".,;:!?)\"'".contains(d),
    };
    let lead_ok = token[..start].chars().all(|c| "(\"'*_".contains(c));
    let same = core.eq_ignore_ascii_case(from)
        || core
            .strip_suffix("'s")
            .is_some_and(|c| c.eq_ignore_ascii_case(from));
    if !same || !trail_ok || !lead_ok {
        return token.to_string();
    }
    let repl = if core.chars().all(|c| !c.is_lowercase()) && core.len() > 1 {
        to.to_uppercase()
    } else if core.starts_with(|c: char| c.is_uppercase()) {
        to.to_string()
    } else {
        to.to_lowercase()
    };
    let tail = if core.ends_with("'s") { "'s" } else { "" };
    format!("{}{repl}{tail}{}", &token[..start], &token[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_is_utc_and_sortable() {
        assert_eq!(stamp_of(0), "19700101-000000");
        // 2026-11-24 15:30:05 UTC
        assert_eq!(stamp_of(1_795_534_205), "20261124-153005");
    }

    #[test]
    fn secret_names_follow_the_hermes_rule() {
        for k in [
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "Authorization",
            "DB_PASSWORD",
            "x-api-key",
            "MY_SECRET",
        ] {
            assert!(is_secret_key(k), "{k}");
        }
        for k in ["JAVA_HOME", "PATH", "LOG_LEVEL", "REGION"] {
            assert!(!is_secret_key(k), "{k}");
        }
        let m: BTreeMap<String, String> = [("JAVA_HOME", "x"), ("API_KEY", "y")]
            .into_iter()
            .map(|(a, b)| (a.into(), b.into()))
            .collect();
        let (kept, gone) = strip_secrets(&m);
        assert_eq!(kept.keys().collect::<Vec<_>>(), ["JAVA_HOME"]);
        assert_eq!(gone, ["API_KEY"]);
    }

    #[test]
    fn rebrand_leaves_urls_paths_and_code_alone() {
        let t = "Você é o Hermes. HERMES ajuda; hermes's tom.\nVeja https://hermes.dev e ~/.hermes/SOUL.md, `hermes run`, hermes-agent, HERMES_HOME.\n```\nhermes x\n```\n";
        let r = rebrand(t, "Hermes", "Zerith");
        assert_eq!(
            r,
            "Você é o Zerith. ZERITH ajuda; zerith's tom.\nVeja https://hermes.dev e ~/.hermes/SOUL.md, `hermes run`, hermes-agent, HERMES_HOME.\n```\nhermes x\n```\n"
        );
    }

    #[test]
    fn ledger_backup_and_report_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(t.path().join("root"));
        paths.ensure().unwrap();
        // Banco real (com schema), para o VACUUM INTO.
        drop(zerith_sessions::Store::open(paths.db()).unwrap());
        std::fs::write(paths.root().join("settings.toml"), "x = 1\n").unwrap();
        std::fs::create_dir_all(paths.skills().join("a")).unwrap();
        std::fs::write(paths.skills().join("a/SKILL.md"), "s").unwrap();
        for i in 0..7 {
            backup(&paths, &format!("20260101-00000{i}")).unwrap();
        }
        let kept: Vec<String> = std::fs::read_dir(paths.backups())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept.len(), KEEP_BACKUPS);
        assert!(!kept.contains(&"pre-migracao-20260101-000000".to_string()));
        let last = paths.backups().join("pre-migracao-20260101-000006");
        assert!(last.join("zerith.db").is_file());
        assert!(last.join("skills/a/SKILL.md").is_file());
        assert!(last.join("settings.toml").is_file());

        let src = t.path().join("MEMORY.md");
        std::fs::write(&src, "a").unwrap();
        let mut l = Ledger::load(&paths);
        let sha = sha256_hex(b"a");
        assert!(!l.unchanged(&src, &sha));
        l.record(&src, &sha, "memory_facts");
        l.save(&paths).unwrap();
        let l = Ledger::load(&paths);
        assert!(l.unchanged(&src, &sha));
        assert!(!l.unchanged(&src, &sha256_hex(b"b")));

        let mut plan = ImportPlan::new("hermes", t.path(), false);
        plan.push("skill", "x", "y", Status::Imported, "");
        plan.push("skill", "z", "y", Status::Skipped, "embutida");
        plan.strip("mcp:gh.env.GITHUB_TOKEN");
        let dir = migration_dir(&paths, "20260101-000000").unwrap();
        write_report(&dir, &plan).unwrap();
        assert!(dir.join("archive").is_dir() && dir.join("overflow").is_dir());
        let md = std::fs::read_to_string(dir.join("summary.md")).unwrap();
        assert!(md.contains("| skill | 1 | 1 | 0 | 0 | 0 |"));
        assert!(md.contains("mcp:gh.env.GITHUB_TOKEN"));
        let back: ImportPlan =
            serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(back, plan);
    }
}
