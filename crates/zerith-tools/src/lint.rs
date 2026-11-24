//! Recusa gravar `.json` / `.toml` quebrado (lint fail-closed, ideia do
//! Hermes, MIT): o modelo que esquece uma vírgula num `package.json` só
//! descobre quando o build cai, e aí já não lembra o que mexeu.
//!
//! Arquivo que já estava inválido passa: bloquear impediria consertá-lo.
//! ponytail: YAML fica de fora, o workspace não tem crate de YAML (decisão
//! no Cargo.toml); entra quando um parser de YAML entrar por outro motivo.

use std::path::Path;

use crate::{Result, ToolError};

#[derive(Clone, Copy)]
enum Kind {
    Json,
    Toml,
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
    }
}

/// Erro se `new` (o conteúdo final de `path`) não é JSON/TOML válido e o
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
        assert!(check(&dir.path().join("a.yaml"), b"a: [").is_ok());
    }
}
