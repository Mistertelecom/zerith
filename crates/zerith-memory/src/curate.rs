//! Curadoria da memória: acha duplicatas e quase duplicatas e fatos que não
//! chegam a um prompt há muito tempo, e monta um PLANO (fundir A+B mantendo o
//! texto X; arquivar C, D). Nada muda sem [`apply`], que só o usuário dispara
//! (CLI com confirmação, botão na tela de memória). Fundir = os outros ficam
//! `superseded_by` o que fica; arquivar = `archived_at`. Os dois são
//! reversíveis ([`SqliteMemory::unarchive`], `superseded_by = NULL`); nada é
//! apagado.
//!
//! Ideia do curador do Hermes (`agent/curator.py`: inatividade medida pelo
//! último uso, nunca apagar, só arquivar e recuperável, intervalo de 7 dias;
//! MIT) e da revisão periódica da memória do opencode
//! (`packages/core/src/plugin/home-memory.ts`: "merge duplicates, delete
//! what's stale"; MIT). O código é do Zerith.

use std::collections::HashSet;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use zerith_sessions::migrations::now_ms;

use crate::backend::Result;
use crate::sqlite::SqliteMemory;

/// Sem uso há mais que isso: proposto para arquivar (o marco de 30 dias do Hermes).
pub const ARCHIVE_AFTER_DAYS: i64 = 30;
/// Semelhança de palavras (Jaccard) a partir da qual dois fatos são o mesmo.
pub const SIMILARITY: f64 = 0.75;
const DAY_MS: i64 = 86_400_000;

/// Fundir: `drop` passa a ser substituído por `keep`, cujo texto fica.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Merge {
    pub keep: i64,
    pub text: String,
    pub drop: Vec<(i64, String)>,
}

/// Arquivar um fato sem uso há `idle_days` dias.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    pub id: i64,
    pub text: String,
    pub idle_days: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub merges: Vec<Merge>,
    pub archives: Vec<Archive>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.merges.is_empty() && self.archives.is_empty()
    }

    /// O plano para a pessoa ler, uma linha por ação.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for m in &self.merges {
            let ids: Vec<String> = std::iter::once(m.keep)
                .chain(m.drop.iter().map(|d| d.0))
                .map(|i| format!("#{i}"))
                .collect();
            out.push(format!("fundir {} em: {}", ids.join(" + "), m.text));
            for (id, t) in &m.drop {
                out.push(format!("    #{id} sai: {t}"));
            }
        }
        for a in &self.archives {
            out.push(format!(
                "arquivar #{} (sem uso há {} dias): {}",
                a.id, a.idle_days, a.text
            ));
        }
        out
    }
}

/// O que [`apply`] fez.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub merged: usize,
    pub archived: usize,
}

const STOP: &[&str] = &[
    "a", "o", "as", "os", "um", "uma", "de", "da", "do", "das", "dos", "e", "em", "na", "no",
    "nas", "nos", "que", "com", "para", "por", "se", "ao", "the", "an", "of", "to", "and", "in",
    "is", "for", "on", "with",
];

/// As palavras de um fato, sem caixa, acento nem palavras vazias.
pub fn words(text: &str) -> HashSet<String> {
    let fold = |c: char| match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        c => c,
    };
    text.to_lowercase()
        .chars()
        .map(fold)
        .collect::<String>()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .map(String::from)
        .collect()
}

/// Jaccard entre os conjuntos de palavras.
pub fn similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

struct Row {
    id: i64,
    scope: String,
    text: String,
    updated_at: i64,
    last_used: i64,
}

/// Monta o plano sobre os fatos ativos (não apagados, não substituídos, não
/// arquivados) de todos os escopos. Só funde dentro do mesmo escopo.
// ponytail: comparação de todos os pares por escopo, O(n²); um índice por
// palavra se a memória passar de alguns milhares de fatos. Sem chamada ao
// modelo para propor textos fundidos: fica o texto mais completo do grupo.
pub fn plan(mem: &SqliteMemory, now: i64) -> Result<Plan> {
    let mut st = mem.conn.prepare(
        "SELECT id, scope, text, updated_at, COALESCE(last_used_at, updated_at)
         FROM memory_facts
         WHERE deleted = 0 AND superseded_by IS NULL AND archived_at IS NULL
         ORDER BY scope, id",
    )?;
    let rows: Vec<Row> = st
        .query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                scope: r.get(1)?,
                text: r.get(2)?,
                updated_at: r.get(3)?,
                last_used: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let sets: Vec<HashSet<String>> = rows.iter().map(|r| words(&r.text)).collect();

    // União de grupos: i e j juntos quando são do mesmo escopo e parecidos.
    let mut parent: Vec<usize> = (0..rows.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..rows.len() {
        for j in i + 1..rows.len() {
            if rows[j].scope != rows[i].scope {
                break; // ordenado por escopo
            }
            if similarity(&sets[i], &sets[j]) >= SIMILARITY {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[b] = a;
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..rows.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    let mut out = Plan::default();
    let mut merged: HashSet<i64> = HashSet::new();
    for members in groups.values().filter(|m| m.len() > 1) {
        // Fica o texto mais completo; empate, o mais recente.
        let &keep = members
            .iter()
            .max_by_key(|&&i| (rows[i].text.chars().count(), rows[i].updated_at))
            .expect("grupo não vazio");
        merged.extend(members.iter().map(|&i| rows[i].id));
        out.merges.push(Merge {
            keep: rows[keep].id,
            text: rows[keep].text.clone(),
            drop: members
                .iter()
                .filter(|&&i| i != keep)
                .map(|&i| (rows[i].id, rows[i].text.clone()))
                .collect(),
        });
    }
    let cutoff = now - ARCHIVE_AFTER_DAYS * DAY_MS;
    for r in &rows {
        // Confirmado ou editado há pouco também conta como uso.
        let last = r.last_used.max(r.updated_at);
        if last < cutoff && !merged.contains(&r.id) {
            out.archives.push(Archive {
                id: r.id,
                text: r.text.clone(),
                idle_days: (now - last) / DAY_MS,
            });
        }
    }
    Ok(out)
}

/// Aplica o plano numa transação. Um fato que mudou desde o plano (apagado,
/// substituído, arquivado) é pulado, não ressuscitado.
pub fn apply(mem: &SqliteMemory, plan: &Plan) -> Result<Applied> {
    let tx = mem.conn.unchecked_transaction()?;
    let ts = now_ms();
    let live = "deleted = 0 AND superseded_by IS NULL AND archived_at IS NULL";
    let mut done = Applied::default();
    for m in &plan.merges {
        let keep_live: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM memory_facts WHERE id = ?1 AND {live})"),
            params![m.keep],
            |r| r.get(0),
        )?;
        if !keep_live {
            continue;
        }
        let mut any = false;
        for (id, _) in &m.drop {
            any |= tx.execute(
                &format!(
                    "UPDATE memory_facts SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3 AND id <> ?1 AND {live}"
                ),
                params![m.keep, ts, id],
            )? > 0;
        }
        done.merged += usize::from(any);
    }
    for a in &plan.archives {
        done.archived += tx.execute(
            &format!("UPDATE memory_facts SET archived_at = ?1 WHERE id = ?2 AND {live}"),
            params![ts, a.id],
        )?;
    }
    tx.commit()?;
    Ok(done)
}

impl SqliteMemory {
    /// Desfaz o arquivamento de um fato. `false`: não estava arquivado.
    pub fn unarchive(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE memory_facts SET archived_at = NULL, last_used_at = ?1 WHERE id = ?2 AND archived_at IS NOT NULL",
            params![now_ms(), id],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Confidence, FactKind, MemoryBackend, Scope};

    fn add(mem: &SqliteMemory, scope: &Scope, text: &str) -> i64 {
        mem.remember(scope, FactKind::Fact, text, Confidence::Confirmed, None)
            .unwrap()
    }

    fn age(mem: &SqliteMemory, id: i64, days: i64) {
        let t = now_ms() - days * DAY_MS;
        mem.conn
            .execute(
                "UPDATE memory_facts SET updated_at = ?1, last_used_at = ?1 WHERE id = ?2",
                params![t, id],
            )
            .unwrap();
    }

    #[test]
    fn near_duplicates_in_one_scope_merge_into_the_fullest_text() {
        let mem = SqliteMemory::open_in_memory().unwrap();
        let u = Scope::user("t");
        let a = add(&mem, &u, "O servidor de borda usa BGP com a AS 65001");
        let b = add(&mem, &u, "servidor de borda usa BGP, AS 65001.");
        let c = add(&mem, &u, "prefere respostas curtas");
        // Mesmo texto em outro escopo não funde.
        add(
            &mem,
            &Scope::project("x"),
            "servidor de borda usa BGP AS 65001",
        );
        let p = plan(&mem, now_ms()).unwrap();
        assert_eq!(p.merges.len(), 1, "{:?}", p.lines());
        assert_eq!(p.merges[0].keep, a);
        assert_eq!(
            p.merges[0].drop.iter().map(|d| d.0).collect::<Vec<_>>(),
            [b]
        );
        assert!(p.archives.is_empty());
        assert!(p.lines()[0].starts_with(&format!("fundir #{a} + #{b} em:")));
        assert!(
            similarity(
                &words("Prefere respostas curtas"),
                &words("prefere respostas curtas.")
            ) > 0.99
        );
        assert!(similarity(&words("usa Rust"), &words("usa Go")) < SIMILARITY);

        let done = apply(&mem, &p).unwrap();
        assert_eq!(
            done,
            Applied {
                merged: 1,
                archived: 0
            }
        );
        assert_eq!(mem.get(b).unwrap().superseded_by, Some(a));
        let live: Vec<i64> = mem.list(&u, 10).unwrap().iter().map(|f| f.id).collect();
        assert!(live.contains(&a) && live.contains(&c) && !live.contains(&b));
        // Aplicar de novo não faz nada.
        assert_eq!(apply(&mem, &p).unwrap(), Applied::default());
    }

    #[test]
    fn facts_unused_past_30_days_are_planned_for_archive_and_archiving_is_reversible() {
        let mem = SqliteMemory::open_in_memory().unwrap();
        let u = Scope::user("t");
        let old = add(&mem, &u, "o roteador antigo era um 7200");
        let recent = add(&mem, &u, "a OLT nova fica no rack 3");
        age(&mem, old, 45);
        age(&mem, recent, 10);
        let p = plan(&mem, now_ms()).unwrap();
        assert_eq!(p.archives.len(), 1);
        assert_eq!((p.archives[0].id, p.archives[0].idle_days), (old, 45));
        assert!(p.lines()[0].contains("sem uso há 45 dias"));

        // Usado agora (chegou a um prompt): sai do plano.
        mem.touch(&[old]).unwrap();
        assert!(plan(&mem, now_ms()).unwrap().archives.is_empty());

        age(&mem, old, 45);
        let p = plan(&mem, now_ms()).unwrap();
        assert_eq!(apply(&mem, &p).unwrap().archived, 1);
        // Arquivado: fora de list e recall, mas a linha continua lá.
        assert!(mem.list(&u, 10).unwrap().iter().all(|f| f.id != old));
        assert!(mem.recall(&u, "roteador", 10).unwrap().is_empty());
        assert_eq!(mem.get(old).unwrap().text, "o roteador antigo era um 7200");
        assert!(plan(&mem, now_ms()).unwrap().is_empty());

        assert!(mem.unarchive(old).unwrap());
        assert!(!mem.unarchive(old).unwrap());
        assert_eq!(mem.recall(&u, "roteador", 10).unwrap()[0].fact.id, old);
        // Lembrar o mesmo texto de novo também tira do arquivo.
        age(&mem, old, 45);
        apply(&mem, &plan(&mem, now_ms()).unwrap()).unwrap();
        add(&mem, &u, "o roteador antigo era um 7200");
        assert_eq!(mem.list(&u, 10).unwrap().len(), 2);
    }
}
