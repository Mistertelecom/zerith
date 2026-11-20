//! Token Saver: prompt-level compression (Ponytail + Caveman) and
//! deterministic tool-output filtering (RTK).

use std::collections::HashMap;

use zerith_core::{CavemanLevel, PonytailLevel};

/// Minimum input size (bytes) before RTK attempts to compress tool output.
const RTK_MIN_BYTES: usize = 500;
/// Maximum input size (bytes) RTK will inspect; larger payloads pass through.
const RTK_MAX_BYTES: usize = 10_000_000;

/// Result of running [`compress_tool_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtkOutcome {
    pub text: String,
    pub saved_bytes: usize,
    pub saved_tokens: u64,
    pub filter: Option<&'static str>,
}

/// Static Ponytail engineering-economy instructions appended to the system prompt.
pub fn ponytail_prompt(level: PonytailLevel) -> Option<&'static str> {
    match level {
        PonytailLevel::Off => None,
        PonytailLevel::Lite => Some(
            "[Token Saver: Ponytail Lite]\n\
             Act like a lazy senior dev (YAGNI, minimum code). Follow the ladder: \
             1) delete or skip unnecessary work, 2) reuse existing code, \
             3) stdlib and native features before abstractions or dependencies, \
             4) minimum code that solves what was asked. \
             Keep security, validation, error handling, and boundary checks intact. \
             Leave brief `ponytail:` comments where an obvious generalization was intentionally skipped. \
             Lite: build what's asked, but name the lazier alternative in one line. User picks.",
        ),
        PonytailLevel::Full => Some(
            "[Token Saver: Ponytail Full]\n\
             Act like a lazy senior dev (YAGNI, minimum code). Follow the ladder: \
             1) delete or skip unnecessary work, 2) reuse existing code, \
             3) stdlib and native first, 4) minimum diff that solves what was asked. \
             Keep security, validation, error handling, and boundary checks intact. \
             Mark skipped generalizations with a brief `ponytail:` comment. \
             Full: the ladder enforced. Stdlib and native first. Shortest diff, shortest explanation. \
             Code first, then at most three short lines: `[code] -> skipped: [X], add when [Y].`",
        ),
        PonytailLevel::Ultra => Some(
            "[Token Saver: Ponytail Ultra]\n\
             Ultra: YAGNI extremist. Deletion before addition. Stdlib/native only when code is truly required. \
             Keep security, validation, and boundary checks intact. \
             Ship the one-liner and challenge the rest of the requirement in the same response.",
        ),
    }
}

/// Static Caveman response-compression instructions appended to the system prompt.
pub fn caveman_prompt(level: CavemanLevel) -> Option<&'static str> {
    match level {
        CavemanLevel::Off => None,
        CavemanLevel::Lite => Some(
            "[Token Saver: Caveman Lite]\n\
             Respond with terse grammar. Drop filler, hedging, and pleasantries. \
             Keep code, file paths, commands, identifiers, error messages, and URLs verbatim. \
             Preserve the user's language. \
             Auto-Clarity: switch to clear full prose for security warnings, irreversible actions, or critical confirmations.",
        ),
        CavemanLevel::Full => Some(
            "[Token Saver: Caveman Full]\n\
             Terse caveman style. Drop articles (a/an/the), filler, pleasantries, and hedging. Fragments OK. \
             Pattern: `[thing] [action] [reason]. [next step].` \
             Keep code, file paths, commands, identifiers, error messages, and URLs verbatim. \
             Preserve the user's language. \
             Auto-Clarity: use full clear prose for security warnings and irreversible actions.",
        ),
        CavemanLevel::Ultra => Some(
            "[Token Saver: Caveman Ultra]\n\
             Telegraphic maximum compression. Strip articles, filler, pleasantries, hedging, and conjunctions. One word when enough. \
             Keep code, file paths, commands, identifiers, error messages, and URLs verbatim. \
             Preserve the user's language. \
             Auto-Clarity: use full clear prose for security warnings and irreversible actions.",
        ),
        CavemanLevel::Wenyan => Some(
            "[Token Saver: Caveman Wenyan]\n\
             Use Classical Chinese (文言文) maximum compression for explanatory prose: terse archaic verbs and particles. \
             Keep all code, identifiers, file paths, commands, error messages, and URLs in original form verbatim. \
             Use clear plain text for security warnings or irreversible actions.",
        ),
    }
}

/// Splits `text` into `(prose_bytes, code_bytes)` by toggling on ```` ``` ````
/// fenced code blocks. `prose_bytes + code_bytes == text.len()`.
fn split_prose_and_code_bytes(text: &str) -> (usize, usize) {
    let mut prose_bytes = 0usize;
    let mut code_bytes = 0usize;
    for (idx, chunk) in text.split("```").enumerate() {
        if idx == 0 {
            prose_bytes += chunk.len();
        } else if idx % 2 == 1 {
            code_bytes += 3 + chunk.len();
        } else {
            code_bytes += 3;
            prose_bytes += chunk.len();
        }
    }
    (prose_bytes, code_bytes)
}

/// Estimates output tokens saved by Caveman prose compression for one step.
pub fn estimate_caveman_saved_tokens(
    level: CavemanLevel,
    text: &str,
    completion_tokens: u64,
) -> u64 {
    if level == CavemanLevel::Off {
        return 0;
    }
    let (prose_bytes, code_bytes) = split_prose_and_code_bytes(text);
    if prose_bytes == 0 && text.is_empty() && completion_tokens == 0 {
        return 0;
    }
    let total_bytes = prose_bytes + code_bytes;
    let prose_tokens = if completion_tokens > 0 && total_bytes > 0 {
        let min_tok = if prose_bytes > 0 { 1 } else { 0 };
        (completion_tokens.saturating_mul(prose_bytes as u64) / total_bytes as u64).max(min_tok)
    } else {
        prose_bytes.div_ceil(4) as u64
    };
    match level {
        CavemanLevel::Off => 0,
        CavemanLevel::Lite => prose_tokens.saturating_mul(33) / 100,
        CavemanLevel::Full => prose_tokens.saturating_mul(82) / 100,
        CavemanLevel::Ultra => prose_tokens.saturating_mul(150) / 100,
        CavemanLevel::Wenyan => prose_tokens.saturating_mul(233) / 100,
    }
}

/// Estimates output tokens saved by Ponytail code/diff minimalism for one step.
pub fn estimate_ponytail_saved_tokens(
    level: PonytailLevel,
    text: &str,
    tool_args_bytes: usize,
    completion_tokens: u64,
) -> u64 {
    if level == PonytailLevel::Off {
        return 0;
    }
    let (_prose_bytes, code_bytes) = split_prose_and_code_bytes(text);
    let total_bytes = text.len() + tool_args_bytes;
    let code_plus_args_bytes = code_bytes + tool_args_bytes;
    let effective_tokens = if completion_tokens > 0 {
        completion_tokens
    } else {
        total_bytes.div_ceil(4) as u64
    };
    let base = if code_plus_args_bytes > 0 {
        (effective_tokens.saturating_mul(code_plus_args_bytes as u64) / total_bytes.max(1) as u64)
            .max(1)
    } else {
        effective_tokens / 2
    };
    match level {
        PonytailLevel::Off => 0,
        PonytailLevel::Lite => base.saturating_mul(25) / 100,
        PonytailLevel::Full => base.saturating_mul(54) / 100,
        PonytailLevel::Ultra => base,
    }
}

fn unchanged(text: &str) -> RtkOutcome {
    RtkOutcome {
        text: text.to_string(),
        saved_bytes: 0,
        saved_tokens: 0,
        filter: None,
    }
}

fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Returns complete lines in `head` (drops a trailing partial line when `head`
/// sliced mid-line inside a larger payload).
fn head_complete_lines(head: &str, is_full_text: bool) -> Vec<&str> {
    let slice = if !is_full_text && !head.ends_with('\n') {
        match head.rfind('\n') {
            Some(pos) => &head[..pos],
            None => head,
        }
    } else {
        head
    };
    slice.lines().collect()
}

/// Applies deterministic RTK compression filters to tool outputs before they
/// are persisted or sent back to the model.
pub fn compress_tool_output(tool_name: &str, text: &str) -> RtkOutcome {
    // read_file already has offset/limit: the model asked for those lines,
    // and a cut there makes it invent the missing ones.
    if tool_name == "read_file" || text.len() < RTK_MIN_BYTES || text.len() > RTK_MAX_BYTES {
        return unchanged(text);
    }

    let cut = floor_char_boundary(text, 1024);
    let head = &text[..cut];
    let is_full = cut == text.len();
    let head_lines = head_complete_lines(head, is_full);

    let accept = |candidate: Option<String>, filter: &'static str| -> Option<RtkOutcome> {
        let out = candidate?;
        if out.is_empty() || out.len() >= text.len() {
            return None;
        }
        let saved_bytes = text.len() - out.len();
        let saved_tokens = (saved_bytes / 4) as u64;
        Some(RtkOutcome {
            text: out,
            saved_bytes,
            saved_tokens,
            filter: Some(filter),
        })
    };

    if is_git_log(head) {
        if let Some(res) = accept(filter_git_log(text), "git-log") {
            return res;
        }
    }

    if is_git_diff(head) {
        if let Some(res) = accept(filter_git_diff(text), "git-diff") {
            return res;
        }
    }

    if is_git_status(head, &head_lines) {
        if let Some(res) = accept(filter_git_status(text), "git-status") {
            return res;
        }
    }

    let grep_like = is_grep_output(&head_lines);
    if is_build_output(head, grep_like) {
        if let Some(res) = accept(filter_build_output(text), "build-output") {
            return res;
        }
    }

    if grep_like {
        if let Some(res) = accept(filter_grep(text), "grep") {
            return res;
        }
    }

    if is_tree_output(head) {
        if let Some(res) = accept(filter_tree(text), "tree") {
            return res;
        }
    }

    if is_find_output(&head_lines, text) {
        if let Some(res) = accept(filter_find(text), "find") {
            return res;
        }
    }

    if let Some(res) = accept(filter_dedup_log(text), "dedup-log") {
        return res;
    }

    if let Some(res) = accept(filter_smart_truncate(text), "smart-truncate") {
        return res;
    }

    unchanged(text)
}

// --- git-log ----------------------------------------------------------------

fn is_commit_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("commit ") else {
        return false;
    };
    let hash = rest.split_whitespace().next().unwrap_or("");
    hash.len() >= 7 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_git_log(head: &str) -> bool {
    let has_commit = head.lines().any(is_commit_line);
    has_commit && (head.contains("Author:") || head.contains("Date:"))
}

fn filter_git_log(text: &str) -> Option<String> {
    let mut kept = Vec::new();
    let mut in_diff = false;
    let mut kept_subject = false;

    for line in text.lines() {
        if is_commit_line(line) {
            in_diff = false;
            kept_subject = false;
            kept.push(line);
            continue;
        }
        if line.starts_with("diff --git ") {
            in_diff = true;
            continue;
        }
        if in_diff {
            continue;
        }
        // `--stat` rows (` path | 12 ++-`) are what the caller asked for: keep them
        let stat_row = line.starts_with(' ') && !line.starts_with("    ") && line.contains(" | ");
        if line.starts_with("Author:")
            || line.starts_with("Date:")
            || line.starts_with("Merge:")
            || line.contains("files changed")
            || line.contains("file changed")
            || stat_row
        {
            kept.push(line);
        } else if line.starts_with("    ") && !line.trim().is_empty() && !kept_subject {
            kept.push(line);
            kept_subject = true;
        }
    }

    if kept.is_empty() {
        return None;
    }

    let total = kept.len();
    let mut out = kept.into_iter().take(200).collect::<Vec<_>>().join("\n");
    if total > 200 {
        out.push_str(&format!("\n... (+{} lines omitted)", total - 200));
    }
    Some(out)
}

// --- git-diff ---------------------------------------------------------------

fn is_git_diff(head: &str) -> bool {
    head.contains("diff --git ") || head.lines().any(|l| l.starts_with("@@ "))
}

struct DiffFile<'a> {
    header: Option<&'a str>,
    meta: Vec<&'a str>,
    hunks: Vec<&'a str>,
    added: usize,
    removed: usize,
}

fn filter_git_diff(text: &str) -> Option<String> {
    let mut files: Vec<DiffFile<'_>> = Vec::new();

    for line in text.lines() {
        if line.starts_with("diff --git ") {
            files.push(DiffFile {
                header: Some(line),
                meta: Vec::new(),
                hunks: Vec::new(),
                added: 0,
                removed: 0,
            });
            continue;
        }
        if files.is_empty() {
            files.push(DiffFile {
                header: None,
                meta: Vec::new(),
                hunks: Vec::new(),
                added: 0,
                removed: 0,
            });
        }
        let cur = files.last_mut().expect("non-empty diff files");
        if cur.hunks.is_empty()
            && (line.starts_with("index ")
                || line.starts_with("old mode ")
                || line.starts_with("new mode ")
                || line.starts_with("similarity index ")
                || line.starts_with("dissimilarity index "))
        {
            continue;
        }
        if cur.hunks.is_empty()
            && (line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("rename ")
                || line.starts_with("copy "))
        {
            cur.meta.push(line);
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            cur.added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            cur.removed += 1;
        }
        cur.hunks.push(line);
    }

    if files.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for f in files {
        match f.header {
            Some(h) => out.push(format!("{h} (+{} -{})", f.added, f.removed)),
            None => out.push(format!("diff (+{} -{})", f.added, f.removed)),
        }
        for m in f.meta {
            out.push(m.to_string());
        }
        for h in f.hunks.iter().take(100) {
            out.push((*h).to_string());
        }
        if f.hunks.len() > 100 {
            out.push(format!("... (+{} hunk lines omitted)", f.hunks.len() - 100));
        }
    }

    Some(out.join("\n"))
}

// --- git-status -------------------------------------------------------------

fn is_porcelain_line(line: &str) -> bool {
    let mut chars = line.chars();
    let (Some(x), Some(y), Some(sp)) = (chars.next(), chars.next(), chars.next()) else {
        return false;
    };
    const VALID: &str = " MADRCU?!";
    sp == ' ' && VALID.contains(x) && VALID.contains(y) && (x != ' ' || y != ' ')
}

fn is_git_status(head: &str, head_lines: &[&str]) -> bool {
    if head.contains("On branch ")
        || head.contains("nothing to commit")
        || head.contains("Changes ")
        || head.contains("Untracked files:")
    {
        return true;
    }
    let mut non_empty = 0usize;
    let mut porcelain = 0usize;
    for line in head_lines {
        if line.trim().is_empty() || line.starts_with("## ") {
            continue;
        }
        non_empty += 1;
        if is_porcelain_line(line) {
            porcelain += 1;
        }
    }
    non_empty >= 2 && porcelain * 10 >= non_empty * 6
}

fn filter_git_status(text: &str) -> Option<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        None,
        Staged,
        Modified,
        Untracked,
    }

    let mut branch: Option<&str> = None;
    let mut clean_note: Option<&str> = None;
    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    let mut sec = Section::None;

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if line.starts_with("On branch ") || line.starts_with("## ") {
            branch = Some(line);
            sec = Section::None;
            continue;
        }
        if line.starts_with("nothing to commit") {
            clean_note = Some(line);
            sec = Section::None;
            continue;
        }
        if line.starts_with("Changes to be committed:") {
            sec = Section::Staged;
            continue;
        }
        if line.starts_with("Changes not staged for commit:") || line.starts_with("Unmerged paths:")
        {
            sec = Section::Modified;
            continue;
        }
        if line.starts_with("Untracked files:") {
            sec = Section::Untracked;
            continue;
        }
        if sec != Section::None && (line.starts_with('\t') || line.starts_with("  ")) {
            if t.starts_with('(') {
                continue;
            }
            match sec {
                Section::Staged => staged.push(t.to_string()),
                Section::Modified => modified.push(t.to_string()),
                Section::Untracked => untracked.push(t.to_string()),
                Section::None => {}
            }
            continue;
        }
        if is_porcelain_line(line) {
            sec = Section::None;
            let mut chars = line.chars();
            let x = chars.next().unwrap_or(' ');
            let y = chars.next().unwrap_or(' ');
            let _ = chars.next();
            let path = chars.as_str().trim();
            if x == '?' && y == '?' {
                untracked.push(path.to_string());
            } else {
                if x != ' ' {
                    staged.push(format!("{x} {path}"));
                }
                if y != ' ' {
                    modified.push(format!("{y} {path}"));
                }
            }
        }
    }

    if branch.is_none()
        && clean_note.is_none()
        && staged.is_empty()
        && modified.is_empty()
        && untracked.is_empty()
    {
        return None;
    }

    let mut out = Vec::new();
    if let Some(b) = branch {
        out.push(b.to_string());
    }
    for (label, items) in [
        ("+ Staged", &staged),
        ("~ Modified", &modified),
        ("? Untracked", &untracked),
    ] {
        if items.is_empty() {
            continue;
        }
        out.push(format!("{label} ({}):", items.len()));
        for item in items.iter().take(10) {
            out.push(format!("  {item}"));
        }
        if items.len() > 10 {
            out.push(format!("  ... (+{} more)", items.len() - 10));
        }
    }
    if staged.is_empty() && modified.is_empty() && untracked.is_empty() {
        if let Some(note) = clean_note {
            out.push(note.to_string());
        }
    }

    Some(out.join("\n"))
}

// --- build-output -----------------------------------------------------------

fn is_build_output(head: &str, grep_like: bool) -> bool {
    head.contains("Compiling ")
        || head.contains("Downloading ")
        || head.contains("Fetching ")
        || head.contains("error[")
        || head.contains("npm ERR!")
        || head.contains("BUILD FAILED")
        || head.contains("BUILD SUCCESS")
        || (!grep_like && (head.contains("error:") || head.contains("ERROR:")))
}

fn is_numbered_pipe_line(trimmed: &str) -> bool {
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let rest = trimmed[digits..].trim_start();
    rest.starts_with('|')
}

fn filter_build_output(text: &str) -> Option<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DiagState {
        None,
        Error,
        Warning,
        SkipWarning,
    }

    let mut compiled = 0usize;
    let mut downloaded = 0usize;
    let mut warnings_seen = 0usize;
    let mut warnings: Vec<&str> = Vec::new();
    let mut errors: Vec<&str> = Vec::new();
    let mut summaries: Vec<&str> = Vec::new();
    let mut state = DiagState::None;

    for line in text.lines() {
        let t = line.trim_start();

        if t.starts_with("Compiling ") {
            compiled += 1;
            state = DiagState::None;
            continue;
        }
        if t.starts_with("Downloading ")
            || t.starts_with("Downloaded ")
            || t.starts_with("Fetching ")
        {
            downloaded += 1;
            state = DiagState::None;
            continue;
        }
        if t.starts_with("Fresh ")
            || t.starts_with("Checking ")
            || t.starts_with("Blocking ")
            || t.starts_with("Updating ")
            || t.starts_with("Locking ")
        {
            state = DiagState::None;
            continue;
        }

        if t.starts_with("warning:") && (t.contains("generated ") || t.contains("emitted")) {
            summaries.push(line);
            state = DiagState::None;
            continue;
        }
        if t.starts_with("Finished ")
            || t.starts_with("test result:")
            || t.starts_with("BUILD FAILED")
            || t.starts_with("BUILD SUCCESS")
        {
            summaries.push(line);
            state = DiagState::None;
            continue;
        }

        if t.starts_with("error[")
            || t.starts_with("error:")
            || t.starts_with("ERROR:")
            || t.starts_with("npm ERR!")
            || t.starts_with("FAILED:")
            || t.contains("BUILD FAILED")
        {
            state = DiagState::Error;
            errors.push(line);
            continue;
        }

        if t.starts_with("warning:") || t.starts_with("warning[") || t.starts_with("WARNING:") {
            warnings_seen += 1;
            if warnings_seen <= 5 {
                state = DiagState::Warning;
                warnings.push(line);
            } else {
                state = DiagState::SkipWarning;
            }
            continue;
        }

        let is_ctx = t.starts_with("-->")
            || t.starts_with('|')
            || t.starts_with("= ")
            || is_numbered_pipe_line(t);

        if is_ctx {
            match state {
                DiagState::Error | DiagState::None => errors.push(line),
                DiagState::Warning => warnings.push(line),
                DiagState::SkipWarning => {}
            }
        } else if t.is_empty() {
            if state == DiagState::Error && errors.last().is_some_and(|l| !l.trim().is_empty()) {
                errors.push(line);
            }
            state = DiagState::None;
        } else if state == DiagState::Error {
            errors.push(line);
        } else if state == DiagState::Warning {
            warnings.push(line);
        }
    }

    let mut out: Vec<String> = Vec::new();
    if compiled > 0 {
        out.push(format!("Compiled {compiled} packages"));
    }
    if downloaded > 0 {
        out.push(format!("Downloaded {downloaded} packages"));
    }
    out.extend(warnings.into_iter().map(str::to_string));
    if warnings_seen > 5 {
        out.push(format!("... (+{} warnings omitted)", warnings_seen - 5));
    }
    out.extend(errors.into_iter().map(str::to_string));
    out.extend(summaries.into_iter().map(str::to_string));

    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

// --- grep -------------------------------------------------------------------

fn parse_grep_line(line: &str) -> Option<(&str, &str, &str)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return None;
    }
    let b = line.as_bytes();
    let search_from = if b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
    {
        2
    } else {
        0
    };
    let rel_c1 = line[search_from..].find(':')?;
    let c1 = search_from + rel_c1;
    let file = &line[..c1];
    if file.is_empty()
        || file.contains("://")
        || file.chars().any(char::is_whitespace)
        || file.bytes().all(|ch| ch.is_ascii_digit())
        || (file.len() >= 10
            && file.as_bytes()[..4].iter().all(u8::is_ascii_digit)
            && file.as_bytes().get(10) == Some(&b'T'))
    {
        return None;
    }
    let after_c1 = &line[c1 + 1..];
    let c2 = after_c1.find(':')?;
    let num = &after_c1[..c2];
    if num.is_empty() || num.len() > 7 || !num.bytes().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let content = &after_c1[c2 + 1..];
    Some((file, num, content))
}

fn is_grep_output(head_lines: &[&str]) -> bool {
    head_lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(5)
        .any(|l| parse_grep_line(l).is_some())
}

fn filter_grep(text: &str) -> Option<String> {
    let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    let mut index_by_file: HashMap<&str, usize> = HashMap::new();

    for line in text.lines() {
        let Some((file, num, content)) = parse_grep_line(line) else {
            continue;
        };
        if let Some(&idx) = index_by_file.get(file) {
            groups[idx].1.push((num, content));
        } else {
            index_by_file.insert(file, groups.len());
            groups.push((file, vec![(num, content)]));
        }
    }

    if groups.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for (file, matches) in groups {
        out.push(format!("[file] {file} ({}):", matches.len()));
        for (num, content) in matches.iter().take(10) {
            out.push(format!("  {num}: {content}"));
        }
        if matches.len() > 10 {
            out.push(format!("  ... (+{} more)", matches.len() - 10));
        }
    }

    Some(out.join("\n"))
}

// --- tree -------------------------------------------------------------------

fn is_tree_output(head: &str) -> bool {
    head.contains("├──") || head.contains("└──") || head.contains("│  ")
}

fn is_tree_summary(line: &str) -> bool {
    let t = line.trim();
    t.chars().next().is_some_and(|c| c.is_ascii_digit())
        && (t.contains("director") || t.contains("file"))
}

fn filter_tree(text: &str) -> Option<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.last().is_some_and(|l| is_tree_summary(l)) {
        lines.pop();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
    }
    if lines.is_empty() {
        return None;
    }
    let total = lines.len();
    let mut out = lines.into_iter().take(200).collect::<Vec<_>>().join("\n");
    if total > 200 {
        out.push_str(&format!("\n... (+{} lines omitted)", total - 200));
    }
    Some(out)
}

// --- find -------------------------------------------------------------------

fn looks_like_path(line: &str) -> bool {
    let s = line.trim();
    if s.is_empty() {
        return false;
    }
    let b = s.as_bytes();
    let without_drive = if b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
    {
        &s[2..]
    } else {
        s
    };
    if without_drive.contains(':') {
        return false;
    }
    s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
        || s.contains('/')
        || s.contains('\\')
}

fn is_find_output(head_lines: &[&str], full_text: &str) -> bool {
    let head_non_empty: Vec<&str> = head_lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if head_non_empty.len() < 3 || !head_non_empty.iter().all(|l| looks_like_path(l)) {
        return false;
    }
    full_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .all(looks_like_path)
}

fn split_dir_file(path: &str) -> (&str, &str) {
    let p = path.trim();
    if let Some((d, f)) = p.rsplit_once('/') {
        (if d.is_empty() { "/" } else { d }, f)
    } else if let Some((d, f)) = p.rsplit_once('\\') {
        (if d.is_empty() { "\\" } else { d }, f)
    } else {
        (".", p)
    }
}

fn filter_find(text: &str) -> Option<String> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut index_by_dir: HashMap<&str, usize> = HashMap::new();

    for raw in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (dir, file) = split_dir_file(raw);
        let name = if file.is_empty() { "." } else { file };
        if let Some(&idx) = index_by_dir.get(dir) {
            groups[idx].1.push(name);
        } else {
            index_by_dir.insert(dir, groups.len());
            groups.push((dir, vec![name]));
        }
    }

    if groups.is_empty() {
        return None;
    }

    let total_dirs = groups.len();
    let mut out = Vec::new();
    for (dir, files) in groups.into_iter().take(20) {
        let dir_label = if dir.ends_with('/') || dir.ends_with('\\') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };
        out.push(format!("{dir_label} ({}):", files.len()));
        for f in files.iter().take(10) {
            out.push(format!("  {f}"));
        }
        if files.len() > 10 {
            out.push(format!("  ... (+{} more)", files.len() - 10));
        }
    }
    if total_dirs > 20 {
        out.push(format!("... (+{} more directories)", total_dirs - 20));
    }

    Some(out.join("\n"))
}

// --- dedup-log --------------------------------------------------------------

fn filter_dedup_log(text: &str) -> Option<String> {
    let non_empty = text.lines().filter(|l| !l.trim().is_empty()).count();
    if non_empty < 5 {
        return None;
    }

    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<&str> = None;
    let mut dup_count: usize = 0;
    let mut any_dedup = false;

    let flush_dups = |out: &mut Vec<String>, dup_count: &mut usize, any_dedup: &mut bool| {
        if *dup_count > 0 {
            *any_dedup = true;
            out.push(format!("  ... ({} duplicate lines)", *dup_count));
            *dup_count = 0;
        }
    };

    for line in text.lines() {
        if prev == Some(line) {
            dup_count += 1;
            continue;
        }
        flush_dups(&mut out, &mut dup_count, &mut any_dedup);
        out.push(line.to_string());
        prev = Some(line);
    }
    flush_dups(&mut out, &mut dup_count, &mut any_dedup);

    let total = out.len();
    if total > 2000 {
        let mut capped: Vec<String> = out.into_iter().take(2000).collect();
        capped.push(format!("... (+{} more lines)", total - 2000));
        return Some(capped.join("\n"));
    }

    if !any_dedup {
        return None;
    }

    Some(out.join("\n"))
}

// --- smart-truncate ---------------------------------------------------------

fn filter_smart_truncate(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 250 {
        return None;
    }
    let omitted = lines.len() - 180;
    let mut out = String::new();
    out.push_str(&lines[..120].join("\n"));
    out.push_str(&format!("\n... (+{omitted} lines truncated)\n"));
    out.push_str(&lines[lines.len() - 60..].join("\n"));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_log_keeps_the_stat_rows_it_was_asked_for() {
        let mut log = String::new();
        for n in 0..6 {
            log.push_str(&format!(
                "commit {n}abcdef1234567\nAuthor: a <a@b>\nDate:   Mon\n\n    subject {n}\n\n    body line\n\n crates/x/App.tsx | 12 ++--\n src/lib.rs | 3 +\n 2 files changed, 10 insertions(+)\n\n"
            ));
        }
        let out = compress_tool_output("shell", &log);
        assert_eq!(out.filter, Some("git-log"));
        assert_eq!(out.text.matches("App.tsx | 12").count(), 6, "{}", out.text);
        assert!(!out.text.contains("body line"));
    }

    #[test]
    fn ponytail_and_caveman_prompts_match_levels() {
        assert_eq!(ponytail_prompt(PonytailLevel::Off), None);
        let pt_lite = ponytail_prompt(PonytailLevel::Lite).unwrap();
        assert!(pt_lite.contains(
            "Lite: build what's asked, but name the lazier alternative in one line. User picks."
        ));
        assert!(pt_lite.contains("ponytail:"));
        let pt_full = ponytail_prompt(PonytailLevel::Full).unwrap();
        assert!(pt_full.contains("Full: the ladder enforced."));
        assert!(pt_full.contains("[code] -> skipped: [X], add when [Y]."));
        let pt_ultra = ponytail_prompt(PonytailLevel::Ultra).unwrap();
        assert!(pt_ultra.contains("Ultra: YAGNI extremist."));

        assert_eq!(caveman_prompt(CavemanLevel::Off), None);
        let cv_lite = caveman_prompt(CavemanLevel::Lite).unwrap();
        assert!(cv_lite.contains("Auto-Clarity"));
        let cv_full = caveman_prompt(CavemanLevel::Full).unwrap();
        assert!(cv_full.contains("[thing] [action] [reason]. [next step]."));
        let cv_ultra = caveman_prompt(CavemanLevel::Ultra).unwrap();
        assert!(cv_ultra.contains("Telegraphic maximum compression"));
        let cv_wenyan = caveman_prompt(CavemanLevel::Wenyan).unwrap();
        assert!(cv_wenyan.contains("文言文"));
    }

    #[test]
    fn small_or_huge_outputs_pass_through_unchanged() {
        let small = "Compiling foo v0.1.0\n".repeat(10);
        assert!(small.len() < 500);
        let out = compress_tool_output("shell", &small);
        assert_eq!(out.filter, None);
        assert_eq!(out.saved_bytes, 0);
        assert_eq!(out.saved_tokens, 0);
        assert_eq!(out.text, small);

        let huge = "a".repeat(10_000_001);
        let out = compress_tool_output("read_file", &huge);
        assert_eq!(out.filter, None);
        assert_eq!(out.saved_bytes, 0);
    }

    #[test]
    fn build_output_preserves_errors_and_caps_warnings() {
        let mut s = String::new();
        for i in 0..25 {
            s.push_str(&format!("   Compiling pkg-{i:02} v0.1.0\n"));
        }
        for i in 0..5 {
            s.push_str(&format!("   Downloading dep-{i:02} v1.0.0\n"));
        }
        for i in 1..=8 {
            s.push_str(&format!(
                "warning: unused variable `v{i}`\n --> src/w{i}.rs:1:5\n  |\n1 | let v{i} = 1;\n  |     ^^^\n\n"
            ));
        }
        s.push_str(
            "error[E0308]: mismatched types\n \
             --> src/main.rs:10:5\n  \
             |\n\
             10 |     let x: u32 = \"oops\";\n  \
             |                  ^^^^^^ expected `u32`, found `&str`\n  \
             = note: cannot coerce\n\
             error: could not compile `app` (bin \"app\") due to 1 previous error\n",
        );

        let out = compress_tool_output("shell", &s);
        assert_eq!(out.filter, Some("build-output"));
        assert!(out.saved_bytes > 0);
        assert_eq!(out.saved_tokens, (out.saved_bytes / 4) as u64);
        assert!(out.text.contains("Compiled 25 packages"), "{}", out.text);
        assert!(out.text.contains("Downloaded 5 packages"), "{}", out.text);
        assert!(out.text.contains("(+3 warnings omitted)"), "{}", out.text);
        assert!(out.text.contains("error[E0308]: mismatched types"));
        assert!(out.text.contains("--> src/main.rs:10:5"));
        assert!(out.text.contains("= note: cannot coerce"));
        assert!(out.text.contains("error: could not compile"));
        assert!(!out.text.contains("Compiling pkg-12"));
        assert!(!out.text.contains("unused variable `v8`"));
    }

    #[test]
    fn git_diff_summarizes_counts_and_caps_hunk_lines() {
        let mut s = String::from("diff --git a/src/lib.rs b/src/lib.rs\nindex 1234..5678 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,140 +1,140 @@\n");
        for i in 0..130 {
            s.push_str(&format!("+added line {i:03}\n"));
        }
        for i in 0..10 {
            s.push_str(&format!("-removed line {i:03}\n"));
        }

        let out = compress_tool_output("shell", &s);
        assert_eq!(out.filter, Some("git-diff"));
        assert!(out.saved_bytes > 0);
        assert!(out.text.contains("(+130 -10)"), "{}", out.text);
        assert!(
            out.text.contains("(+41 hunk lines omitted)"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("index 1234..5678"));
    }

    #[test]
    fn git_status_groups_verbose_and_porcelain() {
        let mut verbose = String::from(
            "On branch main\nYour branch is up to date.\n\nChanges to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n",
        );
        for i in 0..14 {
            verbose.push_str(&format!("\tnew file:   src/staged_{i:02}.rs\n"));
        }
        verbose.push_str("\nChanges not staged for commit:\n");
        for i in 0..3 {
            verbose.push_str(&format!("\tmodified:   src/mod_{i}.rs\n"));
        }
        verbose.push_str("\nUntracked files:\n");
        for i in 0..12 {
            verbose.push_str(&format!("\ttmp/untracked_{i:02}.txt\n"));
        }

        let out = compress_tool_output("shell", &verbose);
        assert_eq!(out.filter, Some("git-status"));
        assert!(out.saved_bytes > 0);
        assert!(out.text.contains("+ Staged (14):"), "{}", out.text);
        assert!(out.text.contains("... (+4 more)"), "{}", out.text);
        assert!(out.text.contains("~ Modified (3):"), "{}", out.text);
        assert!(out.text.contains("? Untracked (12):"), "{}", out.text);
        assert!(out.text.contains("... (+2 more)"), "{}", out.text);

        let mut porcelain = String::new();
        for i in 0..15 {
            porcelain.push_str(&format!("M  src/file_staged_{i:02}_very_long_name.rs\n"));
            porcelain.push_str(&format!("?? extra/untracked_{i:02}_very_long_name.txt\n"));
        }
        let p_out = compress_tool_output("shell", &porcelain);
        assert_eq!(p_out.filter, Some("git-status"));
        assert!(p_out.text.contains("+ Staged (15):"), "{}", p_out.text);
        assert!(p_out.text.contains("? Untracked (15):"), "{}", p_out.text);
    }

    #[test]
    fn grep_groups_by_file_and_caps_matches() {
        let mut s = String::new();
        for i in 1..=15 {
            s.push_str(&format!(
                "crates/zerith-runtime/src/agent.rs:{i}:let value_{i} = compute_something_very_long({i});\n"
            ));
        }
        for i in 1..=4 {
            s.push_str(&format!(
                "crates/zerith-runtime/src/lib.rs:{i}:pub fn helper_{i}() {{}}\n"
            ));
        }

        let out = compress_tool_output("grep", &s);
        assert_eq!(out.filter, Some("grep"));
        assert!(out.saved_bytes > 0);
        assert!(
            out.text
                .contains("[file] crates/zerith-runtime/src/agent.rs (15):"),
            "{}",
            out.text
        );
        assert!(out.text.contains("... (+5 more)"), "{}", out.text);
        assert!(
            out.text
                .contains("[file] crates/zerith-runtime/src/lib.rs (4):"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("value_15"));
    }

    #[test]
    fn read_file_output_is_never_cut() {
        let s: String = (0..600).map(|i| format!("interface Gi0/0/{i}\n")).collect();
        assert_eq!(compress_tool_output("read_file", &s).saved_bytes, 0);
        assert!(compress_tool_output("shell", &s).saved_bytes > 0);
    }

    #[test]
    fn smart_truncate_keeps_head_and_tail_for_long_unique_logs() {
        let mut s = String::new();
        for i in 0..300 {
            s.push_str(&format!("unique output line number {i:04}\n"));
        }
        let out = compress_tool_output("shell", &s);
        assert_eq!(out.filter, Some("smart-truncate"));
        assert!(out.saved_bytes > 0);
        assert!(out.text.contains("unique output line number 0000"));
        assert!(out.text.contains("unique output line number 0119"));
        assert!(out.text.contains("... (+120 lines truncated)"));
        assert!(out.text.contains("unique output line number 0240"));
        assert!(out.text.contains("unique output line number 0299"));
        assert!(!out.text.contains("unique output line number 0150"));
    }

    #[test]
    fn multibyte_utf8_near_head_boundary_does_not_panic() {
        // Place a 4-byte UTF-8 emoji straddling byte index 1024 (1022..1026).
        let prefix = "é".repeat(511); // 1022 bytes
        let mut s = format!("{prefix}🦀🦀🦀\n");
        for i in 0..300 {
            s.push_str(&format!("linha utf8 {i:04} — ação\n"));
        }
        assert!(s.len() > 1024);
        assert!(!s.is_char_boundary(1024));

        let out = compress_tool_output("shell", &s);
        assert_eq!(out.filter, Some("smart-truncate"));
        assert!(out.saved_bytes > 0);
        assert!(out.text.contains("🦀🦀🦀"));
    }

    #[test]
    fn git_log_find_tree_and_dedup_filters_work() {
        // git-log omits diff bodies
        let mut log = String::from(
            "commit 0123456789abcdef\nAuthor: Dev <dev@example.com>\nDate: Mon Jan 1\n\n    feat: fast path\n\n 1 file changed, 2 insertions(+)\n",
        );
        log.push_str("diff --git a/a.rs b/a.rs\n");
        for i in 0..40 {
            log.push_str(&format!("+long diff payload line {i:03}\n"));
        }
        let out = compress_tool_output("shell", &log);
        assert_eq!(out.filter, Some("git-log"));
        assert!(out.text.contains("commit 0123456789abcdef"));
        assert!(out.text.contains("    feat: fast path"));
        assert!(out.text.contains("1 file changed"));
        assert!(!out.text.contains("long diff payload"));

        // find groups by directory
        let mut paths = String::new();
        for i in 0..15 {
            paths.push_str(&format!("./src/mod_a/very_long_file_name_{i:02}.rs\n"));
        }
        let out = compress_tool_output("shell", &paths);
        assert_eq!(out.filter, Some("find"));
        assert!(out.text.contains("./src/mod_a/ (15):"), "{}", out.text);
        assert!(out.text.contains("... (+5 more)"), "{}", out.text);

        // dedup-log collapses repeated consecutive lines
        let mut dup = String::from("start\n");
        for _ in 0..30 {
            dup.push_str("heartbeat waiting for socket connection...\n");
        }
        dup.push_str("done\n");
        let out = compress_tool_output("shell", &dup);
        assert_eq!(out.filter, Some("dedup-log"));
        assert!(out.text.contains("(29 duplicate lines)"), "{}", out.text);

        // tree strips trailing summary line and caps at 200 lines
        let mut tree = String::from(".\n");
        for i in 0..230 {
            tree.push_str(&format!("├── file_{i:03}.rs\n"));
        }
        tree.push_str("\n0 directories, 230 files\n");
        let out = compress_tool_output("shell", &tree);
        assert_eq!(out.filter, Some("tree"));
        assert!(!out.text.contains("0 directories, 230 files"));
        assert!(out.text.contains("(+31 lines omitted)"), "{}", out.text);
    }

    #[test]
    fn estimate_caveman_saved_tokens_splits_prose_and_code_across_levels() {
        let prose_only = "Use concise output without filler words or pleasantries here.";
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Off, prose_only, 100),
            0
        );
        assert_eq!(estimate_caveman_saved_tokens(CavemanLevel::Off, "", 0), 0);
        assert_eq!(estimate_caveman_saved_tokens(CavemanLevel::Full, "", 0), 0);

        // Pure prose with completion_tokens = 100 -> exact percentage factors
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Lite, prose_only, 100),
            33
        );
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Full, prose_only, 100),
            82
        );
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Ultra, prose_only, 100),
            150
        );
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Wenyan, prose_only, 100),
            233
        );

        // Code inside triple backticks is excluded from prose compression
        let pure_code = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Full, pure_code, 80),
            0
        );

        // Mixed prose + fenced code: only the prose share is scaled
        let mixed = format!("{}{}", "a".repeat(20), "```rs\nlet x = 1;\n```"); // 20 prose + 20 code = 40 bytes
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Full, &mixed, 40),
            20 * 82 / 100
        );

        // Fallback when completion_tokens == 0 uses (prose_bytes + 3) / 4
        let forty_bytes = "x".repeat(40); // 10 tokens
        assert_eq!(
            estimate_caveman_saved_tokens(CavemanLevel::Full, &forty_bytes, 0),
            10 * 82 / 100
        );
    }

    #[test]
    fn estimate_ponytail_saved_tokens_weights_code_and_tool_args_across_levels() {
        let code = "```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```";
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Off, code, 0, 100),
            0
        );

        // Pure code with completion_tokens = 100
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Lite, code, 0, 100),
            25
        );
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Full, code, 0, 100),
            54
        );
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Ultra, code, 0, 100),
            100
        );

        // Prose-only output halves effective_tokens before applying level factor
        let prose = "Here is a plain explanation without any code blocks.";
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Lite, prose, 0, 100),
            12
        );
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Full, prose, 0, 100),
            27
        );
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Ultra, prose, 0, 100),
            50
        );

        // Tool arguments with empty text count as code_plus_args_bytes
        assert_eq!(
            estimate_ponytail_saved_tokens(PonytailLevel::Full, "", 80, 0),
            20 * 54 / 100
        );
    }
}
