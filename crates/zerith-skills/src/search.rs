//! Keyword search over the enabled skills, so a library of hundreds stays
//! findable without listing every one in the prompt.
//!
//! BM25 (k1 = 1.2, b = 0.75) over two fields per skill, scored apart and added
//! (a light BM25F): the metadata the author wrote to say when to use it (name
//! ×3, description, category and tags ×2) counts double the body (headings and
//! the first chars). Scored together, a long body that repeats "ssh" ten times
//! would beat a skill whose name says exactly what was asked. Std only: a few
//! hundred short documents do not need an engine.
//!
//! Words are lowercased, stripped of accents and split on anything that is not
//! a letter or digit (so `mikrotik-routeros-cli` is three words). Each word also
//! matches on a short stem (trailing `s` dropped, first 5 chars), so `routers`
//! finds `router` and `legendas` finds `legenda`, at half weight.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::Library;

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// How much of the body joins the index besides its headings.
const BODY_CHARS: usize = 1500;
const STEM: usize = 5;
/// A stem-only match counts half of an exact one.
const STEM_WEIGHT: f64 = 0.5;
/// The metadata field against the body.
const META_WEIGHT: f64 = 2.0;

/// One ranked skill.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub name: String,
    pub description: String,
    /// `category` from the frontmatter, else the folder the skill came from
    /// (Hermes keeps `networking/mikrotik-routeros-cli`). Empty when neither.
    pub category: String,
    pub score: f64,
    /// Query words that matched: the "why" a person or the model can read.
    pub matched: Vec<String>,
}

#[derive(Default)]
struct Field {
    terms: HashMap<String, f64>,
    len: f64,
}

struct Doc {
    name: String,
    description: String,
    category: String,
    meta: Field,
    body: Field,
}

struct Index {
    docs: Vec<Doc>,
    /// Documents holding a term in either field.
    df: HashMap<String, usize>,
    avg_meta: f64,
    avg_body: f64,
}

/// Indexes by library root, rebuilt when the enabled set changes.
///
/// ponytail: the stamp hashes the manifest rows (name, dir, description,
/// installed_at), so install/remove/enable/update invalidate; editing a
/// SKILL.md by hand without going through the library is seen only after a
/// restart. Entries are never evicted (one per root a process opened); add an
/// LRU if a host ever cycles through many roots.
type Cache = HashMap<PathBuf, (u64, Arc<Index>)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The `limit` enabled skills that best match `query`, best first. Skills
/// that match nothing are left out, so the result may be shorter or empty.
pub fn search(lib: &Library, query: &str, limit: usize) -> Vec<Hit> {
    let Some(index) = index(lib) else {
        return Vec::new();
    };
    index.search(query, limit)
}

/// How many skills the index holds (the enabled ones).
pub fn count(lib: &Library) -> usize {
    index(lib).map_or(0, |i| i.docs.len())
}

fn index(lib: &Library) -> Option<Arc<Index>> {
    let rows = lib.enabled().ok()?;
    let mut h = DefaultHasher::new();
    for r in &rows {
        (&r.name, &r.dir, &r.description, r.installed_at).hash(&mut h);
    }
    let stamp = h.finish();
    let root = lib.root().to_path_buf();
    if let Some((s, idx)) = CACHE
        .lock()
        .ok()?
        .get_or_insert_with(HashMap::new)
        .get(&root)
    {
        if *s == stamp {
            return Some(idx.clone());
        }
    }
    // Built outside the lock: reading 300 bodies must not stall another turn.
    let idx = Arc::new(Index::build(lib, &rows));
    CACHE
        .lock()
        .ok()?
        .get_or_insert_with(HashMap::new)
        .insert(root, (stamp, idx.clone()));
    Some(idx)
}

impl Index {
    fn build(lib: &Library, rows: &[crate::Installed]) -> Self {
        let mut docs = Vec::with_capacity(rows.len());
        for r in rows {
            let text = std::fs::read_to_string(lib.root().join(&r.dir).join("SKILL.md"))
                .unwrap_or_default();
            let (fm, body) = match crate::parse(&text) {
                Ok((fm, body)) => (Some(fm), body),
                Err(_) => (None, ""),
            };
            let mut category = String::new();
            let mut tags = String::new();
            if let Some(fm) = &fm {
                for (k, v) in fm.metadata.iter().chain(&fm.extra) {
                    match k.as_str() {
                        "category" if category.is_empty() => category = v.clone(),
                        "tags" | "keywords" => {
                            tags.push(' ');
                            tags.push_str(v);
                        }
                        _ => {}
                    }
                }
            }
            if category.is_empty() {
                category = folder_category(&r.source);
            }
            let mut meta = Field::default();
            meta.add(&r.name, 3.0);
            meta.add(&r.description, 2.0);
            meta.add(&category, 2.0);
            meta.add(&tags, 2.0);
            let mut text = Field::default();
            let headings: String = body
                .lines()
                .filter(|l| l.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ");
            text.add(&headings.chars().take(BODY_CHARS).collect::<String>(), 1.0);
            text.add(&body.chars().take(BODY_CHARS).collect::<String>(), 1.0);
            docs.push(Doc {
                name: r.name.clone(),
                description: r.description.clone(),
                category,
                meta,
                body: text,
            });
        }
        let mut df: HashMap<String, usize> = HashMap::new();
        for d in &docs {
            let keys: HashSet<&String> = d.meta.terms.keys().chain(d.body.terms.keys()).collect();
            for t in keys {
                *df.entry(t.clone()).or_default() += 1;
            }
        }
        let n = docs.len().max(1) as f64;
        let avg_meta = docs.iter().map(|d| d.meta.len).sum::<f64>() / n;
        let avg_body = docs.iter().map(|d| d.body.len).sum::<f64>() / n;
        Index {
            docs,
            df,
            avg_meta,
            avg_body,
        }
    }

    fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let n = self.docs.len() as f64;
        let idf = |t: &str| {
            let df = *self.df.get(t).unwrap_or(&0) as f64;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        };
        // One entry per query word, with its glossary twins: a concept scores
        // once (its best spelling), so a skill that says "subtitles" and
        // "legenda" is not counted twice for the same idea.
        let concepts: Vec<Vec<(String, String)>> = query_words(query)
            .into_iter()
            .map(|alts| {
                alts.into_iter()
                    .map(|w| {
                        let s = stem_term(&w);
                        (w, s)
                    })
                    .collect()
            })
            .collect();
        let mut hits: Vec<Hit> = Vec::new();
        for d in &self.docs {
            let field = |f: &Field, avg: f64, w: &str, st: &str| {
                let norm = K1 * (1.0 - B + B * f.len / avg.max(1e-9));
                let bm = |t: &str| {
                    f.terms
                        .get(t)
                        .map_or(0.0, |tf| idf(t) * tf * (K1 + 1.0) / (tf + norm))
                };
                bm(w) + STEM_WEIGHT * bm(st)
            };
            let mut score = 0.0;
            let mut matched = Vec::new();
            for alts in &concepts {
                let best = alts
                    .iter()
                    .map(|(w, st)| {
                        let s = META_WEIGHT * field(&d.meta, self.avg_meta, w, st)
                            + field(&d.body, self.avg_body, w, st);
                        (s, w)
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((s, w)) = best.filter(|b| b.0 > 0.0) {
                    score += s;
                    matched.push(w.clone());
                }
            }
            if score > 0.0 {
                hits.push(Hit {
                    name: d.name.clone(),
                    description: d.description.clone(),
                    category: d.category.clone(),
                    score,
                    matched,
                });
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name)));
        hits.truncate(limit);
        hits
    }
}

impl Field {
    fn add(&mut self, text: &str, weight: f64) {
        for t in tokens(text) {
            *self.terms.entry(stem_term(&t)).or_default() += weight;
            *self.terms.entry(t).or_default() += weight;
            self.len += weight;
        }
    }
}

/// Hermes records where a skill came from (`.../skills/networking/<name>`);
/// the parent folder is its category unless it is the skills root itself.
fn folder_category(source: &str) -> String {
    let parts: Vec<&str> = source
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect();
    match parts.len().checked_sub(2).map(|i| parts[i]) {
        Some(p) if p != "skills" && !p.contains(':') => p.to_string(),
        _ => String::new(),
    }
}

/// Lowercase, accent-free words of two or more chars, split on anything that
/// is not a letter or digit.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars().flat_map(char::to_lowercase).map(fold) {
        if c.is_alphanumeric() {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.push(cur);
    out.retain(|t| t.chars().count() >= 2);
    out
}

fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        c => c,
    }
}

/// The light stem: trailing `s` dropped, first 5 chars. `routers` → `route`,
/// `redes` → `rede`, `legendas` → `legen`.
fn stem(t: &str) -> String {
    let t = t.strip_suffix('s').filter(|r| r.len() >= 2).unwrap_or(t);
    t.chars().take(STEM).collect()
}

/// The stem as an index key, apart from exact words.
fn stem_term(t: &str) -> String {
    format!("~{}", stem(t))
}

/// Words that say nothing about which skill fits (English and Portuguese).
const STOP: &[&str] = &[
    "an", "and", "are", "as", "at", "be", "by", "for", "from", "how", "in", "into", "is", "it",
    "of", "on", "or", "the", "this", "to", "use", "used", "using", "when", "with", "you", "your",
    "skill", "skills", "want", "wants", "user", "do", "da", "de", "das", "dos", "em", "no", "na",
    "nos", "nas", "ao", "aos", "um", "uma", "para", "pra", "por", "com", "sem", "que", "como",
    "se", "eu", "me", "meu", "minha", "voce", "numa", "num", "isso", "este", "esta", "essa",
    "esse", "usar", "faca", "preciso", "quero", "certa", "certo", "qual", "oi", "ola", "ok", "so",
    "sim", "nao", "responda", "resposta", "obrigado", "hi", "hello", "thanks", "please", "yes",
];

/// A tiny Portuguese ↔ English glossary, applied to the query both ways, so a
/// Portuguese request reaches English descriptions and the reverse.
///
/// ponytail: ~40 common words, not a translator; the `skill_search` tool tells
/// the model to search in both languages, which is the real fix. Grow the list
/// when a miss shows up in practice.
const GLOSSARY: &[(&str, &str)] = &[
    ("legenda", "subtitles"),
    ("roteador", "router"),
    ("rede", "network"),
    ("comutador", "switch"),
    ("servidor", "server"),
    ("provedor", "isp"),
    ("fibra", "fiber"),
    ("transcrever", "transcribe"),
    ("transcricao", "transcription"),
    ("traduzir", "translate"),
    ("traducao", "translation"),
    ("imagem", "image"),
    ("planilha", "spreadsheet"),
    ("apresentacao", "presentation"),
    ("documento", "document"),
    ("arquivo", "file"),
    ("pasta", "folder"),
    ("senha", "password"),
    ("banco", "database"),
    ("diagrama", "diagram"),
    ("musica", "music"),
    ("jogo", "game"),
    ("navegador", "browser"),
    ("teste", "test"),
    ("depurar", "debug"),
    ("erro", "error"),
    ("implantar", "deploy"),
    ("agendar", "schedule"),
    ("tarefa", "task"),
    ("nota", "note"),
    ("pesquisa", "research"),
    ("buscar", "search"),
    ("codigo", "code"),
    ("revisao", "review"),
    ("anuncio", "ads"),
    ("vendas", "sales"),
    ("preco", "pricing"),
    ("concorrente", "competitor"),
    ("impressora", "printer"),
    ("bateria", "battery"),
    ("energia", "power"),
    ("baixar", "download"),
    ("correio", "email"),
    ("reuniao", "meeting"),
];

/// The query's words, stop words out, each with its glossary twins (the word
/// itself first).
fn query_words(query: &str) -> Vec<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for w in tokens(query) {
        if STOP.contains(&w.as_str()) || !seen.insert(w.clone()) {
            continue;
        }
        // Whole word or its plural; a 5-char stem here would pair
        // "transcribe" with "translate".
        let is = |x: &str| {
            w == x
                || w.strip_suffix('s')
                    .is_some_and(|r| r == x || r.strip_suffix('e') == Some(x))
        };
        let mut alts = vec![w.clone()];
        for (pt, en) in GLOSSARY {
            if is(pt) {
                alts.push(en.to_string());
            } else if is(en) {
                alts.push(pt.to_string());
            }
        }
        // "roteador router" is one idea, not two.
        seen.extend(alts.iter().cloned());
        out.push(alts);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Real names and descriptions from the Hermes library, short bodies.
    const FIXTURE: &[(&str, &str, &str)] = &[
        ("mikrotik-routeros-cli", "Use when accessing a MikroTik RouterOS device via SSH.", "# MikroTik RouterOS over SSH\nLog into a RouterOS device, inspect it, apply config."),
        ("mikrotik-ssh-mkcli", "Consultar/validar MikroTik via SSH com a tool Go mkcli.", "# mkcli\nFerramenta Go para consultas."),
        ("network-gear-ssh", "Use when SSH to legacy network hardware (Huawei, Cisco).", "# Legacy SSH\nOld ciphers and kex."),
        ("huawei-ma5800-olt-ops", "Use when operating a Huawei MA5800 OLT (ONT, VLAN, save).", "# Huawei MA5800\nONT provisioning, service-port, save."),
        ("isp-gpon-olt", "Use when helping with ISP GPON/FTTH networks (OLTs routers).", "# Redes ISP — GPON/FTTH e Roteadores\nOLTs GPON (VSOL, Parks, Huawei)."),
        ("cisco-iosxe-bng-ops", "Use when operating a Cisco ASR/IOS-XE ISP router (BNG).", "# Cisco BNG\nPPPoE sessions."),
        ("video-subtitles", "Use when generating subtitles for video files.", "# Video subtitles\nTranscribe audio, translate, deliver .srt beside each video."),
        ("stash-subtitles-pt-br", "Use when subtitling Stash videos to PT-BR.", "# Legendas PT-BR para a biblioteca Stash\nGemini transcreve, DeepSeek traduz, Whisper sincroniza."),
        ("audio-transcription", "Use when transcribing audio to text: whisper large-v3.", "# Audio transcription\nwhisper large-v3 locally."),
        ("youtube-content", "YouTube transcripts to summaries, threads, blogs.", "# YouTube\nFetch transcript."),
        ("ab-testing", "When the user wants to plan, design, or implement an A/B test or experiment.", "# A/B tests\nHypothesis, sample size."),
        ("minecraft-server-hosting", "Use when hosting a Minecraft Java server on Linux.", "# Minecraft\nJava server, systemd."),
        ("xlsx", "Create, read, edit Excel .xlsx workbooks and CSVs.", "# Excel\nopenpyxl."),
        ("github-pr-workflow", "GitHub PR lifecycle: branch, commit, open, CI, merge.", "# PRs\ngh pr create."),
    ];

    fn fixture() -> (tempfile::TempDir, Library) {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        for (name, desc, body) in FIXTURE {
            add(&lib, tmp.path(), name, desc, body);
        }
        (tmp, lib)
    }

    fn add(lib: &Library, tmp: &std::path::Path, name: &str, desc: &str, body: &str) {
        let src = tmp.join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{desc}\"\n---\n\n{body}\n"),
        )
        .unwrap();
        lib.install(&src, "t").unwrap();
    }

    fn names(lib: &Library, q: &str) -> Vec<String> {
        search(lib, q, 8).into_iter().map(|h| h.name).collect()
    }

    #[test]
    fn words_are_lowercased_unaccented_and_split_on_hyphens() {
        assert_eq!(
            tokens("Configuração do Roteador-MikroTik, legendas!"),
            ["configuracao", "do", "roteador", "mikrotik", "legendas"]
        );
        assert_eq!(stem("routers"), "route");
        assert_eq!(stem("redes"), stem("rede"));
    }

    #[test]
    fn rankings_put_the_right_skill_on_top() {
        let (_t, lib) = fixture();
        assert_eq!(
            names(&lib, "mikrotik routeros ssh")[0],
            "mikrotik-routeros-cli"
        );
        let olt = names(&lib, "olt huawei gpon");
        assert!(olt[..3].iter().any(|n| n.contains("olt")), "{olt:?}");
        assert_eq!(names(&lib, "legenda de vídeo")[0], "video-subtitles");
        assert_eq!(names(&lib, "subtitles")[0], "video-subtitles");
        // Accents and Portuguese: "transcrição de áudio" reaches the English one.
        assert_eq!(
            names(&lib, "transcrição de áudio")[0],
            "audio-transcription"
        );
        // Stems: a plural finds the singular.
        assert!(names(&lib, "roteadores")
            .iter()
            .any(|n| n == "isp-gpon-olt"));
    }

    #[test]
    fn a_hit_says_why_and_nonsense_finds_nothing() {
        let (_t, lib) = fixture();
        let top = &search(&lib, "mikrotik ssh", 3)[0];
        assert_eq!(top.matched, ["mikrotik", "ssh"]);
        assert!(search(&lib, "qwertyuiop", 8).is_empty());
        assert!(search(&lib, "", 8).is_empty());
        assert!(search(&lib, "oi, responda só 'ok'", 8).is_empty());
        // A word and its glossary twin count once.
        let both = &search(&lib, "roteador router", 1)[0];
        assert_eq!(both.matched.len(), 1, "{both:?}");
        assert_eq!(search(&lib, "ssh", 2).len(), 2);
    }

    #[test]
    fn the_cache_sees_a_new_skill() {
        let (t, lib) = fixture();
        assert!(search(&lib, "zebra", 8).is_empty());
        assert_eq!(count(&lib), FIXTURE.len());
        add(
            &lib,
            t.path(),
            "zebra-printer",
            "Use when printing labels on a Zebra printer.",
            "# ZPL",
        );
        assert_eq!(names(&lib, "zebra")[0], "zebra-printer");
        assert_eq!(count(&lib), FIXTURE.len() + 1);
        lib.set_status("zebra-printer", crate::Status::Disabled)
            .unwrap();
        assert!(search(&lib, "zebra", 8).is_empty());
    }

    #[test]
    fn the_category_comes_from_the_hermes_folder() {
        assert_eq!(
            folder_category(r"hermes:C:\h\skills\networking\mikrotik-routeros-cli"),
            "networking"
        );
        assert_eq!(folder_category(r"hermes:C:\h\skills\ab-testing"), "");
        assert_eq!(folder_category("t"), "");
    }

    /// The imported 307-skill library, when this checkout has it (not in git).
    #[test]
    fn the_real_hermes_library_ranks_when_present() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/home-hermes/skills");
        if !root.join("manifest.json").is_file() {
            return;
        }
        let lib = Library::open(root).unwrap();
        for (q, want, top) in [
            ("mikrotik routeros ssh", "mikrotik-routeros-cli", 1),
            ("olt huawei gpon", "olt", 3),
            ("legenda de video", "video-subtitles", 1),
            ("subtitles", "video-subtitles", 1),
        ] {
            let got = names(&lib, q);
            assert!(got[..top].iter().any(|n| n.contains(want)), "{q}: {got:?}");
        }
    }
}
