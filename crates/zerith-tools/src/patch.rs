//! `apply_patch`: several files added, changed, moved or deleted in one call,
//! all or nothing. The envelope is the one Codex models are trained on
//! (`*** Begin Patch` / `*** Update File:` / `@@` / `+` `-` ` ` lines), from
//! the grammar in openai/codex `apply_patch.lark` (Apache-2.0). The parser and
//! the matcher below are Zerith's own; the matcher tries exact, then trailing
//! whitespace, then both ends, as Codex's `seek_sequence` describes.
//!
//! Every file goes through the same checks as `edit_file`: the policy (one
//! Write action per path), read-before-edit, and the checkpoint hook.

use std::path::PathBuf;

use serde_json::{json, Value};
use zerith_security::{Action, Verdict};

use crate::builtin::write_atomic;
use crate::fntool::{make_with, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

#[derive(Debug, PartialEq)]
enum Op {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

/// One `@@` block: lines it must find (`old`) and what replaces them (`new`).
#[derive(Debug, PartialEq, Default)]
struct Chunk {
    /// Text after `@@`: a line to find first, to narrow where `old` sits.
    anchor: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    /// `*** End of File`: `old` sits at the end of the file.
    eof: bool,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

fn parse(text: &str) -> Result<Vec<Op>> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .ok_or_else(|| bad("o patch precisa começar com `*** Begin Patch`"))?;
    let mut ops = Vec::new();
    let mut i = start + 1;
    let mut ended = false;
    while i < lines.len() {
        let l = lines[i];
        if l.trim() == "*** End Patch" {
            ended = true;
            break;
        }
        if let Some(p) = l.strip_prefix("*** Add File: ") {
            let mut add = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let body = lines[i].strip_prefix('+').ok_or_else(|| {
                    bad(format!(
                        "linha {}: em Add File toda linha começa com +",
                        i + 1
                    ))
                })?;
                add.push(body.to_string());
                i += 1;
            }
            ops.push(Op::Add {
                path: p.trim().into(),
                lines: add,
            });
            continue;
        }
        if let Some(p) = l.strip_prefix("*** Delete File: ") {
            ops.push(Op::Delete {
                path: p.trim().into(),
            });
            i += 1;
            continue;
        }
        if let Some(p) = l.strip_prefix("*** Update File: ") {
            i += 1;
            let mut to = None;
            if let Some(t) = lines.get(i).and_then(|l| l.strip_prefix("*** Move to: ")) {
                to = Some(t.trim().to_string());
                i += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            let mut cur = Chunk::default();
            let mut started = false;
            while i < lines.len() {
                let l = lines[i];
                if l.trim() == "*** End of File" {
                    cur.eof = true;
                    i += 1;
                    continue;
                }
                if l.starts_with("*** ") {
                    break;
                }
                if l == "@@" || l.starts_with("@@ ") {
                    if started {
                        chunks.push(std::mem::take(&mut cur));
                    }
                    let a = l.trim_start_matches('@').trim();
                    cur.anchor = (!a.is_empty()).then(|| a.to_string());
                    started = true;
                } else if let Some(r) = l.strip_prefix('-') {
                    cur.old.push(r.into());
                    started = true;
                } else if let Some(r) = l.strip_prefix('+') {
                    cur.new.push(r.into());
                    started = true;
                } else if let Some(r) = l.strip_prefix(' ') {
                    cur.old.push(r.into());
                    cur.new.push(r.into());
                    started = true;
                } else if l.is_empty() {
                    // A blank context line whose leading space was stripped.
                    cur.old.push(String::new());
                    cur.new.push(String::new());
                } else {
                    return Err(bad(format!(
                        "linha {}: em Update File a linha começa com @@, +, - ou espaço",
                        i + 1
                    )));
                }
                i += 1;
            }
            if started {
                chunks.push(cur);
            }
            if chunks.is_empty() && to.is_none() {
                return Err(bad(format!("Update File {p} sem nenhuma mudança")));
            }
            ops.push(Op::Update {
                path: p.trim().into(),
                to,
                chunks,
            });
            continue;
        }
        if l.trim().is_empty() {
            i += 1;
            continue;
        }
        return Err(bad(format!("linha {}: esperava `*** Add File:`, `*** Update File:`, `*** Delete File:` ou `*** End Patch`", i + 1)));
    }
    if !ended {
        return Err(bad("o patch precisa terminar com `*** End Patch`"));
    }
    if ops.is_empty() {
        return Err(bad("o patch não tem nenhum arquivo"));
    }
    Ok(ops)
}

/// All start indices where `pat` matches in `lines` at or after `from` in the
/// first matching pass: exact, then ignoring trailing whitespace, then ignoring
/// both ends.
fn seek_all(lines: &[String], pat: &[String], from: usize, eof: bool) -> Vec<usize> {
    if pat.is_empty() {
        return vec![if eof { lines.len() } else { from }];
    }
    if pat.len() > lines.len() {
        return Vec::new();
    }
    let last = lines.len() - pat.len();
    let start = if eof { last.max(from) } else { from };
    let eq: [fn(&str, &str) -> bool; 3] = [
        |a, b| a == b,
        |a, b| a.trim_end() == b.trim_end(),
        |a, b| a.trim() == b.trim(),
    ];
    for f in eq {
        let mut hits = Vec::new();
        for i in start..=last {
            if pat.iter().enumerate().all(|(k, p)| f(&lines[i + k], p)) {
                hits.push(i);
            }
        }
        if !hits.is_empty() {
            return hits;
        }
        if eof && start > from {
            for i in from..start {
                if pat.iter().enumerate().all(|(k, p)| f(&lines[i + k], p)) {
                    hits.push(i);
                }
            }
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    Vec::new()
}

/// Where `pat` starts in `lines` at or after `from`: exact, then ignoring
/// trailing whitespace, then ignoring both ends.
#[cfg(test)]
fn seek(lines: &[String], pat: &[String], from: usize, eof: bool) -> Option<usize> {
    seek_all(lines, pat, from, eof).into_iter().next()
}

fn apply_chunks(path: &str, text: &str, chunks: &[Chunk]) -> Result<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let crlf = text.contains("\r\n");
    let trailing = text.ends_with('\n') || text.is_empty();
    let mut cursor = 0usize;
    for (n, c) in chunks.iter().enumerate() {
        if let Some(a) = &c.anchor {
            let anchor_hits = seek_all(&lines, std::slice::from_ref(a), cursor, false);
            if anchor_hits.is_empty() {
                return Err(ToolError::Failed(format!(
                    "{path}: trecho {} não achou a linha de referência `@@ {a}`; nada foi alterado",
                    n + 1
                )));
            }
            if c.old.is_empty() && anchor_hits.len() > 1 {
                let line_nums: Vec<String> = anchor_hits
                    .iter()
                    .map(|&idx| (idx + 1).to_string())
                    .collect();
                return Err(ToolError::Failed(format!(
                    "{path}: trecho {} tem linha de referência `@@ {a}` ambígua (linhas {}); inclua mais linhas de contexto. Nada foi alterado",
                    n + 1,
                    line_nums.join(", ")
                )));
            }
            cursor = anchor_hits[0];
        }
        let hits = seek_all(&lines, &c.old, cursor, c.eof);
        let at = match hits.as_slice() {
            [] => {
                return Err(ToolError::Failed(format!(
                    "{path}: trecho {} não bate com o arquivo (as linhas com - e espaço precisam existir nessa ordem); leia o arquivo de novo. Nada foi alterado",
                    n + 1
                )));
            }
            [only] => *only,
            many => {
                let line_nums: Vec<String> =
                    many.iter().map(|&idx| (idx + 1).to_string()).collect();
                return Err(ToolError::Failed(format!(
                    "{path}: trecho {} é ambíguo e casa em {} locais (linhas {}); inclua mais linhas de contexto. Nada foi alterado",
                    n + 1,
                    many.len(),
                    line_nums.join(", ")
                )));
            }
        };
        lines.splice(at..at + c.old.len(), c.new.iter().cloned());
        cursor = at + c.new.len();
    }
    let nl = if crlf { "\r\n" } else { "\n" };
    let mut out = lines.join(nl);
    if trailing && !out.is_empty() {
        out.push_str(nl);
    }
    Ok(out)
}

/// Every path the patch writes or deletes, for the policy.
fn targets(ops: &[Op]) -> Vec<&str> {
    let mut v = Vec::new();
    for op in ops {
        match op {
            Op::Add { path, .. } | Op::Delete { path } => v.push(path.as_str()),
            Op::Update { path, to, .. } => {
                v.push(path.as_str());
                if let Some(t) = to {
                    v.push(t.as_str());
                }
            }
        }
    }
    v
}

fn read_patch_text(p: &std::path::Path) -> Result<(String, bool)> {
    let raw = std::fs::read(p)
        .map_err(|e| ToolError::Failed(format!("{}: {e}. Nada foi alterado", p.display())))?;
    Ok(match String::from_utf8(raw) {
        Ok(s) => (s, false),
        Err(e) => (
            e.into_bytes().into_iter().map(|b| b as char).collect(),
            true,
        ),
    })
}

fn encode_patch_bytes(p: &std::path::Path, body: String, is_latin1: bool) -> Result<Vec<u8>> {
    if !is_latin1 {
        return Ok(body.into_bytes());
    }
    let mut bytes = Vec::with_capacity(body.len());
    for ch in body.chars() {
        let b = u8::try_from(ch as u32).map_err(|_| {
            ToolError::Failed(format!(
                "{}: caractere fora de Latin-1 em arquivo legado. Nada foi alterado",
                p.display()
            ))
        })?;
        bytes.push(b);
    }
    Ok(bytes)
}

fn run(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let ops = parse(str_arg(args, "patch")?)?;
    // Every touched path must pass policy, not only the one returned by `action()`.
    // `Verdict::Ask` was already resolved via `authorize()` + interactive user approval
    // (or rejected upfront by non-interactive `Registry::call`), so only `Deny` blocks here.
    for t in targets(&ops) {
        let p = ctx.resolve(t);
        if let Verdict::Deny(why) = ctx
            .policy
            .check_tool("apply_patch", &Action::Write(p.clone()))
        {
            return Err(ToolError::Denied(format!("{}: {why}", p.display())));
        }
    }
    // Plan everything first; touch the disk only when every file is ready.
    enum Step {
        Write(PathBuf, Vec<u8>),
        Remove(PathBuf),
    }
    let mut plan: Vec<Step> = Vec::new();
    let mut summary = Vec::new();
    let mut moved_from = std::collections::HashMap::new();
    for op in &ops {
        match op {
            Op::Add { path, lines } => {
                let p = ctx.resolve(path);
                if p.exists() {
                    return Err(ToolError::Failed(format!(
                        "{}: já existe; use Update File. Nada foi alterado",
                        p.display()
                    )));
                }
                let mut body = lines.join("\n");
                body.push('\n');
                summary.push(format!("A {path}"));
                plan.push(Step::Write(p, body.into_bytes()));
            }
            Op::Delete { path } => {
                let p = ctx.resolve(path);
                exec.host.check_editable(&p)?;
                if !p.is_file() {
                    return Err(ToolError::Failed(format!(
                        "{}: não existe. Nada foi alterado",
                        p.display()
                    )));
                }
                summary.push(format!("D {path}"));
                plan.push(Step::Remove(p));
            }
            Op::Update { path, to, chunks } => {
                let p = ctx.resolve(path);
                exec.host.check_editable(&p)?;
                let (text, is_latin1) = read_patch_text(&p)?;
                let new = apply_chunks(path, &text, chunks)?;
                let target = to.as_ref().map_or_else(|| p.clone(), |t| ctx.resolve(t));
                let bytes = encode_patch_bytes(&target, new, is_latin1)?;
                match to {
                    Some(t) => {
                        summary.push(format!("R {path} -> {t}"));
                        moved_from.insert(target.clone(), p.clone());
                        plan.push(Step::Write(target, bytes));
                        plan.push(Step::Remove(p));
                    }
                    None => {
                        summary.push(format!("M {path}"));
                        plan.push(Step::Write(p, bytes));
                    }
                }
            }
        }
    }
    // Conteúdo final inválido barra o patch inteiro antes de tocar o disco.
    for s in &plan {
        if let Step::Write(p, body) = s {
            crate::lint::check(p, body)?;
        }
    }
    exec.host.snapshot_workspace(ctx.workspace);
    for s in &plan {
        let p = match s {
            Step::Write(p, _) | Step::Remove(p) => p,
        };
        if p.exists() {
            exec.host.before_write(p)?;
        }
    }
    let mut checked = Vec::new();
    for s in plan {
        match s {
            Step::Write(p, body) => {
                // Num `Move to`, o "antes" é o arquivo de origem.
                let pc = crate::postcheck::before(ctx.workspace, moved_from.get(&p).unwrap_or(&p));
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d)?;
                }
                write_atomic(&p, &body)?;
                exec.host.note_read(&p);
                checked.push((p, pc));
            }
            Step::Remove(p) => std::fs::remove_file(&p)?,
        }
    }
    let mut msg = format!("patch aplicado:\n{}", summary.join("\n"));
    for (p, pc) in checked {
        let mut extras = Vec::new();
        if let Some(diags) = crate::lsp::warm_diagnostics_for_edit(ctx.workspace, &p) {
            extras.push(format!("[diagnósticos LSP]\n{diags}"));
        }
        let block = crate::postcheck::after(&p, pc);
        if !block.is_empty() {
            extras.push(block);
        }
        if !extras.is_empty() {
            msg.push_str(&format!("\n\n{}:\n{}", p.display(), extras.join("\n\n")));
        }
    }
    Ok(msg)
}

/// Evaluates every target path against the policy and returns the most
/// restrictive path (`Deny` > boundary `Ask` > in-workspace `Ask` > `Allow`),
/// so files `2..N` in a multi-file patch can never bypass `Ask`, `Deny`, or
/// sensitive-boundary forced prompts in `authorize()` / `Pipeline::prepare`.
fn action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    let ops = parse(str_arg(args, "patch")?)?;
    let mut chosen: Option<(u8, PathBuf)> = None;
    for t in targets(&ops) {
        let p = ctx.resolve(t);
        let rank = match ctx
            .policy
            .check_tool("apply_patch", &Action::Write(p.clone()))
        {
            Verdict::Deny(_) => 3,
            Verdict::Ask(_)
                if !ctx.policy.inside_workspace(&p) || ctx.policy.is_own_control(&p) =>
            {
                2
            }
            Verdict::Ask(_) => 1,
            Verdict::Allow => 0,
        };
        if chosen.as_ref().is_none_or(|(best, _)| rank > *best) {
            chosen = Some((rank, p));
            if rank == 3 {
                break;
            }
        }
    }
    let target = chosen.map(|(_, p)| p).unwrap_or_else(|| PathBuf::from("."));
    Ok(Some(Action::Write(target)))
}

pub(crate) fn tool() -> Box<dyn Tool> {
    make_with(
        "apply_patch",
        "Aplica em uma chamada só mudanças em vários arquivos (criar, alterar, mover, apagar), tudo ou nada: se um trecho não bate, nenhum arquivo muda. Os arquivos alterados ou apagados precisam ter sido lidos antes. Para uma troca simples num arquivo, edit_file basta.",
        json!({"type": "object", "properties": {
            "patch": {"type": "string", "description": "o patch inteiro. Formato: `*** Begin Patch`, depois um ou mais blocos `*** Add File: caminho` (cada linha com +), `*** Delete File: caminho`, ou `*** Update File: caminho` (opcional `*** Move to: novo`) seguido de trechos `@@ linha de referência opcional` com linhas ` contexto`, `-removida` e `+nova` (3 linhas de contexto ao redor de cada mudança); termine com `*** End Patch`."}
        }, "required": ["patch"]}),
        "a lista de arquivos alterados (A criado, M alterado, R movido, D apagado)",
        Permission::Write,
        30,
        false,
        action,
        run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn parses_every_kind_of_hunk() {
        let ops = parse("*** Begin Patch\n*** Add File: a.txt\n+oi\n*** Delete File: b.txt\n*** Update File: c.py\n*** Move to: d.py\n@@ def f():\n-    return 1\n+    return 2\n*** End Patch\n").unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(
            ops[0],
            Op::Add {
                path: "a.txt".into(),
                lines: vec!["oi".into()]
            }
        );
        assert_eq!(
            ops[1],
            Op::Delete {
                path: "b.txt".into()
            }
        );
        let Op::Update { to, chunks, .. } = &ops[2] else {
            panic!()
        };
        assert_eq!(to.as_deref(), Some("d.py"));
        assert_eq!(chunks[0].anchor.as_deref(), Some("def f():"));
        assert!(parse("*** Begin Patch\n*** End Patch").is_err(), "empty");
        assert!(parse("*** Update File: x\n-a\n").is_err(), "no envelope");
        assert!(parse("*** Begin Patch\n*** Add File: x\nsem mais\n*** End Patch").is_err());
    }

    #[test]
    fn the_matcher_forgives_whitespace_but_not_content() {
        let f = lines("a\n  b  \nc");
        assert_eq!(seek(&f, &lines("b"), 0, false), Some(1));
        assert_eq!(seek(&f, &lines("  b"), 0, false), Some(1));
        assert_eq!(seek(&f, &lines("x"), 0, false), None);
        assert_eq!(seek(&f, &lines("c"), 0, true), Some(2));
    }

    #[test]
    fn chunks_apply_in_order_and_keep_line_endings() {
        let out = apply_chunks(
            "x",
            "um\r\ndois\r\ntres\r\n",
            &[Chunk {
                old: lines("dois"),
                new: lines("DOIS\nmeio"),
                ..Chunk::default()
            }],
        )
        .unwrap();
        assert_eq!(out, "um\r\nDOIS\r\nmeio\r\ntres\r\n");
        let miss = apply_chunks(
            "x",
            "a\n",
            &[Chunk {
                old: lines("z"),
                new: vec![],
                ..Chunk::default()
            }],
        );
        assert!(miss.is_err());
    }

    #[test]
    fn ambiguous_hunk_is_rejected_with_line_numbers() {
        let src = "fn a() {\n    return 1;\n}\nfn b() {\n    return 1;\n}\n";
        let err = apply_chunks(
            "dup.rs",
            src,
            &[Chunk {
                old: lines("    return 1;"),
                new: lines("    return 2;"),
                ..Chunk::default()
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ambíguo"), "{err}");
        assert!(err.contains("2, 5"), "{err}");
    }

    #[test]
    fn latin1_file_and_multi_file_policy_check_work() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(scratch).unwrap();

        let legacy = dir.path().join("legacy.txt");
        std::fs::write(&legacy, b"N\xfamero 1\nlinha 2\n").unwrap();

        let pol = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::AcceptEdits,
            dir.path(),
        );
        let reg = crate::Registry::new();
        let ctx = CallCtx {
            workspace: pol.workspace.as_path(),
            policy: &pol,
            memory: None,
            scope: None,
        };

        let patch = "*** Begin Patch\n*** Update File: legacy.txt\n Número 1\n-linha 2\n+linha B\n*** End Patch\n";
        reg.call("apply_patch", &json!({"patch": patch}), &ctx)
            .unwrap();
        assert_eq!(std::fs::read(&legacy).unwrap(), b"N\xfamero 1\nlinha B\n");

        // File 1 inside workspace (Allow in AcceptEdits), file 2 outside workspace (Ask!).
        let escape_patch = "*** Begin Patch\n*** Add File: inside.txt\n+ok\n*** Add File: ../outside.txt\n+escape\n*** End Patch\n";
        let (verdict, act) = reg
            .authorize("apply_patch", &json!({"patch": escape_patch}), &ctx)
            .unwrap();
        assert!(matches!(verdict, Verdict::Ask(_)), "{verdict:?}");
        let Some(Action::Write(flagged)) = act else {
            panic!("expected Write action");
        };
        assert!(
            flagged.to_string_lossy().contains("outside.txt"),
            "{}",
            flagged.display()
        );

        // Mode::Default: boundary Ask on file 2 must outrank in-workspace Ask on file 1,
        // and an interactively approved in-workspace patch must succeed in `run_approved()`.
        let pol_default = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            dir.path(),
        );
        let ctx_default = CallCtx {
            workspace: pol_default.workspace.as_path(),
            policy: &pol_default,
            memory: None,
            scope: None,
        };
        let (v_def, act_def) = reg
            .authorize("apply_patch", &json!({"patch": escape_patch}), &ctx_default)
            .unwrap();
        assert!(matches!(v_def, Verdict::Ask(_)), "{v_def:?}");
        let Some(Action::Write(flagged_def)) = act_def else {
            panic!("expected Write action in Mode::Default");
        };
        assert!(
            flagged_def.to_string_lossy().contains("outside.txt"),
            "boundary file must win in Mode::Default, got {}",
            flagged_def.display()
        );

        let add_ok = "*** Begin Patch\n*** Add File: approved_default.txt\n+ok\n*** End Patch\n";
        let (v_add, _) = reg
            .authorize("apply_patch", &json!({"patch": add_ok}), &ctx_default)
            .unwrap();
        assert!(matches!(v_add, Verdict::Ask(_)), "{v_add:?}");
        let cancel = zerith_providers::Cancel::default();
        reg.run_approved(
            "apply_patch",
            &json!({"patch": add_ok}),
            &ctx_default,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("approved_default.txt")).unwrap(),
            "ok\n"
        );
    }
}
