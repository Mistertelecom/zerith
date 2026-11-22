//! Read-only search tools: `list_dir`, `glob` and `grep` (ripgrep when
//! available, a parallel gitignore-aware walker otherwise).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use zerith_security::glob_match;

use super::str_arg;
use crate::{CallCtx, Exec, Result, ToolError};

pub(super) fn list_dir(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(ctx.resolve(str_arg(args, "path")?))? {
        let entry = entry?;
        let mut n = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            n.push('/');
        }
        names.push(n);
    }
    names.sort();
    Ok(names.join("\n"))
}

/// Directories nobody wants searched: build output and VCS internals.
fn skipped(e: &walkdir::DirEntry) -> bool {
    e.depth() > 0
        && e.file_type().is_dir()
        && matches!(
            e.file_name().to_str(),
            Some(".git" | "target" | "node_modules" | "dist" | ".next" | "build")
        )
}

#[derive(Clone)]
struct IgnoreRule {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    pattern: String,
}

fn parse_gitignore(content: &str) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, mut pat) = match line.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let dir_only = pat.ends_with('/');
        if dir_only {
            pat = pat.trim_end_matches('/');
        }
        let anchored = pat.starts_with('/');
        if anchored {
            pat = pat.trim_start_matches('/');
        }
        if !pat.is_empty() {
            rules.push(IgnoreRule {
                negated,
                dir_only,
                anchored,
                pattern: pat.to_string(),
            });
        }
    }
    rules
}

fn load_root_gitignore(base: &Path) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    let mut dir = if base.is_dir() {
        base.to_path_buf()
    } else {
        base.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let mut chain = Vec::new();
    loop {
        let gi = dir.join(".gitignore");
        if gi.is_file() {
            if let Ok(text) = std::fs::read_to_string(&gi) {
                chain.push(parse_gitignore(&text));
            }
        }
        if dir.join(".git").exists() {
            break;
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => break,
        }
    }
    for group in chain.into_iter().rev() {
        rules.extend(group);
    }
    rules
}

fn matches_gitignore(rules: &[IgnoreRule], rel: &str, name: &str, is_dir: bool) -> bool {
    let mut ignored = false;
    for r in rules {
        if r.dir_only && !is_dir {
            continue;
        }
        let hit = if r.anchored || r.pattern.contains('/') {
            glob_match(&r.pattern, rel, true)
                || (is_dir && rel.starts_with(&format!("{}/", r.pattern)))
        } else {
            glob_match(&r.pattern, name, true) || glob_match(&r.pattern, rel, true)
        };
        if hit {
            ignored = !r.negated;
        }
    }
    ignored
}

/// Walks files under `base`, yielding (relative path with `/`, entry).
fn walk<'a>(
    base: &'a Path,
    exec: &'a Exec<'_>,
) -> impl Iterator<Item = Result<(String, walkdir::DirEntry)>> + 'a {
    let base_rules = load_root_gitignore(base);
    let nested = std::sync::Mutex::new(Vec::<(PathBuf, Vec<IgnoreRule>)>::new());
    walkdir::WalkDir::new(base)
        // Directory order is whatever the file system hands back (it differed
        // between Windows and the CI runner): sorted, the model gets the same
        // answer twice.
        .sort_by_file_name()
        .into_iter()
        .filter_entry(move |e| {
            if skipped(e) {
                return false;
            }
            if e.depth() == 0 {
                return true;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            let name = e.file_name().to_string_lossy();
            let is_dir = e.file_type().is_dir();
            if matches_gitignore(&base_rules, &rel, &name, is_dir) {
                return false;
            }
            if let Ok(guard) = nested.lock() {
                for (dir, rules) in guard.iter() {
                    if let Ok(sub_rel) = e.path().strip_prefix(dir) {
                        let sub = sub_rel.to_string_lossy().replace('\\', "/");
                        if matches_gitignore(rules, &sub, &name, is_dir) {
                            return false;
                        }
                    }
                }
            }
            if is_dir {
                let gi = e.path().join(".gitignore");
                if gi.is_file() {
                    if let Ok(text) = std::fs::read_to_string(&gi) {
                        let parsed = parse_gitignore(&text);
                        if !parsed.is_empty() {
                            if let Ok(mut guard) = nested.lock() {
                                guard.push((e.path().to_path_buf(), parsed));
                            }
                        }
                    }
                }
            }
            true
        })
        .filter_map(move |e| {
            if let Err(err) = exec.check() {
                return Some(Err(err));
            }
            let e = e.ok()?;
            if !e.file_type().is_file() {
                return None;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            Some(Ok((rel, e)))
        })
}

const MAX_MATCHES: usize = 500;

pub(super) fn glob(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let pattern = str_arg(args, "pattern")?.replace('\\', "/");
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let mut hits = Vec::new();
    for item in walk(&base, exec) {
        let (rel, _) = item?;
        if glob_match(&pattern, &rel, true) {
            hits.push(rel);
            if hits.len() >= MAX_MATCHES {
                hits.push(format!("[parado em {MAX_MATCHES} resultados]"));
                break;
            }
        }
    }
    if hits.is_empty() {
        return Ok("nenhum arquivo encontrado".into());
    }
    Ok(hits.join("\n"))
}

fn rg_available() -> bool {
    static RG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RG.get_or_init(|| {
        Command::new("rg")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[allow(clippy::too_many_arguments)]
fn try_rg_grep(
    raw: &str,
    literal: bool,
    insensitive: bool,
    mode: &str,
    filter: Option<&str>,
    before: usize,
    after: usize,
    cap: usize,
    base: &Path,
) -> Option<String> {
    if !rg_available() || base.is_file() {
        return None;
    }
    let mut cmd = Command::new("rg");
    cmd.current_dir(base)
        .arg("--color")
        .arg("never")
        .arg("--sort")
        .arg("path")
        .arg("--no-heading");
    if literal {
        cmd.arg("-F");
    }
    if insensitive {
        cmd.arg("-i");
    }
    if let Some(g) = filter {
        cmd.arg("--glob").arg(g);
    }
    match mode {
        "files" => {
            cmd.arg("-l");
        }
        "count" => {
            cmd.arg("-c");
        }
        _ => {
            cmd.arg("--line-number");
            if before > 0 {
                cmd.arg("-B").arg(before.to_string());
            }
            if after > 0 {
                cmd.arg("-A").arg(after.to_string());
            }
        }
    }
    cmd.arg("--").arg(raw).arg(".");
    let out = cmd.stdin(Stdio::null()).output().ok()?;
    let code = out.status.code().unwrap_or(2);
    if code == 2 {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = Vec::new();
    let mut truncated = false;
    for l in text.lines() {
        if lines.len() >= cap {
            truncated = true;
            break;
        }
        let clean_path = l
            .strip_prefix("./")
            .or_else(|| l.strip_prefix(".\\"))
            .unwrap_or(l);
        if mode == "content" {
            if clean_path == "--" {
                lines.push("--".to_string());
            } else if let Some((file, ln, sep, body)) = parse_rg_content_line(clean_path) {
                lines.push(format!("{file}{sep}{ln}{sep} {body}"));
            } else {
                lines.push(clean_path.replace('\\', "/"));
            }
        } else if let Some((f, c)) = clean_path.rsplit_once(':') {
            lines.push(format!("{}:{c}", f.replace('\\', "/")));
        } else {
            lines.push(clean_path.replace('\\', "/"));
        }
    }
    if lines.is_empty() {
        return Some("nenhuma ocorrência".into());
    }
    if truncated {
        lines.push(format!("[parado em {cap} resultados]"));
    }
    Some(lines.join("\n"))
}

fn parse_rg_content_line(line: &str) -> Option<(String, &str, char, &str)> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        let sep = bytes[i];
        if sep != b':' && sep != b'-' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 && j < bytes.len() && bytes[j] == sep {
            let file = line[..i].replace('\\', "/");
            let ln = &line[i + 1..j];
            let body = line[j + 1..].trim_end();
            return Some((file, ln, sep as char, body));
        }
    }
    None
}

pub(super) fn grep(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let insensitive = args["-i"]
        .as_bool()
        .or_else(|| args["case_insensitive"].as_bool())
        .unwrap_or(true);
    let ctx_both = args["-C"]
        .as_u64()
        .or_else(|| args["context"].as_u64())
        .unwrap_or(0) as usize;
    let before = args["-B"]
        .as_u64()
        .or_else(|| args["before_context"].as_u64())
        .map_or(ctx_both, |n| n as usize)
        .min(20);
    let after = args["-A"]
        .as_u64()
        .or_else(|| args["after_context"].as_u64())
        .map_or(ctx_both, |n| n as usize)
        .min(20);
    let raw = str_arg(args, "query")?;
    let mut literal = false;
    // A regex (`vlan 13[0-9]`, `^interface`); text that is not a valid
    // pattern (`foo(`) is searched literally, as before.
    let re = regex::RegexBuilder::new(raw)
        .case_insensitive(insensitive)
        .size_limit(1 << 20)
        .build()
        .or_else(|_| {
            literal = true;
            regex::RegexBuilder::new(&regex::escape(raw))
                .case_insensitive(insensitive)
                .build()
        })
        .map_err(|e| ToolError::BadArguments(e.to_string()))?;
    let mode = args["output_mode"].as_str().unwrap_or("content");
    if !matches!(mode, "content" | "files" | "count") {
        return Err(ToolError::BadArguments(
            "output_mode precisa ser content, files ou count".into(),
        ));
    }
    let cap = args["head_limit"]
        .as_u64()
        .map_or(MAX_MATCHES, |n| (n as usize).clamp(1, MAX_MATCHES));
    let filter = args["glob"].as_str();
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));

    if let Some(fast) = try_rg_grep(
        raw,
        literal,
        insensitive,
        mode,
        filter,
        before,
        after,
        cap,
        &base,
    ) {
        return Ok(fast);
    }

    let mut candidates = Vec::new();
    for item in walk(&base, exec) {
        let (rel, entry) = item?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(f) = filter {
            if !glob_match(f, &rel, true) && !glob_match(f, &name, true) {
                continue;
            }
        }
        let shown = if base.is_file() { name } else { rel };
        candidates.push((shown, entry.into_path()));
    }

    // Parallel file read + regex matching across workers, then deterministic merge.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(2)
        .min(candidates.len().max(1));
    let mut per_file: Vec<(String, Vec<String>, usize)> = Vec::with_capacity(candidates.len());
    if candidates.len() >= 16 && workers > 1 {
        let chunk_sz = candidates.len().div_ceil(workers);
        let re_ref = &re;
        let parts: Vec<Vec<(String, Vec<String>, usize)>> = std::thread::scope(|s| {
            let handles: Vec<_> = candidates
                .chunks(chunk_sz)
                .map(|slice| {
                    s.spawn(move || {
                        let mut local = Vec::new();
                        for (shown, path) in slice {
                            if exec.cancel.is_cancelled() {
                                break;
                            }
                            if let Ok(text) = std::fs::read_to_string(path) {
                                let (lines, hits) =
                                    scan_file(shown, &text, re_ref, mode, before, after);
                                if hits > 0 {
                                    local.push((shown.clone(), lines, hits));
                                }
                            }
                        }
                        local
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for chunk in parts {
            per_file.extend(chunk);
        }
    } else {
        for (shown, path) in &candidates {
            exec.check()?;
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let (lines, hits) = scan_file(shown, &text, &re, mode, before, after);
            if hits > 0 {
                per_file.push((shown.clone(), lines, hits));
            }
        }
    }

    let mut out = Vec::new();
    let mut truncated = false;
    for (shown, lines, hits) in per_file {
        match mode {
            "files" => out.push(shown),
            "count" => out.push(format!("{shown}:{hits}")),
            _ => {
                if (before > 0 || after > 0) && !out.is_empty() {
                    out.push("--".to_string());
                }
                for l in lines {
                    out.push(l);
                    if out.len() >= cap {
                        truncated = true;
                        break;
                    }
                }
            }
        }
        if truncated || (mode != "content" && out.len() >= cap) {
            truncated = mode != "content" || truncated;
            if out.len() >= cap {
                out.truncate(cap);
                truncated = true;
            }
            break;
        }
    }
    if out.is_empty() {
        return Ok("nenhuma ocorrência".into());
    }
    if truncated {
        out.push(format!("[parado em {cap} resultados]"));
    }
    Ok(out.join("\n"))
}

fn scan_file(
    shown: &str,
    text: &str,
    re: &regex::Regex,
    mode: &str,
    before: usize,
    after: usize,
) -> (Vec<String>, usize) {
    if before == 0 && after == 0 {
        let mut out = Vec::new();
        let mut hits = 0usize;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits += 1;
                if mode == "content" {
                    out.push(format!("{shown}:{}: {}", i + 1, line.trim_end()));
                } else if mode == "files" {
                    return (out, 1);
                }
            }
        }
        return (out, hits);
    }

    let all: Vec<&str> = text.lines().collect();
    let mut matched = vec![false; all.len()];
    let mut hits = 0usize;
    for (i, line) in all.iter().enumerate() {
        if re.is_match(line) {
            hits += 1;
            matched[i] = true;
            if mode == "files" {
                return (Vec::new(), 1);
            }
        }
    }
    if hits == 0 || mode != "content" {
        return (Vec::new(), hits);
    }
    let mut emit = vec![false; all.len()];
    for (i, &m) in matched.iter().enumerate() {
        if m {
            let lo = i.saturating_sub(before);
            let hi = (i + after).min(all.len().saturating_sub(1));
            for slot in &mut emit[lo..=hi] {
                *slot = true;
            }
        }
    }
    let mut out = Vec::new();
    let mut prev_emitted = None::<usize>;
    for i in 0..all.len() {
        if !emit[i] {
            continue;
        }
        if prev_emitted.is_some_and(|p| p + 1 < i) {
            out.push("--".to_string());
        }
        let sep = if matched[i] { ':' } else { '-' };
        out.push(format!("{shown}{sep}{}{sep} {}", i + 1, all[i].trim_end()));
        prev_emitted = Some(i);
    }
    (out, hits)
}

fn is_mappable_ext(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go", ".c", ".h", ".cpp",
        ".hpp", ".java", ".kt", ".rb", ".sh", ".sql", ".cfg", ".conf", ".rsc", ".ios", ".vrp",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn clean_signature(line: &str) -> String {
    let mut s = line.trim();
    if let Some(idx) = s.find(" {") {
        s = s[..idx].trim_end();
    } else if let Some(idx) = s.strip_suffix('{') {
        s = idx.trim_end();
    }
    let cleaned: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() > 110 {
        let mut trunc: String = cleaned.chars().take(107).collect();
        trunc.push_str("...");
        trunc
    } else {
        cleaned
    }
}

fn is_symbol_line(rel: &str, trimmed: &str) -> bool {
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('!')
    {
        return false;
    }
    let lower_path = rel.to_ascii_lowercase();
    if lower_path.ends_with(".cfg")
        || lower_path.ends_with(".conf")
        || lower_path.ends_with(".rsc")
        || lower_path.ends_with(".ios")
        || lower_path.ends_with(".vrp")
    {
        let low = trimmed.to_ascii_lowercase();
        return [
            "hostname ",
            "sysname ",
            "interface ",
            "router bgp",
            "router ospf",
            "router isis",
            "bgp ",
            "ospf ",
            "ip vrf ",
            "ip vpn-instance ",
            "vrf definition ",
            "/interface",
            "/routing",
            "/ip firewall",
        ]
        .iter()
        .any(|p| low.starts_with(p));
    }

    const CODE_PREFIXES: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "fn ",
        "async fn ",
        "pub struct ",
        "pub(crate) struct ",
        "struct ",
        "pub enum ",
        "pub(crate) enum ",
        "enum ",
        "pub trait ",
        "trait ",
        "impl ",
        "impl<",
        "pub mod ",
        "pub type ",
        "export function ",
        "export async function ",
        "export default function ",
        "function ",
        "export class ",
        "class ",
        "export interface ",
        "interface ",
        "export type ",
        "export const ",
        "def ",
        "async def ",
        "func ",
        "type ",
        "CREATE TABLE ",
        "create table ",
    ];
    CODE_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

pub(crate) fn extract_file_symbols(
    rel: &str,
    text: &str,
    max_per_file: usize,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        if indent > 4 {
            continue;
        }
        let trimmed = raw.trim();
        if is_symbol_line(rel, trimmed) {
            out.push((idx + 1, clean_signature(trimmed)));
            if out.len() >= max_per_file {
                break;
            }
        }
    }
    out
}

pub(super) fn repo_map(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let filter = args["glob"].as_str();
    let max_chars = args["max_chars"]
        .as_u64()
        .map_or(6_000usize, |n| (n as usize).clamp(500, 24_000));
    let max_per_file = args["max_symbols_per_file"]
        .as_u64()
        .map_or(25usize, |n| (n as usize).clamp(3, 100));

    let mut files = Vec::new();
    for item in walk(&base, exec) {
        let (rel, entry) = item?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(f) = filter {
            if !glob_match(f, &rel, true) && !glob_match(f, &name, true) {
                continue;
            }
        } else if !is_mappable_ext(&rel) {
            continue;
        }
        let shown = if base.is_file() { name } else { rel };
        files.push((shown, entry.into_path()));
    }

    if files.is_empty() {
        return Ok("nenhum arquivo de código ou configuração encontrado para mapear".into());
    }

    let mut out = String::new();
    let mut mapped_files = 0usize;
    let mut total_symbols = 0usize;
    let mut truncated = false;

    for (shown, path) in &files {
        exec.check()?;
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if meta.len() > 1_048_576 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let symbols = extract_file_symbols(shown, &text, max_per_file);
        let mut block = String::new();
        block.push_str(shown);
        block.push('\n');
        for (line_no, sig) in &symbols {
            block.push_str(&format!("  L{line_no}: {sig}\n"));
        }
        if out.len() + block.len() > max_chars && !out.is_empty() {
            truncated = true;
            break;
        }
        out.push_str(&block);
        mapped_files += 1;
        total_symbols += symbols.len();
    }

    if truncated {
        out.push_str(&format!(
            "[repo_map truncado em {max_chars} caracteres: {mapped_files}/{} arquivos, {total_symbols} símbolos — use path ou glob para focar]",
            files.len()
        ));
    } else {
        out.push_str(&format!(
            "[repo_map: {mapped_files} arquivo(s), {total_symbols} símbolo(s)]"
        ));
    }
    Ok(out)
}
