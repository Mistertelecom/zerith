//! The GitHub-backed catalogue.
//!
//! Three rules the spec makes explicit, encoded here:
//!
//! 1. **Stars are popularity, not a certification.** Nothing in this module
//!    derives a quality or safety score from them.
//! 2. **Cached and timestamped.** Every entry carries when it was fetched, so
//!    the UI can say "as of" instead of implying live data.
//! 3. **Rate limited on purpose.** One request per repository, with a TTL, so
//!    opening the library twice does not spend the anonymous quota.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Repositories named by the spec. The list is data, not a claim that all of
/// them are compatible: compatibility is decided by the SKILL.md scan.
pub const SOURCES: &[&str] = &[
    "mattpocock/skills",
    "obra/superpowers",
    "anthropics/skills",
    "vercel-labs/agent-skills",
    "github/awesome-copilot",
    "ComposioHQ/awesome-claude-skills",
    "openai/skills",
    "agentskills/agentskills",
];

/// How long a cached answer is used before the network is asked again.
pub const CACHE_TTL_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub repo: String,
    pub description: String,
    pub stars: u64,
    pub forks: u64,
    pub updated_at: String,
    pub license: String,
    pub url: String,
    /// Why this repository is in the list. The spec names them; the note keeps
    /// the reason visible instead of implying an endorsement.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    pub fetched_at: i64,
    pub entries: Vec<RepoInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("http: {0}")]
    Http(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no GitHub token and the anonymous quota is exhausted")]
    Quota,
}

pub type Result<T> = std::result::Result<T, CatalogError>;

/// Reads the cache when it is fresh, otherwise asks GitHub.
pub fn load(root: &Path, token: Option<&str>, force: bool) -> Result<Catalogue> {
    let path = cache_path(root);
    if !force {
        if let Some(c) = read_cache(&path) {
            if now_ms() - c.fetched_at < CACHE_TTL_MS {
                return Ok(c);
            }
        }
    }
    let mut entries = Vec::new();
    for repo in SOURCES {
        // One failure must not empty the library: the remaining sources are
        // still worth showing.
        if let Ok(info) = fetch_repo(repo, token) {
            entries.push(info);
        }
    }
    let catalogue = Catalogue {
        fetched_at: now_ms(),
        entries,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&catalogue)?)?;
    Ok(catalogue)
}

fn cache_path(root: &Path) -> PathBuf {
    root.join("catalog.json")
}

fn read_cache(path: &Path) -> Option<Catalogue> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Fetches one repository. Kept separate so the parser is testable without a
/// socket.
pub fn fetch_repo(repo: &str, token: Option<&str>) -> Result<RepoInfo> {
    let url = format!("https://api.github.com/repos/{repo}");
    let mut req = ureq::get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "zerith");
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let body = match req.call() {
        Ok(r) => r
            .into_string()
            .map_err(|e| CatalogError::Http(e.to_string()))?,
        Err(ureq::Error::Status(403, _)) | Err(ureq::Error::Status(429, _)) => {
            return Err(CatalogError::Quota)
        }
        Err(e) => return Err(CatalogError::Http(e.to_string())),
    };
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(parse_repo(repo, &v))
}

/// The fields the library shows, from GitHub's repository object. Missing
/// fields degrade to empty strings instead of failing: a repository without a
/// licence is a fact to display, not an error.
pub fn parse_repo(repo: &str, v: &serde_json::Value) -> RepoInfo {
    RepoInfo {
        repo: repo.to_string(),
        description: v["description"].as_str().unwrap_or_default().to_string(),
        stars: v["stargazers_count"].as_u64().unwrap_or(0),
        forks: v["forks_count"].as_u64().unwrap_or(0),
        updated_at: v["pushed_at"].as_str().unwrap_or_default().to_string(),
        license: v["license"]["spdx_id"]
            .as_str()
            .filter(|s| *s != "NOASSERTION")
            .unwrap_or("unknown")
            .to_string(),
        url: v["html_url"].as_str().unwrap_or_default().to_string(),
        note: "fonte nomeada na especificação".to_string(),
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_without_a_licence_is_reported_as_unknown() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"stargazers_count":12,"forks_count":3,"pushed_at":"2026-01-02T03:04:05Z","license":{"spdx_id":"NOASSERTION"},"html_url":"https://x"}"#,
        )
        .unwrap();
        let info = parse_repo("a/b", &v);
        assert_eq!(info.license, "unknown");
        assert_eq!(info.stars, 12);
    }

    #[test]
    fn a_missing_field_does_not_fail_the_entry() {
        let v: serde_json::Value = serde_json::from_str("{}").unwrap();
        let info = parse_repo("a/b", &v);
        assert!(info.description.is_empty());
        assert_eq!(info.stars, 0);
    }

    #[test]
    fn a_fresh_cache_is_used_instead_of_the_network() {
        let dir = tempfile::tempdir_in(scratch()).unwrap();
        let c = Catalogue {
            fetched_at: now_ms(),
            entries: vec![parse_repo(
                "a/b",
                &serde_json::json!({"stargazers_count": 5}),
            )],
        };
        std::fs::write(cache_path(dir.path()), serde_json::to_vec(&c).unwrap()).unwrap();
        let loaded = load(dir.path(), None, false).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].stars, 5);
    }

    fn scratch() -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
