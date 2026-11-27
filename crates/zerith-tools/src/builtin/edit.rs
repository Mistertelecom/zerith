//! File editing tools: `edit_file` (exact, CRLF-aware) and `multi_edit`
//! (atomic list of edits with a whitespace-normalised fallback).

use serde_json::Value;

use super::str_arg;
use crate::{CallCtx, Exec, Result, ToolError};

pub(super) fn edit_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    exec.host.check_editable(&path)?;
    let old = str_arg(args, "old_string")?;
    let new = str_arg(args, "new_string")?;
    let all = args["replace_all"].as_bool().unwrap_or(false);
    if old.is_empty() {
        return Err(ToolError::BadArguments("old_string está vazio".into()));
    }
    if old == new {
        return Err(ToolError::BadArguments(
            "old_string e new_string são iguais".into(),
        ));
    }
    let raw = std::fs::read(&path)?;
    let Ok(text) = String::from_utf8(raw) else {
        // Legacy Latin-1 file: multi_edit already reads and writes it byte for byte.
        let one = serde_json::json!({
            "path": args["path"],
            "edits": [{"old_string": old, "new_string": new, "replace_all": all}],
        });
        return multi_edit(&one, ctx, exec);
    };
    // A Windows file uses CRLF while the model, which sees lines through
    // `.lines()`, sends LF. The text as it came is tried first (the file keeps
    // its own style); the CRLF retry is what made a multi-line edit of this
    // very repo fail.
    let crlf = |t: &str| t.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = match (text.matches(old).count(), text.contains("\r\n")) {
        (0, true) => (crlf(old), crlf(new)),
        _ => (old.to_string(), new.to_string()),
    };
    let (old, new) = (old.as_str(), new.as_str());
    let n = text.matches(old).count();
    if n == 0 && !all {
        // Same tolerance as multi_edit: indentation lost in a copy is not a miss.
        let raw_new = str_arg(args, "new_string")?;
        return match find_unique_whitespace_span(&text, str_arg(args, "old_string")?) {
            WhitespaceFallback::Applied(start, end) => {
                let reindented =
                    reindent_replacement(&text[start..end], str_arg(args, "old_string")?, raw_new);
                let rep = if text.contains("\r\n") {
                    crlf(&reindented)
                } else {
                    reindented
                };
                let mut buf = text;
                buf.replace_range(start..end, &rep);
                crate::lint::check(&path, buf.as_bytes())?;
                let pc = crate::postcheck::before(ctx.workspace, &path);
                exec.host.before_write(&path)?;
                super::write_atomic(&path, buf.as_bytes())?;
                exec.host.note_read(&path);
                let mut msg = format!(
                    "1 ocorrência substituída em {} (casada ignorando espaços)",
                    path.display()
                );
                if let Some(diags) = crate::lsp::warm_diagnostics_for_edit(ctx.workspace, &path) {
                    msg.push_str(&format!("\n\n[diagnósticos LSP]\n{diags}"));
                }
                crate::postcheck::append(&mut msg, &path, pc);
                Ok(msg)
            }
            WhitespaceFallback::Ambiguous(k) => Err(ToolError::Failed(format!(
                "old_string não encontrado exato; {k} trechos casam ignorando espaços em {}: inclua mais contexto",
                path.display()
            ))),
            WhitespaceFallback::None => Err(ToolError::Failed(not_found(&text, old, &path))),
        };
    }
    match (n, all) {
        (0, _) => Err(ToolError::Failed(not_found(&text, old, &path))),
        (n, false) if n > 1 => Err(ToolError::Failed(format!(
            "old_string aparece {n} vezes em {}; inclua mais contexto ou use replace_all",
            path.display()
        ))),
        _ => {
            let out = text.replace(old, new);
            crate::lint::check(&path, out.as_bytes())?;
            let pc = crate::postcheck::before(ctx.workspace, &path);
            exec.host.before_write(&path)?;
            super::write_atomic(&path, out.as_bytes())?;
            exec.host.note_read(&path);
            let mut msg = format!("{n} ocorrência(s) substituída(s) em {}", path.display());
            if let Some(diags) = crate::lsp::warm_diagnostics_for_edit(ctx.workspace, &path) {
                msg.push_str(&format!("\n\n[diagnósticos LSP]\n{diags}"));
            }
            crate::postcheck::append(&mut msg, &path, pc);
            Ok(msg)
        }
    }
}

/// The miss, plus a short snippet around the file line closest to `old`'s
/// first non-empty line, so the model can copy the real text immediately.
fn not_found(buf: &str, old: &str, path: &std::path::Path) -> String {
    let mut msg = format!("old_string não encontrado em {}", path.display());
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = norm(old.lines().find(|l| !l.trim().is_empty()).unwrap_or(""));
    if first.is_empty() {
        return msg;
    }
    let words: Vec<&str> = first.split(' ').collect();
    let lines: Vec<&str> = buf.lines().collect();
    let mut scored: Vec<(usize, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let n = norm(l);
            let hits = words.iter().filter(|w| n.contains(*w)).count();
            (hits * 2 >= words.len()).then_some((hits, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    if !scored.is_empty() {
        msg.push_str("\nlinhas mais parecidas (releia com read_file ou copie o trecho exato):");
        let mut shown = std::collections::HashSet::new();
        for (rank, &(_, idx)) in scored.iter().take(3).enumerate() {
            let from = if rank == 0 {
                idx.saturating_sub(1)
            } else {
                idx
            };
            let to = if rank == 0 {
                (idx + 1).min(lines.len().saturating_sub(1))
            } else {
                idx
            };
            for (j, line) in lines.iter().enumerate().take(to + 1).skip(from) {
                if shown.insert(j) {
                    let l: String = line.chars().take(200).collect();
                    msg.push_str(&format!("\n{}\t{l}", j + 1));
                }
            }
        }
    }
    msg
}

fn leading_ws(line: &str) -> &str {
    let n = line
        .bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count();
    &line[..n]
}

/// Adjusts `raw_new`'s indentation to match the indentation actually found in
/// `matched_slice` when a whitespace-normalized fallback matched `raw_old` at
/// a different indent level (prevents corrupting Python/YAML/Rust indentation).
pub(crate) fn reindent_replacement(matched_slice: &str, raw_old: &str, raw_new: &str) -> String {
    let actual_indent = matched_slice
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(leading_ws)
        .unwrap_or("");
    let old_indent = raw_old
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(leading_ws)
        .unwrap_or("");
    let new_indent = raw_new
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(leading_ws)
        .unwrap_or("");
    // Only re-indent when `old` and `new` share the same shifted indent frame
    // (e.g. model copied a block at 4 spaces instead of 8). If the model
    // already wrote `new` with the file's real indentation (or intentionally
    // changed the base indentation relative to `old`), keep `raw_new` verbatim.
    if actual_indent == old_indent || new_indent != old_indent {
        return raw_new.to_string();
    }
    let normalized = raw_new.replace("\r\n", "\n");
    let had_trailing_nl = normalized.ends_with('\n');
    let adjusted: Vec<String> = normalized
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                return String::new();
            }
            if let Some(extra) = actual_indent.strip_prefix(old_indent) {
                format!("{extra}{line}")
            } else if let Some(excess) = old_indent.strip_prefix(actual_indent) {
                line.strip_prefix(excess).unwrap_or(line).to_string()
            } else if let Some(rest) = line.strip_prefix(old_indent) {
                format!("{actual_indent}{rest}")
            } else {
                format!("{actual_indent}{}", line.trim_start_matches([' ', '\t']))
            }
        })
        .collect();
    let mut out = adjusted.join("\n");
    if had_trailing_nl {
        out.push('\n');
    }
    out
}

#[derive(Clone, Copy)]
enum WhitespaceFallback {
    Applied(usize, usize),
    None,
    Ambiguous(usize),
}

fn find_unique_whitespace_span(buf: &str, old: &str) -> WhitespaceFallback {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let old_lines: Vec<String> = old.lines().map(norm).collect();
    if old_lines.is_empty() || old_lines.iter().all(String::is_empty) {
        return WhitespaceFallback::None;
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut off = 0usize;
    for chunk in buf.split_inclusive('\n') {
        let len = chunk.len();
        let trimmed_end = chunk.trim_end_matches(['\r', '\n']);
        spans.push((off, off + trimmed_end.len()));
        off += len;
    }
    if !buf.ends_with('\n') && off < buf.len() {
        spans.push((off, buf.len()));
    }
    if spans.len() < old_lines.len() {
        return WhitespaceFallback::None;
    }
    let line_norms: Vec<String> = spans.iter().map(|&(s, e)| norm(&buf[s..e])).collect();
    let mut hits = Vec::new();
    for i in 0..=spans.len() - old_lines.len() {
        if line_norms[i..i + old_lines.len()] == old_lines[..] {
            hits.push((spans[i].0, spans[i + old_lines.len() - 1].1));
        }
    }
    match hits.len() {
        0 => WhitespaceFallback::None,
        1 => WhitespaceFallback::Applied(hits[0].0, hits[0].1),
        n => WhitespaceFallback::Ambiguous(n),
    }
}

pub(super) fn multi_edit(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    exec.host.check_editable(&path)?;
    let edits = args["edits"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| ToolError::BadArguments("edits precisa ser um array não vazio".into()))?;
    let raw = std::fs::read(&path)?;
    let (mut buf, is_latin1) = match String::from_utf8(raw) {
        Ok(s) => (s, false),
        Err(e) => (
            e.into_bytes().into_iter().map(|b| b as char).collect(),
            true,
        ),
    };
    let crlf = |t: &str| t.replace("\r\n", "\n").replace('\n', "\r\n");
    let has_crlf = buf.contains("\r\n");
    let mut applied_count = 0usize;
    for (idx, item) in edits.iter().enumerate() {
        let raw_old = item["old_string"].as_str().ok_or_else(|| {
            ToolError::BadArguments(format!("edits[{idx}].old_string precisa ser string"))
        })?;
        let raw_new = item["new_string"].as_str().ok_or_else(|| {
            ToolError::BadArguments(format!("edits[{idx}].new_string precisa ser string"))
        })?;
        let replace_all = item["replace_all"].as_bool().unwrap_or(false);
        if raw_old.is_empty() {
            return Err(ToolError::BadArguments(format!(
                "edits[{idx}].old_string está vazio"
            )));
        }
        if raw_old == raw_new {
            return Err(ToolError::BadArguments(format!(
                "edits[{idx}]: old_string e new_string são iguais"
            )));
        }
        let (old_s, new_s) = match (buf.matches(raw_old).count(), has_crlf) {
            (0, true) => (crlf(raw_old), crlf(raw_new)),
            _ => (raw_old.to_string(), raw_new.to_string()),
        };
        let n = buf.matches(&old_s).count();
        if n == 1 || (replace_all && n >= 1) {
            buf = if replace_all {
                buf.replace(&old_s, &new_s)
            } else {
                buf.replacen(&old_s, &new_s, 1)
            };
            applied_count += n;
        } else if n > 1 {
            return Err(ToolError::Failed(format!(
                "edits[{idx}] aparece {n} vezes (use replace_all=true ou dê mais contexto)"
            )));
        } else if !replace_all {
            match find_unique_whitespace_span(&buf, raw_old) {
                WhitespaceFallback::Applied(start, end) => {
                    let reindented = reindent_replacement(&buf[start..end], raw_old, raw_new);
                    let rep = if has_crlf {
                        crlf(&reindented)
                    } else {
                        reindented
                    };
                    buf.replace_range(start..end, &rep);
                    applied_count += 1;
                }
                WhitespaceFallback::Ambiguous(k) => {
                    return Err(ToolError::Failed(format!(
                        "edits[{idx}]: {k} trechos compatíveis com espaços normalizados (ambíguo)"
                    )));
                }
                WhitespaceFallback::None => {
                    return Err(ToolError::Failed(format!(
                        "edits[{idx}]: {}",
                        not_found(&buf, raw_old, &path)
                    )));
                }
            }
        } else {
            return Err(ToolError::Failed(format!(
                "edits[{idx}]: trecho não encontrado em {}",
                path.display()
            )));
        }
    }
    let out_bytes = if is_latin1 {
        let mut bytes = Vec::with_capacity(buf.len());
        for ch in buf.chars() {
            let b = u8::try_from(ch as u32).map_err(|_| {
                ToolError::Failed("multi_edit: caractere fora de Latin-1 em arquivo legado".into())
            })?;
            bytes.push(b);
        }
        bytes
    } else {
        buf.into_bytes()
    };
    crate::lint::check(&path, &out_bytes)?;
    let pc = crate::postcheck::before(ctx.workspace, &path);
    exec.host.before_write(&path)?;
    super::write_atomic(&path, &out_bytes)?;
    exec.host.note_read(&path);
    let mut msg = format!(
        "{} edição(ões) atômica(s) ({applied_count} substituição(ões)) em {}",
        edits.len(),
        path.display()
    );
    if let Some(diags) = crate::lsp::warm_diagnostics_for_edit(ctx.workspace, &path) {
        msg.push_str(&format!("\n\n[diagnósticos LSP]\n{diags}"));
    }
    crate::postcheck::append(&mut msg, &path, pc);
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindent_replacement_preserves_python_indentation_on_under_indent() {
        let matched = "        if ok:\n            return 1";
        let old = "    if ok:\n        return 1";
        let new = "    if ok:\n        log('yes')\n        return 2";
        assert_eq!(
            reindent_replacement(matched, old, new),
            "        if ok:\n            log('yes')\n            return 2"
        );
    }

    #[test]
    fn reindent_replacement_handles_over_indent_and_tabs() {
        let matched = "\tval = compute()\n\treturn val";
        let old = "    val = compute()\n    return val";
        let new = "    val = compute() + 1\n    return val";
        assert_eq!(
            reindent_replacement(matched, old, new),
            "\tval = compute() + 1\n\treturn val"
        );

        let matched2 = "  x = 1";
        let old2 = "      x = 1";
        let new2 = "      x = 2\n        y = 3";
        assert_eq!(
            reindent_replacement(matched2, old2, new2),
            "  x = 2\n    y = 3"
        );
    }

    #[test]
    fn not_found_includes_surrounding_context_around_closest_line() {
        let file = "def alpha():\n    x = compute_value(1)\n    return x\n";
        let msg = not_found(
            file,
            "    x = compute_value(99)",
            std::path::Path::new("a.py"),
        );
        assert!(msg.contains("1\tdef alpha():"), "{msg}");
        assert!(msg.contains("2\t    x = compute_value(1)"), "{msg}");
        assert!(msg.contains("3\t    return x"), "{msg}");
    }
}
