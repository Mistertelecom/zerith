//! Optional post-edit auto-formatter (inspired by OpenCode's `Formatter`).
//!
//! Opt-in via `[coding] auto_format = true` in `settings.toml` or
//! `ZERITH_AUTO_FORMAT=1`. Always fail-open with a short 5s timeout so a
//! missing or broken formatter never fails a file edit.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FORMAT_TIMEOUT: Duration = Duration::from_secs(5);

fn auto_format_enabled(workspace: &Path) -> bool {
    if let Ok(v) = std::env::var("ZERITH_AUTO_FORMAT") {
        let v = v.trim();
        if v == "1" || v.eq_ignore_ascii_case("true") {
            return true;
        }
        if v == "0" || v.eq_ignore_ascii_case("false") {
            return false;
        }
    }
    let paths = zerith_core::Paths::resolve();
    zerith_core::Settings::load(&paths, Some(workspace))
        .settings
        .coding
        .auto_format
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p)
            .any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
    })
}

fn find_local_bin(start: &Path, name: &str) -> Option<std::path::PathBuf> {
    for dir in start.ancestors().take(6) {
        let bin = dir.join("node_modules").join(".bin");
        if cfg!(windows) {
            let cmd = bin.join(format!("{name}.cmd"));
            if cmd.is_file() {
                return Some(cmd);
            }
        }
        let plain = bin.join(name);
        if plain.is_file() {
            return Some(plain);
        }
    }
    None
}

fn has_ancestor_file(start: &Path, names: &[&str]) -> bool {
    start
        .ancestors()
        .take(6)
        .any(|d| names.iter().any(|n| d.join(n).is_file()))
}

/// Resolves `(label, program, args)` for formatting `path`, if a matching
/// project formatter is available.
pub(crate) fn pick_formatter(path: &Path) -> Option<(&'static str, String, Vec<String>)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let parent = path.parent().unwrap_or(path);
    let target = path.to_string_lossy().into_owned();
    match ext.as_str() {
        "rs" if on_path("rustfmt") => Some((
            "rustfmt",
            "rustfmt".into(),
            vec!["--edition".into(), "2021".into(), target],
        )),
        "py" if on_path("ruff") => Some((
            "ruff format",
            "ruff".into(),
            vec!["format".into(), "--quiet".into(), target],
        )),
        "go" if on_path("gofmt") => Some(("gofmt", "gofmt".into(), vec!["-w".into(), target])),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "css" => {
            if has_ancestor_file(parent, &["biome.json", "biome.jsonc"]) {
                if let Some(bin) = find_local_bin(parent, "biome") {
                    return Some((
                        "biome",
                        bin.to_string_lossy().into_owned(),
                        vec!["format".into(), "--write".into(), target],
                    ));
                }
                if on_path("biome") {
                    return Some((
                        "biome",
                        "biome".into(),
                        vec!["format".into(), "--write".into(), target],
                    ));
                }
            }
            if let Some(bin) = find_local_bin(parent, "prettier") {
                return Some((
                    "prettier",
                    bin.to_string_lossy().into_owned(),
                    vec!["--write".into(), target],
                ));
            }
            None
        }
        _ => None,
    }
}

/// Formats `path` in place when `[coding] auto_format = true` (or
/// `ZERITH_AUTO_FORMAT=1`) and a matching formatter is installed. Returns a
/// short note only when the formatter ran and changed the file.
pub(crate) fn format_if_enabled(workspace: &Path, path: &Path) -> Option<String> {
    if !auto_format_enabled(workspace) || !path.is_file() {
        return None;
    }
    let (label, program, args) = pick_formatter(path)?;
    let before = std::fs::read(path).ok()?;

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(path.parent().unwrap_or(workspace))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + FORMAT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let after = std::fs::read(path).ok()?;
    (after != before).then(|| format!("\n[formatado automaticamente com {label}]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_local_biome_or_prettier_for_ts_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let bin = tmp.path().join("node_modules/.bin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(tmp.path().join("biome.json"), "{}").unwrap();
        let biome_name = if cfg!(windows) { "biome.cmd" } else { "biome" };
        std::fs::write(bin.join(biome_name), "").unwrap();

        let ts = src.join("app.ts");
        std::fs::write(&ts, "const x=1\n").unwrap();
        let picked = pick_formatter(&ts).unwrap();
        assert_eq!(picked.0, "biome");
        assert!(picked.2.contains(&"format".to_string()));
    }
}
