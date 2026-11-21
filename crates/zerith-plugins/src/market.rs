//! Marketplaces in the Claude Code format: a repository with
//! `.claude-plugin/marketplace.json` listing plugins and where each one lives.
//!
//! `zerith` (this repository's own catalog) ships inside the binary, so it
//! works offline and with nothing to add. Others are added by `owner/repo`,
//! a git URL or a local folder; their catalog is cached under
//! `<plugins>/marketplaces/`. Installing `name@market` fetches only the
//! plugin's folder (shallow, sparse clone), installs it, and enables it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Library, Plugin, PluginError, Result, Status};

pub const BUILTIN: &str = "zerith";
const BUILTIN_SOURCE: &str = "Mistertelecom/zerith";
const BUILTIN_CATALOG: &str = include_str!("../../../.claude-plugin/marketplace.json");
const CLONE_TIMEOUT: Duration = Duration::from_secs(300);

/// A marketplace the person added. The built-in one is not stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub name: String,
    /// `owner/repo`, a git URL, or a local folder.
    pub source: String,
}

/// One plugin a marketplace offers.
#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    pub market: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub category: String,
    pub homepage: String,
}

fn bad(why: impl Into<String>) -> PluginError {
    PluginError::Invalid(why.into())
}

fn index(lib: &Library) -> PathBuf {
    lib.root.join("marketplaces.json")
}

fn catalog_path(lib: &Library, name: &str) -> PathBuf {
    lib.root.join("marketplaces").join(format!("{name}.json"))
}

/// Added marketplaces, then the built-in one (an added `zerith` replaces it).
pub fn list(lib: &Library) -> Result<Vec<Market>> {
    let mut v: Vec<Market> = match std::fs::read_to_string(index(lib)) {
        Ok(t) => serde_json::from_str(&t)?,
        Err(_) => Vec::new(),
    };
    if !v.iter().any(|m| m.name == BUILTIN) {
        v.push(Market {
            name: BUILTIN.into(),
            source: BUILTIN_SOURCE.into(),
        });
    }
    Ok(v)
}

fn catalog(lib: &Library, m: &Market) -> Result<Value> {
    match std::fs::read_to_string(catalog_path(lib, &m.name)) {
        Ok(t) => Ok(serde_json::from_str(t.trim_start_matches('\u{feff}'))?),
        Err(_) if m.name == BUILTIN => Ok(serde_json::from_str(BUILTIN_CATALOG)?),
        Err(e) => Err(bad(format!(
            "catálogo de {} ausente ({e}); rode `zerith plugin marketplace update {}`",
            m.name, m.name
        ))),
    }
}

/// Every plugin every marketplace offers.
pub fn offers(lib: &Library) -> Result<Vec<Offer>> {
    let mut out = Vec::new();
    for m in list(lib)? {
        let Ok(c) = catalog(lib, &m) else { continue };
        for p in c["plugins"].as_array().into_iter().flatten() {
            let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
            out.push(Offer {
                market: m.name.clone(),
                name: s("name"),
                description: s("description"),
                version: s("version"),
                category: s("category"),
                homepage: s("homepage"),
            });
        }
    }
    Ok(out)
}

/// Adds (or refreshes) a marketplace from `owner/repo`, a git URL or a folder.
pub fn add(lib: &Library, source: &str) -> Result<Market> {
    let source = source.trim().trim_end_matches('/');
    let local = Path::new(source);
    let text = if local.is_dir() {
        std::fs::read_to_string(local.join(".claude-plugin/marketplace.json")).map_err(|e| {
            bad(format!(
                "{source} não tem .claude-plugin/marketplace.json ({e})"
            ))
        })?
    } else {
        let tmp = lib
            .root
            .join("cache")
            .join(format!("market-{}", crate::now_ms()));
        let dir = fetch(&git_url(source)?, None, Some(".claude-plugin"), &tmp)?;
        let t = std::fs::read_to_string(dir.join(".claude-plugin/marketplace.json"));
        let _ = std::fs::remove_dir_all(&tmp);
        t.map_err(|e| {
            bad(format!(
                "{source} não tem .claude-plugin/marketplace.json ({e})"
            ))
        })?
    };
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    let name = v["name"].as_str().unwrap_or_default().to_string();
    crate::validate_name(&name)?;
    std::fs::create_dir_all(lib.root.join("marketplaces"))?;
    std::fs::write(catalog_path(lib, &name), &text)?;
    // A folder is stored absolute: the next call may run elsewhere.
    let stored = if local.is_dir() {
        std::path::absolute(local)?.to_string_lossy().into_owned()
    } else {
        source.to_string()
    };
    let mut all: Vec<Market> = list(lib)?
        .into_iter()
        .filter(|m| m.name != name && m.name != BUILTIN)
        .collect();
    let m = Market {
        name,
        source: stored,
    };
    all.push(m.clone());
    std::fs::write(index(lib), serde_json::to_string_pretty(&all)?)?;
    Ok(m)
}

/// Forgets an added marketplace (the built-in one cannot go).
pub fn remove(lib: &Library, name: &str) -> Result<()> {
    let all: Vec<Market> = list(lib)?
        .into_iter()
        .filter(|m| m.name != BUILTIN)
        .collect();
    if !all.iter().any(|m| m.name == name) {
        return Err(bad(format!("marketplace {name} não foi adicionado")));
    }
    let keep: Vec<Market> = all.into_iter().filter(|m| m.name != name).collect();
    std::fs::write(index(lib), serde_json::to_string_pretty(&keep)?)?;
    let _ = std::fs::remove_file(catalog_path(lib, name));
    Ok(())
}

/// `owner/repo` → its GitHub URL; URLs pass. Anything that git could read as
/// an option is refused.
fn git_url(s: &str) -> Result<String> {
    let ok_part = |p: &str| {
        !p.is_empty()
            && !p.starts_with(['-', '.'])
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if s.starts_with("https://") || s.starts_with("http://") || s.starts_with("git@") {
        return Ok(s.to_string());
    }
    match s.split_once('/') {
        Some((o, r)) if ok_part(o) && ok_part(r) => Ok(format!("https://github.com/{o}/{r}.git")),
        _ => Err(bad(format!(
            "fonte inválida: {s:?} (use dono/repo, uma URL git ou uma pasta)"
        ))),
    }
}

/// Shallow clone of `url` into `dest`; with `sparse`, only that folder is
/// checked out. Returns `dest`.
fn fetch(url: &str, git_ref: Option<&str>, sparse: Option<&str>, dest: &Path) -> Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut clone = vec!["clone", "--depth", "1", "--quiet"];
    if sparse.is_some() {
        clone.extend(["--filter=blob:none", "--sparse"]);
    }
    if let Some(r) = git_ref {
        clone.extend(["--branch", r]);
    }
    let d = dest.to_string_lossy().into_owned();
    clone.extend(["--", url, d.as_str()]);
    git(&clone, None)?;
    if let Some(path) = sparse.filter(|p| !p.is_empty()) {
        git(&["sparse-checkout", "set", "--", path], Some(dest))?;
    }
    Ok(dest.to_path_buf())
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut c = Command::new("git");
    c.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = c
        .spawn()
        .map_err(|e| bad(format!("git não encontrado ({e}); instale o Git")))?;
    let deadline = Instant::now() + CLONE_TIMEOUT;
    loop {
        if let Some(st) = child.try_wait()? {
            if st.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            return Err(bad(format!("git {}: {}", args[0], err.trim())));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(bad(format!(
                "git {} passou de {} s",
                args[0],
                CLONE_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `"./x"` with the catalog's `metadata.pluginRoot` in front, cleaned.
fn relative(cat: &Value, p: &str) -> String {
    let base = cat["metadata"]["pluginRoot"].as_str().unwrap_or("");
    [base, p]
        .iter()
        .map(|s| s.trim().replace('\\', "/"))
        .map(|s| s.trim_start_matches("./").trim_matches('/').to_string())
        .filter(|s| !s.is_empty() && s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Where an entry's plugin lives: `(git URL or local folder, ref, subfolder)`.
fn locate(m: &Market, cat: &Value, entry: &Value) -> Result<(String, Option<String>, String)> {
    let src = &entry["source"];
    let s = |k: &str| src[k].as_str().map(str::to_string);
    let market_repo = || -> Result<String> {
        if Path::new(&m.source).is_dir() {
            Ok(m.source.clone())
        } else {
            git_url(&m.source)
        }
    };
    let path = |k: &str| s(k).map(|p| relative(&Value::Null, &p)).unwrap_or_default();
    match (src.as_str(), s("source").as_deref()) {
        (Some(p), _) => Ok((market_repo()?, None, relative(cat, p))),
        (None, Some("local")) => Ok((market_repo()?, None, relative(cat, &path("path")))),
        (None, Some("github")) => {
            let repo = s("repo").ok_or_else(|| bad("source github sem `repo`"))?;
            Ok((git_url(&repo)?, s("ref"), path("path")))
        }
        (None, Some("url" | "git" | "git-subdir")) => {
            let url = s("url").ok_or_else(|| bad("source sem `url`"))?;
            Ok((git_url(&url)?, s("ref"), path("path")))
        }
        (None, other) => Err(bad(format!(
            "tipo de source não suportado: {}",
            other.unwrap_or("?")
        ))),
    }
}

/// Splits `name@market`; a bare name searches every marketplace.
fn find(lib: &Library, spec: &str) -> Result<(Market, Value, Value)> {
    let (name, want) = match spec.split_once('@') {
        Some((n, m)) => (n, Some(m)),
        None => (spec, None),
    };
    for m in list(lib)? {
        if want.is_some_and(|w| w != m.name) {
            continue;
        }
        let cat = catalog(lib, &m)?;
        let hit = cat["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["name"].as_str() == Some(name))
            .cloned();
        if let Some(entry) = hit {
            return Ok((m, cat, entry));
        }
    }
    Err(bad(match want {
        Some(w) if !list(lib)?.iter().any(|m| m.name == w) => format!(
            "marketplace {w} não foi adicionado; rode `zerith plugin marketplace add <dono/repo>`"
        ),
        Some(w) => format!("{name} não está no marketplace {w}"),
        None => format!("nenhum marketplace oferece {name}"),
    }))
}

/// Whether the plugin runs code of its own (MCP servers, hooks, a process),
/// which stays off until it is reviewed. Skills and agents are text.
pub fn runs_code(p: &Plugin) -> bool {
    let m = &p.manifest;
    !m.mcp.is_empty() || !m.hooks.is_empty() || !m.command.trim().is_empty()
}

/// Installs `name@market` and enables it: its skills and agents are in use
/// from the next turn. Code it runs ([`runs_code`]) waits for `trust`.
pub fn install(lib: &Library, spec: &str) -> Result<Plugin> {
    let (m, cat, entry) = find(lib, spec)?;
    let (from, git_ref, sub) = locate(&m, &cat, &entry)?;
    let tmp = lib.root.join("cache").join(format!(
        "{}-{}",
        entry["name"].as_str().unwrap_or("plugin"),
        crate::now_ms()
    ));
    let base = if Path::new(&from).is_dir() {
        PathBuf::from(&from)
    } else {
        fetch(
            &from,
            git_ref.as_deref(),
            Some(&sub).filter(|s| !s.is_empty()).map(|s| s.as_str()),
            &tmp,
        )?
    };
    let dir = if sub.is_empty() {
        base
    } else {
        base.join(&sub)
    };
    let done = (|| {
        if !dir.is_dir() {
            return Err(bad(format!(
                "a pasta do plugin não existe: {}",
                dir.display()
            )));
        }
        let read = |p: PathBuf| -> Value {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
                .unwrap_or(Value::Null)
        };
        let mut v = match read(dir.join(".claude-plugin/plugin.json")) {
            Value::Null => read(dir.join(crate::MANIFEST)),
            found => found,
        };
        if !v.is_object() {
            v = Value::Object(Default::default());
        }
        // The marketplace entry wins: it is the curated description.
        for (k, val) in entry.as_object().into_iter().flatten() {
            if k != "source" {
                v[k] = val.clone();
            }
        }
        let p = lib.install_claude(&dir, v)?;
        lib.set_status(&p.manifest.name, Status::Enabled)?;
        lib.get(&p.manifest.name)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_catalog_parses_and_lists_its_plugins() {
        let v: Value = serde_json::from_str(BUILTIN_CATALOG).unwrap();
        assert_eq!(v["name"], BUILTIN);
        let names: Vec<&str> = v["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        for want in [
            "zerith-netops",
            "superpowers",
            "context7",
            "ast-grep",
            "pr-review-toolkit",
            "security-guidance",
            "commit-commands",
            "claude-bughunter",
            "cli-anything",
            "stitch-skills",
            "astryx",
            "billion-context",
        ] {
            assert!(names.contains(&want), "{want} fora da loja: {names:?}");
        }
        let m = Market {
            name: BUILTIN.into(),
            source: BUILTIN_SOURCE.into(),
        };
        for p in v["plugins"].as_array().unwrap() {
            locate(&m, &v, p).unwrap_or_else(|e| panic!("{}: {e}", p["name"]));
        }
    }

    #[test]
    fn sources_resolve_like_claude_code_and_options_are_refused() {
        let m = Market {
            name: "x".into(),
            source: "dono/repo".into(),
        };
        let cat = serde_json::json!({"metadata": {"pluginRoot": "./plugins"}});
        let (url, _, sub) = locate(&m, &cat, &serde_json::json!({"source": "./a"})).unwrap();
        assert_eq!(
            (url.as_str(), sub.as_str()),
            ("https://github.com/dono/repo.git", "plugins/a")
        );
        let e = serde_json::json!({"source": {"source": "git-subdir", "url": "https://h/x.git", "path": "p/q/"}});
        assert_eq!(locate(&m, &cat, &e).unwrap().2, "p/q");
        assert!(git_url("--upload-pack=x/y").is_err());
        assert!(git_url("dono/-x").is_err());
    }
}
