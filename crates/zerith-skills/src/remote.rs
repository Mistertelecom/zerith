//! Skills inside a GitHub repository: list them and install one folder
//! without cloning anything.
//!
//! Two API requests per repository (metadata for the default branch, then the
//! recursive tree); every file itself comes from `raw.githubusercontent.com`,
//! which does not spend the API quota. Installing downloads into a staging
//! folder and hands it to [`Library::install`], so the same scan and the same
//! name rules apply as for a local folder. Nothing downloaded is executed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::catalog::{now_ms, CACHE_TTL_MS};
use crate::install::{InstallError, Installed, Library};

pub const MAX_FILES: usize = 300;
pub const MAX_TOTAL: u64 = 10 * 1024 * 1024;
pub const MAX_FILE: u64 = 5 * 1024 * 1024;
/// How many SKILL.md headers are fetched for descriptions, and by how many
/// threads. ponytail: skills past the cap list without a description; raise
/// it or page the UI if a repository ships hundreds.
const DESCRIBE: usize = 80;
const THREADS: usize = 8;

const API: &str = "https://api.github.com";
const RAW: &str = "https://raw.githubusercontent.com";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSkill {
    /// Folder inside the repository; `""` for a SKILL.md at the root.
    pub path: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteRepo {
    pub repo: String,
    pub branch: String,
    pub fetched_at: i64,
    pub skills: Vec<RemoteSkill>,
}

/// One file of the repository tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("GitHub: {0}")]
    Http(String),
    #[error("o GitHub limitou as requisições sem login (60 por hora); tente mais tarde ou defina a variável de ambiente GITHUB_TOKEN")]
    Quota,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Install(#[from] InstallError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RemoteError>;

/// `owner/name` as GitHub spells it. Checked before the value reaches a URL
/// or a cache file name.
pub fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.is_empty()
        && !name.contains("..")
        && name != "."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 200
        && !branch.contains("..")
        && !branch.starts_with(['/', '-'])
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// A path from the remote tree, relative to the skill folder. Rejected
/// instead of cleaned: a tree that tries to climb out is not installed at all.
pub fn safe_rel(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && !p.contains('\0')
        && !p.contains("..")
        && p.split('/').all(|s| !s.is_empty() && s != ".")
}

/// Files of a recursive tree response. Symlinks (mode 120000) and submodules
/// are not files a skill can ship.
pub fn parse_tree(v: &serde_json::Value) -> Vec<Blob> {
    v["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["type"] == "blob" && e["mode"] != "120000")
        .filter_map(|e| {
            Some(Blob {
                path: e["path"].as_str()?.to_string(),
                size: e["size"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

/// Every folder holding a SKILL.md, named after the folder until the header
/// is read.
pub fn skills_in(blobs: &[Blob], repo: &str) -> Vec<RemoteSkill> {
    let repo_name = repo.rsplit('/').next().unwrap_or(repo);
    let mut out: Vec<RemoteSkill> = blobs
        .iter()
        .filter_map(|b| {
            let dir = if b.path == "SKILL.md" {
                ""
            } else {
                b.path.strip_suffix("/SKILL.md")?
            };
            let name = dir
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(repo_name);
            Some(RemoteSkill {
                path: dir.to_string(),
                name: name.to_string(),
                description: String::new(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    out
}

/// The files of one skill folder as `(path in repo, path in the skill)`, after
/// the security and size limits.
pub fn plan_files(blobs: &[Blob], dir: &str) -> Result<Vec<(String, String)>> {
    if !dir.is_empty() && !safe_rel(dir) {
        return Err(RemoteError::Invalid(format!("pasta inválida: {dir:?}")));
    }
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let picked: Vec<&Blob> = blobs
        .iter()
        .filter(|b| b.path.starts_with(&prefix))
        .collect();
    if !picked.iter().any(|b| b.path[prefix.len()..] == *"SKILL.md") {
        return Err(RemoteError::Invalid(format!(
            "{dir:?} não tem um SKILL.md no repositório"
        )));
    }
    if picked.len() > MAX_FILES {
        return Err(RemoteError::Invalid(format!(
            "a skill tem {} arquivos; o limite é {MAX_FILES}",
            picked.len()
        )));
    }
    let mut total = 0u64;
    let mut out = Vec::with_capacity(picked.len());
    for b in picked {
        let rel = &b.path[prefix.len()..];
        if !safe_rel(rel) {
            return Err(RemoteError::Invalid(format!(
                "caminho recusado: {:?}",
                b.path
            )));
        }
        if b.size > MAX_FILE {
            return Err(RemoteError::Invalid(format!("{} passa de 5 MB", b.path)));
        }
        total += b.size;
        out.push((b.path.clone(), rel.to_string()));
    }
    if total > MAX_TOTAL {
        return Err(RemoteError::Invalid("a skill passa de 10 MB".into()));
    }
    Ok(out)
}

/// The cached listing while it is fresh, otherwise GitHub.
pub fn load(root: &Path, repo: &str, token: Option<&str>, force: bool) -> Result<RemoteRepo> {
    if !valid_repo(repo) {
        return Err(RemoteError::Invalid(format!(
            "repositório inválido: {repo:?}"
        )));
    }
    let path = cache_path(root, repo);
    if !force {
        let cached: Option<RemoteRepo> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(c) = cached.filter(|c| now_ms() - c.fetched_at < CACHE_TTL_MS) {
            return Ok(c);
        }
    }
    let fresh = list_repo_skills(repo, token)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&fresh)?)?;
    Ok(fresh)
}

fn cache_path(root: &Path, repo: &str) -> PathBuf {
    root.join("remote-skills")
        .join(format!("{}.json", repo.replace('/', "__")))
}

/// The `description:` of a SKILL.md header without validation, folded
/// blocks joined, cut to 300 chars for a card.
fn loose_description(text: &str) -> String {
    let Some(header) = text.strip_prefix("---").and_then(|r| r.split("\n---").next()) else {
        return String::new();
    };
    let mut out = String::new();
    let mut folded = false;
    for line in header.lines() {
        if folded {
            if line.starts_with([' ', '\t']) {
                out.push_str(if out.is_empty() { "" } else { " " });
                out.push_str(line.trim());
                continue;
            }
            break;
        }
        if let Some(v) = line.strip_prefix("description:") {
            let v = v.trim();
            if matches!(v, ">" | "|" | ">-" | "|-") {
                folded = true;
            } else {
                out = v.trim_matches(['"', '\'']).to_string();
                break;
            }
        }
    }
    match out.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &out[..i]),
        None => out,
    }
}

/// Lists the skills of a repository: default branch, tree, then the header
/// of each SKILL.md for its real name and description.
pub fn list_repo_skills(repo: &str, token: Option<&str>) -> Result<RemoteRepo> {
    if !valid_repo(repo) {
        return Err(RemoteError::Invalid(format!(
            "repositório inválido: {repo:?}"
        )));
    }
    let meta = get_json(&format!("{API}/repos/{repo}"), token)?;
    let branch = meta["default_branch"]
        .as_str()
        .unwrap_or("main")
        .to_string();
    let tree = get_json(
        &format!("{API}/repos/{repo}/git/trees/{}?recursive=1", enc(&branch)),
        token,
    )?;
    // ponytail: a tree over GitHub's 100k-entry cap comes back `truncated`
    // and lists only part of the skills; walk subtrees if a source hits it.
    let mut skills = skills_in(&parse_tree(&tree), repo);
    let n = skills.len().min(DESCRIBE);
    let chunk = n.div_ceil(THREADS).max(1);
    let branch_ref = branch.as_str();
    std::thread::scope(|s| {
        for part in skills[..n].chunks_mut(chunk) {
            s.spawn(move || {
                for sk in part {
                    let file = join(&sk.path, "SKILL.md");
                    let Ok(text) = get_raw(repo, branch_ref, &file, 256 * 1024) else {
                        continue;
                    };
                    let text = String::from_utf8_lossy(&text);
                    match crate::parse(&text) {
                        Ok((fm, _)) => {
                            sk.name = fm.name;
                            sk.description = fm.description;
                        }
                        // Listing only: a header the install scan would refuse
                        // (e.g. description over 1024 chars) still shows what
                        // it is; the install itself keeps the strict check.
                        Err(_) => sk.description = loose_description(&text),
                    }
                }
            });
        }
    });
    Ok(RemoteRepo {
        repo: repo.to_string(),
        branch,
        fetched_at: now_ms(),
        skills,
    })
}

/// Downloads one skill folder and installs it like a local one.
pub fn install_remote(
    lib: &Library,
    repo: &str,
    branch: &str,
    dir: &str,
    token: Option<&str>,
) -> Result<Installed> {
    if !valid_repo(repo) || !valid_branch(branch) {
        return Err(RemoteError::Invalid(format!(
            "origem inválida: {repo:?} @ {branch:?}"
        )));
    }
    let tree = get_json(
        &format!("{API}/repos/{repo}/git/trees/{}?recursive=1", enc(branch)),
        token,
    )?;
    let files = plan_files(&parse_tree(&tree), dir)?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let staging = lib.root().join(".downloads").join(format!(
        "{}-{}-{}",
        now_ms(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = download(repo, branch, &files, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let source = if dir.is_empty() {
        format!("github:{repo}")
    } else {
        format!("github:{repo}/{dir}")
    };
    install_staged(lib, &staging, &source)
}

/// Installs a downloaded folder and always deletes it, also on failure.
pub fn install_staged(lib: &Library, staging: &Path, source: &str) -> Result<Installed> {
    let out = lib.install(staging, source).map_err(RemoteError::from);
    let _ = std::fs::remove_dir_all(staging);
    out
}

fn download(repo: &str, branch: &str, files: &[(String, String)], staging: &Path) -> Result<()> {
    let mut total = 0u64;
    for (full, rel) in files {
        // The tree's sizes are a promise; the byte cap is the enforcement.
        let bytes = get_raw(repo, branch, full, MAX_FILE)?;
        total += bytes.len() as u64;
        if total > MAX_TOTAL {
            return Err(RemoteError::Invalid("a skill passa de 10 MB".into()));
        }
        let target = staging.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
}

fn status_error(e: ureq::Error, url: &str) -> RemoteError {
    match e {
        ureq::Error::Status(403 | 429, _) => RemoteError::Quota,
        ureq::Error::Status(404, _) => RemoteError::Http(format!("não encontrado: {url}")),
        e => RemoteError::Http(e.to_string()),
    }
}

fn get_json(url: &str, token: Option<&str>) -> Result<serde_json::Value> {
    let mut req = agent()
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "zerith");
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let body = req
        .call()
        .map_err(|e| status_error(e, url))?
        .into_string()
        .map_err(|e| RemoteError::Http(e.to_string()))?;
    Ok(serde_json::from_str(&body)?)
}

fn get_raw(repo: &str, branch: &str, path: &str, limit: u64) -> Result<Vec<u8>> {
    let url = format!("{RAW}/{repo}/{}/{}", enc(branch), enc(path));
    let resp = agent()
        .get(&url)
        .set("User-Agent", "zerith")
        .call()
        .map_err(|e| status_error(e, &url))?;
    let mut bytes = Vec::new();
    resp.into_reader().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(RemoteError::Invalid(format!(
            "{path} passa do limite de tamanho"
        )));
    }
    Ok(bytes)
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// Percent-encodes a path for a URL, keeping `/` as the separator.
fn enc(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn listing_shows_a_description_the_strict_parser_refuses() {
        let long = "x ".repeat(600);
        let md = format!("---\nname: big\ndescription: |-\n  {long}\n  more\nlicense: MIT\n---\n# big\n");
        assert!(crate::parse(&md).is_err());
        let d = loose_description(&md);
        assert!(d.starts_with("x x") && d.ends_with('…'), "{d}");
        assert_eq!(loose_description("---\nname: a\ndescription: \"Lê PDFs\"\n---\n"), "Lê PDFs");
    }

    use super::*;

    fn unique(tag: &str) -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tree() -> Vec<Blob> {
        parse_tree(&serde_json::json!({
            "tree": [
                {"path": "SKILL.md", "mode": "100644", "type": "blob", "size": 10},
                {"path": "skills", "mode": "040000", "type": "tree"},
                {"path": "skills/pdf/SKILL.md", "mode": "100644", "type": "blob", "size": 20},
                {"path": "skills/pdf/scripts/fill.py", "mode": "100755", "type": "blob", "size": 30},
                {"path": "skills/pdf-extra/SKILL.md", "mode": "100644", "type": "blob", "size": 5},
                {"path": "skills/deep/nested/docx/SKILL.md", "mode": "100644", "type": "blob", "size": 5},
                {"path": "skills/pdf/link", "mode": "120000", "type": "blob", "size": 9},
                {"path": "vendor/lib", "mode": "160000", "type": "commit"},
                {"path": "README.SKILL.md", "mode": "100644", "type": "blob", "size": 1}
            ],
            "truncated": false
        }))
    }

    #[test]
    fn the_tree_lists_root_and_nested_skills_only() {
        let skills = skills_in(&tree(), "acme/kit");
        let found: Vec<(&str, &str)> = skills
            .iter()
            .map(|s| (s.path.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("skills/deep/nested/docx", "docx"),
                ("", "kit"),
                ("skills/pdf", "pdf"),
                ("skills/pdf-extra", "pdf-extra"),
            ]
        );
    }

    #[test]
    fn a_skill_folder_takes_its_own_files_without_symlinks_or_siblings() {
        let files = plan_files(&tree(), "skills/pdf").unwrap();
        let rel: Vec<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(rel, vec!["SKILL.md", "scripts/fill.py"]);
        assert!(plan_files(&tree(), "skills").is_err(), "no SKILL.md there");
        assert!(plan_files(&tree(), "../skills/pdf").is_err());
    }

    #[test]
    fn limits_are_enforced_before_any_download() {
        let mut big = vec![Blob {
            path: "s/SKILL.md".into(),
            size: 1,
        }];
        big.push(Blob {
            path: "s/huge.bin".into(),
            size: MAX_FILE + 1,
        });
        assert!(plan_files(&big, "s").is_err());
        let many: Vec<Blob> = std::iter::once(Blob {
            path: "s/SKILL.md".into(),
            size: 1,
        })
        .chain((0..MAX_FILES).map(|i| Blob {
            path: format!("s/f{i}"),
            size: 1,
        }))
        .collect();
        assert!(plan_files(&many, "s").is_err());
        let heavy: Vec<Blob> = std::iter::once(Blob {
            path: "s/SKILL.md".into(),
            size: 1,
        })
        .chain((0..3).map(|i| Blob {
            path: format!("s/f{i}"),
            size: 4 * 1024 * 1024,
        }))
        .collect();
        assert!(plan_files(&heavy, "s").is_err());
    }

    #[test]
    fn paths_that_could_escape_are_rejected() {
        for bad in [
            "../x",
            "a/../../b",
            "C:/x",
            "/etc",
            "a\\b",
            "a//b",
            "",
            "./a",
            "a/.",
        ] {
            assert!(!safe_rel(bad), "{bad:?} should be rejected");
        }
        assert!(safe_rel("scripts/fill.py"));
        let evil = vec![
            Blob {
                path: "s/SKILL.md".into(),
                size: 1,
            },
            Blob {
                path: "s/../../boot.ini".into(),
                size: 1,
            },
        ];
        assert!(plan_files(&evil, "s").is_err());
    }

    #[test]
    fn repository_names_are_checked() {
        for ok in crate::catalog::SOURCES {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in ["a", "a/..", "a/b/c", "/b", "a/", "a b/c", "a/b?x"] {
            assert!(!valid_repo(bad), "{bad:?}");
        }
        assert!(valid_branch("main") && valid_branch("release/v2"));
        assert!(!valid_branch("../main") && !valid_branch("a b"));
    }

    #[test]
    fn a_staged_folder_installs_and_is_always_deleted() {
        let root = unique("staged");
        let lib = Library::open(root.join("skills")).unwrap();
        let staging = root.join("skills/.downloads/one");
        std::fs::create_dir_all(staging.join("scripts")).unwrap();
        std::fs::write(
            staging.join("SKILL.md"),
            "---\nname: pdf\ndescription: Lê PDFs\n---\n\n# pdf\n",
        )
        .unwrap();
        std::fs::write(staging.join("scripts/fill.py"), "print(1)\n").unwrap();
        let row = install_staged(&lib, &staging, "github:acme/kit/skills/pdf").unwrap();
        assert_eq!(row.name, "pdf");
        assert_eq!(row.source, "github:acme/kit/skills/pdf");
        assert!(lib.root().join("pdf/scripts/fill.py").exists());
        assert!(!staging.exists());

        // A broken download is deleted too.
        let bad = root.join("skills/.downloads/two");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), "no header").unwrap();
        assert!(install_staged(&lib, &bad, "x").is_err());
        assert!(!bad.exists());
    }

    #[test]
    fn a_fresh_listing_is_read_from_the_cache() {
        let root = unique("cache");
        let cached = RemoteRepo {
            repo: "acme/kit".into(),
            branch: "main".into(),
            fetched_at: now_ms(),
            skills: vec![RemoteSkill {
                path: "skills/pdf".into(),
                name: "pdf".into(),
                description: "d".into(),
            }],
        };
        std::fs::create_dir_all(root.join("remote-skills")).unwrap();
        std::fs::write(
            cache_path(&root, "acme/kit"),
            serde_json::to_vec(&cached).unwrap(),
        )
        .unwrap();
        assert_eq!(load(&root, "acme/kit", None, false).unwrap(), cached);
        assert!(load(&root, "acme/..", None, false).is_err());
    }

    #[test]
    fn urls_encode_spaces_but_keep_slashes() {
        assert_eq!(
            enc("skills/my skill/SKILL.md"),
            "skills/my%20skill/SKILL.md"
        );
    }

    /// Real network: `cargo test -p zerith-skills -- --ignored smoke`.
    #[test]
    #[ignore]
    fn smoke_lists_anthropic_skills() {
        let token = std::env::var("GITHUB_TOKEN").ok();
        let repo = list_repo_skills("anthropics/skills", token.as_deref()).unwrap();
        eprintln!(
            "{} @ {}: {} skills",
            repo.repo,
            repo.branch,
            repo.skills.len()
        );
        for s in repo.skills.iter().take(5) {
            eprintln!("  {} ({}) — {}", s.name, s.path, s.description);
        }
        let blank: Vec<&str> = repo
            .skills
            .iter()
            .filter(|s| s.description.is_empty())
            .map(|s| s.path.as_str())
            .collect();
        eprintln!("sem descrição: {blank:?}");
        assert!(repo
            .skills
            .iter()
            .any(|s| s.name == "pdf" && !s.description.is_empty()));

        let root = unique("smoke");
        let lib = Library::open(root.join("skills")).unwrap();
        let row = install_remote(
            &lib,
            &repo.repo,
            &repo.branch,
            "skills/brand-guidelines",
            token.as_deref(),
        )
        .unwrap();
        eprintln!("instalada: {} <- {}", row.name, row.source);
        assert!(lib.root().join(&row.dir).join("SKILL.md").exists());
        assert_eq!(
            std::fs::read_dir(lib.root().join(".downloads"))
                .unwrap()
                .count(),
            0
        );
    }
}
