//! Recusa gravar `.json` / `.toml` / `.yaml` / `.yml` quebrado (lint
//! fail-closed, ideia do Hermes, MIT): o modelo que esquece uma vírgula num
//! `package.json` ou quebra a indentação de um `.yaml` só descobre quando o
//! build cai, e aí já não lembra o que mexeu.
//!
//! Arquivo que já estava inválido passa: bloquear impediria consertá-lo.

use std::path::Path;

use crate::{Result, ToolError};

#[derive(Clone, Copy)]
enum Kind {
    Json,
    Toml,
    Yaml,
}

fn kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        // tsconfig/jsconfig e as configurações do VS Code são JSONC
        // (comentários, vírgula no fim): o parser estrito os recusaria.
        "json"
            if !name.starts_with("tsconfig")
                && !name.starts_with("jsconfig")
                && path.parent().and_then(Path::file_name) != Some(".vscode".as_ref()) =>
        {
            Some(Kind::Json)
        }
        "toml" => Some(Kind::Toml),
        "yaml" | "yml" => Some(Kind::Yaml),
        _ => None,
    }
}

/// `(linha, coluna, mensagem)` do primeiro erro, ou `None` se o texto é válido.
fn parse(kind: Kind, text: &str) -> Option<(usize, usize, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    match kind {
        Kind::Json => serde_json::from_str::<serde::de::IgnoredAny>(text)
            .err()
            .map(|e| {
                let msg = e.to_string();
                // O Display do serde_json repete " at line X column Y".
                let msg = msg.split(" at line ").next().unwrap_or(&msg).to_string();
                (e.line(), e.column(), msg)
            }),
        Kind::Toml => text.parse::<toml::Table>().err().map(|e| {
            let start = e.span().map_or(0, |s| s.start).min(text.len());
            let before = &text[..start];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            (line, col, e.message().trim().to_string())
        }),
        Kind::Yaml => validate_yaml(text),
    }
}

fn is_block_scalar_indicator(val: &str) -> bool {
    let mut chars = val.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '|' && first != '>' {
        return false;
    }
    chars.all(|c| matches!(c, '+' | '-' | '0'..='9'))
}

fn split_mapping(body: &str) -> Option<(&str, &str)> {
    if let Some((k, v)) = body.split_once(": ") {
        return Some((k, v.trim_start()));
    }
    if let Some(k) = body.strip_suffix(':') {
        return Some((k, ""));
    }
    None
}

fn has_missing_space_after_colon(body: &str) -> bool {
    let Some(pos) = body.find(':') else {
        return false;
    };
    let before = &body[..pos];
    let after = &body[pos + 1..];
    if after.is_empty() || after.starts_with("//") || after.starts_with(['\\', '/']) {
        return false;
    }
    let mut chars = before.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !after.starts_with(char::is_whitespace)
}

fn scan_yaml_line(
    line: &str,
    line_no: usize,
    quote: &mut Option<(char, usize, usize)>,
    flow: &mut Vec<(char, usize, usize)>,
) -> std::result::Result<String, (usize, usize, String)> {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0usize;
    let mut prev_ws = true;
    while i < chars.len() {
        let col = i + 1;
        let ch = chars[i].1;
        if let Some((q, _, _)) = *quote {
            if q == '"' && ch == '\\' {
                i += 2;
                prev_ws = false;
                continue;
            }
            if q == '\'' && ch == '\'' && chars.get(i + 1).is_some_and(|&(_, n)| n == '\'') {
                i += 2;
                prev_ws = false;
                continue;
            }
            if ch == q {
                *quote = None;
                out.push('_');
            }
            i += 1;
            prev_ws = false;
            continue;
        }
        if ch == '#' && prev_ws {
            break;
        }
        match ch {
            '"' | '\'' => {
                *quote = Some((ch, line_no, col));
                prev_ws = false;
            }
            '[' | '{' => {
                if flow.is_empty() {
                    out.push('_');
                }
                flow.push((ch, line_no, col));
                prev_ws = false;
            }
            ']' | '}' => {
                let want = if ch == ']' { '[' } else { '{' };
                match flow.pop() {
                    Some((open, _, _)) if open == want => {}
                    Some((open, _, _)) => {
                        return Err((
                            line_no,
                            col,
                            format!("delimitador `{ch}` não fecha `{open}` aberto antes"),
                        ));
                    }
                    None => {
                        return Err((
                            line_no,
                            col,
                            format!("delimitador `{ch}` inesperado sem abertura"),
                        ));
                    }
                }
                prev_ws = false;
            }
            _ => {
                if flow.is_empty() {
                    out.push(ch);
                }
                prev_ws = ch.is_whitespace();
            }
        }
        i += 1;
    }
    Ok(out)
}

/// Lightweight structural YAML validation (no external YAML crate):
/// catches tab indentation, unclosed quotes/flow `[`/`{`, invalid mapping
/// syntax (`key:value`, unexpected colons), and inconsistent indentation.
fn validate_yaml(text: &str) -> Option<(usize, usize, String)> {
    let mut quote: Option<(char, usize, usize)> = None;
    let mut flow: Vec<(char, usize, usize)> = Vec::new();
    let mut block_parent_indent: Option<usize> = None;
    let mut block_content_indent: Option<usize> = None;
    let mut levels: Vec<usize> = Vec::new();
    let mut can_open_child: Option<usize> = None;

    for (idx, raw_line) in text.split('\n').enumerate() {
        let line_no = idx + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if let Some(parent_ind) = block_parent_indent {
            if line.trim().is_empty() {
                continue;
            }
            let spaces = line.bytes().take_while(|&b| b == b' ').count();
            match block_content_indent {
                None => {
                    if line.as_bytes().get(spaces) == Some(&b'\t') {
                        return Some((
                            line_no,
                            spaces + 1,
                            "tabulação (\\t) não é permitida na indentação YAML; use espaços"
                                .into(),
                        ));
                    }
                    if spaces > parent_ind {
                        block_content_indent = Some(spaces);
                        continue;
                    }
                    block_parent_indent = None;
                    block_content_indent = None;
                }
                Some(content_ind) => {
                    if spaces >= content_ind {
                        continue;
                    }
                    block_parent_indent = None;
                    block_content_indent = None;
                }
            }
        }

        let started_in_multiline = quote.is_some() || !flow.is_empty();
        if !started_in_multiline {
            for (col0, b) in line.bytes().enumerate() {
                if b == b'\t' {
                    return Some((
                        line_no,
                        col0 + 1,
                        "tabulação (\\t) não é permitida na indentação YAML; use espaços".into(),
                    ));
                }
                if b != b' ' {
                    break;
                }
            }
        }

        let indent = line.bytes().take_while(|&b| b == b' ').count();
        let trimmed = line[indent..].trim_end();
        if !started_in_multiline && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        if !started_in_multiline
            && indent == 0
            && (trimmed == "---"
                || trimmed.starts_with("--- ")
                || trimmed == "..."
                || trimmed.starts_with("... "))
        {
            levels.clear();
            can_open_child = None;
            continue;
        }

        let sanitized = match scan_yaml_line(&line[indent..], line_no, &mut quote, &mut flow) {
            Ok(s) => s,
            Err(e) => return Some(e),
        };
        if started_in_multiline {
            continue;
        }

        let mut body = sanitized.trim();
        if body.is_empty() {
            continue;
        }

        let mut is_seq_item = false;
        let mut extra_seq_col = 0usize;
        loop {
            if body == "-" {
                is_seq_item = true;
                body = "";
                break;
            }
            if let Some(after_dash) = body.strip_prefix("- ") {
                is_seq_item = true;
                let rest = after_dash.trim_start();
                extra_seq_col += body.len() - rest.len();
                body = rest;
                continue;
            }
            break;
        }

        let mapping = split_mapping(body);
        if let Some((_, val)) = mapping {
            if split_mapping(val).is_some() {
                return Some((
                    line_no,
                    indent + 1,
                    "sintaxe de mapeamento YAML inválida (dois-pontos inesperado no valor)".into(),
                ));
            }
        } else if !is_seq_item && has_missing_space_after_colon(body) {
            return Some((
                line_no,
                indent + 1,
                "sintaxe de mapeamento YAML inválida (`chave:valor` sem espaço após `:`)".into(),
            ));
        }

        let has_mapping = mapping.is_some();
        if levels.is_empty() {
            levels.push(indent);
        } else {
            let top = *levels.last().unwrap();
            if indent > top {
                if can_open_child.is_some_and(|p| indent > p) {
                    levels.push(indent);
                } else if has_mapping || is_seq_item {
                    return Some((
                        line_no,
                        indent + 1,
                        format!("indentação YAML inconsistente: {indent} espaços após nível {top}"),
                    ));
                }
            } else if indent < top {
                while levels.last().is_some_and(|&l| l > indent) {
                    levels.pop();
                }
                if levels.last() != Some(&indent) {
                    return Some((
                        line_no,
                        indent + 1,
                        format!(
                            "indentação YAML inconsistente: {indent} espaços não alinha com bloco anterior"
                        ),
                    ));
                }
            }
        }

        if is_seq_item && !body.is_empty() && extra_seq_col > 0 && has_mapping {
            levels.push(indent + extra_seq_col);
        }

        if let Some((_, val)) = mapping {
            if is_block_scalar_indicator(val) {
                block_parent_indent = Some(indent);
                block_content_indent = None;
                can_open_child = None;
                continue;
            }
            let opens = val.is_empty() || val.starts_with('&') || val.starts_with('!');
            can_open_child = opens.then_some(if is_seq_item {
                indent + extra_seq_col
            } else {
                indent
            });
        } else if is_seq_item && body.is_empty() {
            can_open_child = Some(indent);
        } else if is_seq_item {
            can_open_child = None;
        }
    }

    if let Some((q, ql, qc)) = quote {
        return Some((ql, qc, format!("aspas `{q}` sem fechamento")));
    }
    if let Some((ch, fl, fc)) = flow.last().copied() {
        return Some((fl, fc, format!("delimitador `{ch}` sem fechamento")));
    }
    None
}

/// Erro se `new` (o conteúdo final de `path`) não é JSON/TOML/YAML válido e o
/// arquivo no disco era válido ou não existia. Outras extensões passam.
pub(crate) fn check(path: &Path, new: &[u8]) -> Result<()> {
    let Some(kind) = kind(path) else {
        return Ok(());
    };
    let Ok(text) = std::str::from_utf8(new) else {
        return Ok(());
    };
    let Some((line, col, msg)) = parse(kind, text) else {
        return Ok(());
    };
    let was_broken = std::fs::read(path)
        .ok()
        .is_some_and(|old| std::str::from_utf8(&old).map_or(true, |o| parse(kind, o).is_some()));
    if was_broken {
        return Ok(());
    }
    let what = match kind {
        Kind::Json => "JSON",
        Kind::Toml => "TOML",
        Kind::Yaml => "YAML",
    };
    Err(ToolError::Failed(format!(
        "{}: {what} inválido na linha {line}, coluna {col}: {msg}. Nada foi gravado; corrija o conteúdo e tente de novo",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let p =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        tempfile::Builder::new().tempdir_in(p).unwrap()
    }

    #[test]
    fn blocks_broken_json_and_toml_unless_already_broken_or_jsonc() {
        let dir = scratch();
        let j = dir.path().join("package.json");
        assert!(check(&j, b"\xef\xbb\xbf{\"a\": 1}").is_ok());
        let err = check(&j, b"{\n  \"a\": 1,\n}").unwrap_err().to_string();
        assert!(err.contains("linha 3, coluna 1"), "{err}");
        assert!(!err.contains(" at line "), "{err}");

        let t = dir.path().join("Cargo.toml");
        assert!(check(&t, b"[a]\nb = 1\n").is_ok());
        let err = check(&t, b"[a]\nb = 1\nc = \n").unwrap_err().to_string();
        assert!(err.contains("TOML inválido na linha 3"), "{err}");

        // O original já quebrado: a edição que o conserta (ou não) passa.
        std::fs::write(&t, "x = ").unwrap();
        assert!(check(&t, b"x = = 2").is_ok());

        // JSONC e extensões desconhecidas não são checados.
        assert!(check(&dir.path().join("tsconfig.app.json"), b"{// c\n}").is_ok());
        assert!(check(&dir.path().join(".vscode/settings.json"), b"{,}").is_ok());
        assert!(check(&dir.path().join("a.jsonc"), b"{,}").is_ok());
        assert!(check(&dir.path().join("a.txt"), b"a: [").is_ok());
    }

    #[test]
    fn validates_yaml_structure_and_allows_valid_yaml() {
        let dir = scratch();
        let y = dir.path().join("deploy.yaml");
        let yml = dir.path().join("ci.yml");

        let valid = "\
---
# comment: not a mapping
name: web-service
url: https://example.com:8080/health
quoted: \"hello: [world\"
image: nginx:latest
items:
- one
- two
nested:
  - name: api
    port: 8080
    ports: [80, 443]
    meta: {env: prod, debug: false}
script: |
  echo \"unclosed in literal block is ok\"
  \tmake build
  key:value_inside_block
";
        assert!(check(&y, valid.as_bytes()).is_ok());

        // 1. Tab characters in indentation
        let err = check(&yml, b"a:\n\tb: 1\n").unwrap_err().to_string();
        assert!(
            err.contains("YAML inválido na linha 2") && err.contains("tabulação"),
            "{err}"
        );

        // 2. Unclosed quotes
        let err = check(&y, b"name: \"unclosed\nport: 80\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("YAML inválido na linha 1") && err.contains("aspas"),
            "{err}"
        );

        // 3. Unclosed or mismatched flow `[` / `{`
        let err = check(&y, b"a: [1, 2\n").unwrap_err().to_string();
        assert!(
            err.contains("YAML inválido na linha 1") && err.contains('['),
            "{err}"
        );
        let err = check(&y, b"a: {x: 1]\n").unwrap_err().to_string();
        assert!(err.contains("YAML inválido na linha 1"), "{err}");

        // 4. Invalid mapping syntax (`key:value` and unexpected colons)
        let err = check(&y, b"server:\n  port:8080\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("YAML inválido na linha 2") && err.contains("chave:valor"),
            "{err}"
        );
        let err = check(&y, b"a: b: c\n").unwrap_err().to_string();
        assert!(
            err.contains("YAML inválido na linha 1") && err.contains("dois-pontos"),
            "{err}"
        );

        // 5. Inconsistent indentation
        let err = check(&y, b"server:\n    host: localhost\n  port: 8080\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("YAML inválido na linha 3") && err.contains("indentação"),
            "{err}"
        );
        let err = check(&y, b"a: 1\n  b: 2\n").unwrap_err().to_string();
        assert!(
            err.contains("YAML inválido na linha 2") && err.contains("indentação"),
            "{err}"
        );

        // Already-broken YAML on disk can still be edited.
        std::fs::write(&yml, "a: [\n").unwrap();
        assert!(check(&yml, b"a: {\n").is_ok());
    }
}
