//! The data folder, as the window reports it.
//!
//! A database that will not open usually means the folder is not writable, and
//! "unable to open database file" does not say which folder or why. This probes
//! the folders for real (create, write, read back, delete) and returns the
//! cause in words. It never changes permissions.

use std::path::Path;

/// A sentence for the first data folder that cannot be used, or `None` when
/// every folder works (so the original error stands).
pub fn explain(root: &Path) -> Option<String> {
    let paths = zerith_core::Paths::with_root(root);
    let bad = paths.check_writable().into_iter().find(|f| !f.ok())?;
    // Short on purpose: the window says what and why; `zerith doctor` has the
    // steps to review, so a long permissions recipe never fills a screen.
    let cause = if zerith_core::diagnose::repair_steps(root)
        .iter()
        .any(|s| s.contains("rótulo"))
    {
        "O programa está numa pasta marcada por uma ferramenta de sandbox. Copie o Zerith para fora dela (por exemplo, Documentos) e abra de lá."
    } else {
        "Verifique as permissões da pasta."
    };
    Some(format!(
        "Não consigo gravar em {}. {cause} Detalhes e correção: rode `zerith doctor`.",
        bad.dir.display(),
    ))
}

/// `error` with the folder explanation first, when there is one.
pub fn with_cause(root: &Path, error: &str) -> String {
    match explain(root) {
        Some(why) => format!("{why} (erro original: {error})"),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_folder_leaves_the_error_alone() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/ui-ok");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(with_cause(&root, "boom"), "boom");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_cannot_exist_is_named() {
        let file =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp/ui-file");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x").unwrap();
        let msg = with_cause(&file.join("zerith"), "unable to open database file");
        assert!(msg.contains("Não consigo gravar"), "{msg}");
        assert!(msg.contains("unable to open database file"), "{msg}");
        let _ = std::fs::remove_file(&file);
    }
}
