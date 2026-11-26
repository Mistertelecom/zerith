//! Read-only search tools: `list_dir`, `glob` and `grep` (ripgrep when
//! available, a parallel gitignore-aware walker otherwise).

use std::io::{BufRead, BufReader};
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
const MAX_GREP_FILE_BYTES: u64 = 4 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

fn read_searchable_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_GREP_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

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
    exec: &Exec<'_>,
) -> Option<Result<String>> {
    if !rg_available() || base.is_file() {
        return None;
    }
    let mut cmd = Command::new("rg");
    cmd.current_dir(base)
        .arg("--color")
        .arg("never")
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
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let mut reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    let mut truncated = false;
    let mut raw_line = String::new();
    loop {
        if let Err(e) = exec.check() {
            let _ = child.kill();
            let _ = child.wait();
            return Some(Err(e));
        }
        raw_line.clear();
        match reader.read_line(&mut raw_line) {
            Ok(0) => break,
            Ok(_) => {
                if lines.len() >= cap {
                    truncated = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                let l = raw_line.trim_end_matches(['\r', '\n']);
                let clean_path = l
                    .strip_prefix("./")
                    .or_else(|| l.strip_prefix(".\\"))
                    .unwrap_or(l);
                if mode == "content" {
                    if clean_path == "--" {
                        lines.push("--".to_string());
                    } else if let Some((file, ln, sep, body)) =
                        parse_rg_content_line_ctx(clean_path, before > 0 || after > 0, Some(base))
                    {
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
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    if !truncated {
        let status = child.wait().ok()?;
        let code = status.code().unwrap_or(2);
        if code == 2 && lines.is_empty() {
            return None;
        }
    }
    if lines.is_empty() {
        return Some(Ok("nenhuma ocorrência".into()));
    }
    if !truncated && mode != "content" {
        lines.sort();
    }
    if truncated {
        lines.push(format!("[parado em {cap} resultados]"));
    }
    Some(Ok(lines.join("\n")))
}

fn has_clean_extension(file: &str) -> bool {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !ext.is_empty()
        && ext.len() <= 10
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn pick_best_rg_candidate(line: &str, candidates: &[(usize, usize, u8)]) -> (usize, usize, u8) {
    let mut best = candidates[0];
    let mut best_score = (false, false, false, false, false, false);
    for (idx, &(i, j, sep)) in candidates.iter().enumerate() {
        let file = &line[..i];
        let body = &line[j + 1..];
        let head_end = body
            .find(char::is_whitespace)
            .map_or(line.len(), |off| j + 1 + off);
        let head = &line[j + 1..head_end];
        let has_later_candidate = candidates.iter().any(|&(i2, _, _)| i2 > j);
        let has_slash_before_later = has_later_candidate && head.contains(['/', '\\']);
        let no_earlier_ext_match = !candidates[..idx].iter().any(|&(i0, _, sep0)| {
            (sep0 == b':' || sep0 == sep) && has_clean_extension(&line[..i0])
        });
        let no_candidate_in_head = !candidates.iter().any(|&(i2, _, _)| i2 > j && i2 < head_end);
        let score = (
            !file.contains(char::is_whitespace),
            !has_slash_before_later,
            no_earlier_ext_match,
            has_clean_extension(file),
            sep == b':',
            no_candidate_in_head,
        );
        if idx == 0 || score > best_score {
            best = (i, j, sep);
            best_score = score;
        }
    }
    best
}

#[cfg(test)]
fn parse_rg_content_line(line: &str) -> Option<(String, &str, char, &str)> {
    parse_rg_content_line_ctx(line, true, None)
}

fn parse_rg_content_line_ctx<'a>(
    line: &'a str,
    allow_context: bool,
    base: Option<&Path>,
) -> Option<(String, &'a str, char, &'a str)> {
    let bytes = line.as_bytes();
    let mut candidates: Vec<(usize, usize, u8)> = Vec::new();
    for i in 1..bytes.len() {
        let sep = bytes[i];
        if sep != b':' && !(allow_context && sep == b'-') {
            continue;
        }
        // Windows drive letter (`C:\...` / `C:/...`).
        if sep == b':' && i == 1 && bytes[0].is_ascii_alphabetic() {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 && j < bytes.len() && bytes[j] == sep {
            let ln = &line[i + 1..j];
            // ripgrep line numbers are 1-based and never have leading zeros (`01`).
            if !ln.starts_with('0') {
                candidates.push((i, j, sep));
            }
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let (i, j, sep) = if candidates.len() == 1 {
        candidates[0]
    } else if let Some(dir) = base {
        candidates
            .iter()
            .copied()
            .find(|&(i, _, _)| dir.join(&line[..i]).is_file())
            .unwrap_or_else(|| pick_best_rg_candidate(line, &candidates))
    } else {
        pick_best_rg_candidate(line, &candidates)
    };
    let file = line[..i].replace('\\', "/");
    let ln = &line[i + 1..j];
    let body = line[j + 1..].trim_end();
    Some((file, ln, sep as char, body))
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
        exec,
    ) {
        return fast;
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
                            if let Some(text) = read_searchable_text(path) {
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
            let Some(text) = read_searchable_text(path) else {
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
        ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".java", ".kt", ".rb", ".sh", ".sql", ".cfg",
        ".conf", ".rsc", ".ios", ".vrp", ".junos", ".yml", ".yaml",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn is_c_cpp_ext(lower_path: &str) -> bool {
    [".c", ".h", ".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx"]
        .iter()
        .any(|ext| lower_path.ends_with(ext))
}

fn is_c_cpp_symbol(trimmed: &str) -> bool {
    const CC_PREFIXES: &[&str] = &[
        "class ",
        "struct ",
        "enum ",
        "union ",
        "namespace ",
        "typedef ",
        "using ",
        "template<",
        "template <",
    ];
    if CC_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        return true;
    }
    let Some((before_paren, _)) = trimmed.split_once('(') else {
        return false;
    };
    if before_paren.contains('=') || before_paren.contains(';') {
        return false;
    }
    let ends_like_fn = trimmed.ends_with('{')
        || trimmed.ends_with(')')
        || trimmed.ends_with(';')
        || trimmed.ends_with("const")
        || trimmed.ends_with("override")
        || trimmed.ends_with("noexcept")
        || trimmed.ends_with("final");
    if !ends_like_fn {
        return false;
    }
    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    const CONTROL_KW: &[&str] = &[
        "if", "for", "while", "switch", "return", "else", "do", "case", "catch", "delete", "throw",
        "goto", "sizeof", "assert",
    ];
    if CONTROL_KW.contains(&tokens[0]) {
        return false;
    }
    tokens.len() >= 2 || before_paren.contains("::")
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

fn is_yaml_symbol(trimmed: &str, indent: usize) -> bool {
    if trimmed == "---" || trimmed == "..." {
        return false;
    }
    let low = trimmed.to_ascii_lowercase();
    if indent <= 6 && low.starts_with("- name:") {
        return true;
    }
    if indent <= 4 && (low.starts_with("hosts:") || low.starts_with("- hosts:")) {
        return true;
    }
    if indent == 0 && !trimmed.starts_with('-') {
        if let Some((key, _)) = trimmed.split_once(':') {
            let k = key.trim();
            return !k.is_empty()
                && k.as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                && !k.contains(' ');
        }
    }
    false
}

fn is_symbol_line(rel: &str, trimmed: &str, indent: usize) -> bool {
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
    if lower_path.ends_with(".yml") || lower_path.ends_with(".yaml") {
        return is_yaml_symbol(trimmed, indent);
    }
    if indent > 4 {
        return false;
    }
    if lower_path.ends_with(".rsc") {
        let low = trimmed.to_ascii_lowercase();
        let is_menu = trimmed.starts_with('/')
            && trimmed
                .as_bytes()
                .get(1)
                .is_some_and(u8::is_ascii_alphabetic);
        return is_menu || low.starts_with(":global ") || low.starts_with(":local ");
    }
    if lower_path.ends_with(".cfg")
        || lower_path.ends_with(".conf")
        || lower_path.ends_with(".ios")
        || lower_path.ends_with(".vrp")
        || lower_path.ends_with(".junos")
    {
        let low = trimmed.to_ascii_lowercase();
        return [
            "hostname ",
            "sysname ",
            "set system host-name ",
            "interface ",
            "router bgp",
            "router ospf",
            "router ospf6",
            "router isis",
            "router rip",
            "router static",
            "bgp ",
            "ospf ",
            "isis ",
            "route-map ",
            "route-policy ",
            "policy-map ",
            "class-map ",
            "ip access-list ",
            "ipv6 access-list ",
            "access-list ",
            "acl ",
            "ip prefix-list ",
            "ipv6 prefix-list ",
            "ip community-list ",
            "ip as-path ",
            "ip vrf ",
            "ip vpn-instance ",
            "vrf definition ",
            "vrf ",
            "vlan ",
            "bridge-domain ",
            "mpls ",
            "l2vpn ",
            "evpn ",
            "interfaces {",
            "protocols {",
            "routing-options {",
            "policy-options {",
            "routing-instances {",
        ]
        .iter()
        .any(|p| low.starts_with(p));
    }
    if is_c_cpp_ext(&lower_path) {
        return is_c_cpp_symbol(trimmed);
    }

    const CODE_PREFIXES: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub unsafe fn ",
        "pub(crate) fn ",
        "pub(crate) async fn ",
        "pub(crate) const fn ",
        "pub(super) fn ",
        "fn ",
        "async fn ",
        "const fn ",
        "unsafe fn ",
        "pub struct ",
        "pub(crate) struct ",
        "pub(super) struct ",
        "struct ",
        "pub enum ",
        "pub(crate) enum ",
        "pub(super) enum ",
        "enum ",
        "pub trait ",
        "pub(crate) trait ",
        "trait ",
        "impl ",
        "impl<",
        "unsafe impl ",
        "pub mod ",
        "pub(crate) mod ",
        "pub type ",
        "pub(crate) type ",
        "macro_rules! ",
        "export function ",
        "export async function ",
        "export default function ",
        "export default async function ",
        "function ",
        "async function ",
        "export class ",
        "export default class ",
        "export abstract class ",
        "abstract class ",
        "class ",
        "export interface ",
        "interface ",
        "export type ",
        "export enum ",
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
        let trimmed = raw.trim();
        if is_symbol_line(rel, trimmed, indent) {
            out.push((idx + 1, clean_signature(trimmed)));
            if out.len() >= max_per_file {
                break;
            }
        }
    }
    out
}

fn norm_rel_path(s: &str) -> String {
    s.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_ascii_lowercase()
}

fn is_entrypoint_file(norm_path: &str) -> bool {
    let name = norm_path.rsplit('/').next().unwrap_or(norm_path);
    matches!(
        name,
        "lib.rs"
            | "main.rs"
            | "mod.rs"
            | "index.ts"
            | "index.tsx"
            | "index.js"
            | "index.jsx"
            | "app.tsx"
            | "app.ts"
            | "__init__.py"
            | "main.py"
            | "main.go"
            | "main.c"
            | "main.cpp"
    )
}

/// Sort key for `repo_map` `focus` proximity ranking (smaller sorts first):
/// same file > same directory > shared prefix segments (more first) >
/// entrypoint files (`lib.rs`, `main.rs`, `index.ts`, ...) > alphabetical.
fn focus_rank_key(shown: &str, focus_norm: &str) -> (u8, usize, u8, String) {
    let p = norm_rel_path(shown);
    let p_segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    let f_segs: Vec<&str> = focus_norm.split('/').filter(|s| !s.is_empty()).collect();
    let p_name = p_segs.last().copied().unwrap_or("");
    let p_stem = p_name.split('.').next().unwrap_or(p_name);
    let p_dir = &p_segs[..p_segs.len().saturating_sub(1)];
    let f_dir = if f_segs.len() > 1 {
        &f_segs[..f_segs.len() - 1]
    } else {
        &[]
    };
    let shared_prefix = p_segs
        .iter()
        .zip(f_segs.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let has_slash = focus_norm.contains('/');

    let same_file = p == focus_norm
        || p_name == focus_norm
        || (!has_slash && p_stem == focus_norm)
        || (has_slash && p.ends_with(&format!("/{focus_norm}")));
    let same_dir = (!f_dir.is_empty() && p_dir == f_dir)
        || (!f_segs.is_empty() && p_dir == f_segs)
        || (!has_slash
            && (p_name.contains(focus_norm) || p_dir.iter().any(|seg| seg.contains(focus_norm))));

    let tier = if same_file {
        0
    } else if same_dir {
        1
    } else if shared_prefix > 0 || (has_slash && p.contains(focus_norm)) {
        2
    } else {
        3
    };
    let entrypoint_rank = if is_entrypoint_file(&p) { 0 } else { 1 };
    (tier, usize::MAX - shared_prefix, entrypoint_rank, p)
}

pub(super) fn repo_map(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let filter = args["glob"].as_str();
    let focus = args["focus"]
        .as_str()
        .map(norm_rel_path)
        .filter(|s| !s.is_empty());
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

    if let Some(focus_norm) = focus.as_deref() {
        files.sort_by_cached_key(|(shown, _)| focus_rank_key(shown, focus_norm));
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
            "[repo_map truncado em {max_chars} caracteres: {mapped_files}/{} arquivos, {total_symbols} símbolos — use path, glob ou focus para focar]",
            files.len()
        ));
    } else {
        out.push_str(&format!(
            "[repo_map: {mapped_files} arquivo(s), {total_symbols} símbolo(s)]"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rg_content_line_handles_digits_in_filenames_context_and_windows_paths() {
        assert_eq!(
            parse_rg_content_line("router-01-core.cfg:12:set system host-name r1"),
            Some((
                "router-01-core.cfg".to_string(),
                "12",
                ':',
                "set system host-name r1"
            ))
        );
        assert_eq!(
            parse_rg_content_line("router-12-core.cfg:12:set port 1:2:3"),
            Some((
                "router-12-core.cfg".to_string(),
                "12",
                ':',
                "set port 1:2:3"
            ))
        );
        assert_eq!(
            parse_rg_content_line("foo-2024-01.log-15-context line"),
            Some(("foo-2024-01.log".to_string(), "15", '-', "context line"))
        );
        assert_eq!(
            parse_rg_content_line("foo-2024-01-bar-15-context line"),
            Some(("foo-2024-01-bar".to_string(), "15", '-', "context line"))
        );
        assert_eq!(
            parse_rg_content_line(r"C:\repo\router-01-core.cfg:12:line"),
            Some(("C:/repo/router-01-core.cfg".to_string(), "12", ':', "line"))
        );
        assert_eq!(
            parse_rg_content_line(r"C:\repo\dir-12-prod\foo-2024-01.log-15-context"),
            Some((
                "C:/repo/dir-12-prod/foo-2024-01.log".to_string(),
                "15",
                '-',
                "context"
            ))
        );
        assert_eq!(
            parse_rg_content_line("dir:12:sub/file.txt:7:hello:8:world"),
            Some(("dir:12:sub/file.txt".to_string(), "7", ':', "hello:8:world"))
        );
        assert_eq!(
            parse_rg_content_line("src/a.rs-9-see b.rs:4:call()"),
            Some(("src/a.rs".to_string(), "9", '-', "see b.rs:4:call()"))
        );
        assert_eq!(
            parse_rg_content_line_ctx("a-12-b.txt:3:x-9-y", false, None),
            Some(("a-12-b.txt".to_string(), "3", ':', "x-9-y"))
        );
    }
}
