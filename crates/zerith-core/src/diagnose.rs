//! Why a data folder cannot be written, in words a person can act on.
//!
//! The data folder is the one place the app must always be able to write: the
//! database, the checkpoints and the backups live there. When it is not
//! writable, "Acesso negado" is not an answer. This module names the folder,
//! says what was tried, and points at the likely cause.
//!
//! It only reads and probes. It never edits permissions: that is a decision for
//! the person, who gets the exact commands to review (see [`repair_steps`]).

use std::path::{Path, PathBuf};

/// The result of really trying to use one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderCheck {
    pub dir: PathBuf,
    /// `None`: the folder can be created, written to and cleaned. `Some`: what
    /// failed, in order, and why.
    pub problem: Option<String>,
}

impl FolderCheck {
    pub fn ok(&self) -> bool {
        self.problem.is_none()
    }
}

/// Creates `dir` if needed, writes a file, reads it back and removes it.
/// Each step is reported by name so the failing one is visible.
pub fn probe(dir: &Path) -> FolderCheck {
    let step = |what: &str, e: std::io::Error| FolderCheck {
        dir: dir.to_path_buf(),
        problem: Some(format!("{what}: {e}{}", hint(dir, &e))),
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return step("criar a pasta", e);
    }
    let file = dir.join(format!(".zerith-write-test-{}", std::process::id()));
    if let Err(e) = std::fs::write(&file, b"ok") {
        return step("gravar um arquivo", e);
    }
    let read = std::fs::read(&file);
    let removed = std::fs::remove_file(&file);
    if let Err(e) = read {
        return step("ler de volta", e);
    }
    if let Err(e) = removed {
        return step("apagar o arquivo de teste", e);
    }
    FolderCheck {
        dir: dir.to_path_buf(),
        problem: None,
    }
}

/// A short pointer to the likely cause of `err` at `dir`, or an empty string
/// when there is nothing useful to add.
pub fn hint(dir: &Path, err: &std::io::Error) -> String {
    if err.kind() != std::io::ErrorKind::PermissionDenied {
        return String::new();
    }
    #[cfg(windows)]
    {
        // A denial on a folder that is itself fine usually comes from the
        // *program*: an executable under a Low-integrity label runs as a
        // Low-integrity process, which Windows stops from creating folders.
        if let Some(exe) = win::own_exe_with_low_label() {
            return format!(
                ". Causa provável: este programa ({}) está numa pasta com rótulo de integridade Baixa, deixado por uma ferramenta de sandbox (como o Codex). Um programa assim roda com integridade Baixa e o Windows não o deixa criar pastas. A pasta de dados em si está normal; copie o programa para fora dessa pasta, ou veja `zerith doctor`",
                exe.display()
            );
        }
        if let Some(label) = win::low_integrity_on_chain(dir) {
            return format!(
                ". Causa provável: a pasta {} tem rótulo de integridade Baixa (deixado por uma ferramenta de sandbox, como o Codex)",
                label.display()
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
    }
    ". Verifique as permissões da pasta (e se outro programa a bloqueia)".to_string()
}

/// What the person can do about a Low integrity label, as text to read. The
/// Zerith never runs these: the label sits on a folder another tool set up on
/// purpose, and clearing it is the person's call.
pub fn repair_steps(data_dir: &Path) -> Vec<String> {
    #[cfg(windows)]
    {
        if let Some(exe) = win::own_exe_with_low_label() {
            let folder = win::label_root(&exe).unwrap_or_else(|| exe.clone());
            return vec![
                format!(
                    "O rótulo está em {} (onde o programa roda), não na pasta de dados {}.",
                    folder.display(),
                    data_dir.display()
                ),
                "Opção 1, sem mexer em permissões: copie zerith.exe / zerith-desktop.exe para uma pasta sem esse rótulo (por exemplo, Documentos) e rode de lá."
                    .to_string(),
                format!(
                    "Opção 2, se a ferramenta de sandbox não é mais usada: icacls \"{}\" /setintegritylevel (OI)(CI)M /T",
                    folder.display()
                ),
                "A opção 2 troca o rótulo para Médio (o normal) na pasta e em tudo dentro dela. Não muda dono nem quem pode acessar. A ferramenta de sandbox pode pôr o rótulo de volta na próxima vez que rodar."
                    .to_string(),
            ];
        }
        vec![format!(
            "Veja as permissões de {} com: icacls \"{}\"",
            data_dir.display(),
            data_dir.display()
        )]
    }
    #[cfg(not(windows))]
    {
        vec![format!("ls -ld \"{}\"", data_dir.display())]
    }
}

#[cfg(windows)]
mod win {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The running executable, when it or a folder above it carries a Low
    /// mandatory label. Found by reading `icacls`; nothing is changed.
    pub fn own_exe_with_low_label() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        label_root(&exe).map(|_| exe)
    }

    /// The outermost folder at or above `p` carrying a Low label, i.e. where
    /// the label was set. Inherited copies below it are not the source.
    pub fn label_root(p: &Path) -> Option<PathBuf> {
        let mut root = None;
        let mut cur: Option<&Path> = Some(p);
        while let Some(d) = cur {
            if d.exists() && has_low_label(d) {
                root = Some(d.to_path_buf());
            }
            cur = d.parent();
        }
        // A file's own entry is inherited from its folder: report the folder.
        root.map(|r| {
            if r.is_file() {
                r.parent().map(Path::to_path_buf).unwrap_or(r)
            } else {
                r
            }
        })
    }

    /// The nearest folder at or above `dir` that carries a Low label.
    pub fn low_integrity_on_chain(dir: &Path) -> Option<PathBuf> {
        let mut cur: Option<&Path> = Some(dir);
        while let Some(p) = cur {
            if p.exists() && has_low_label(p) {
                return Some(p.to_path_buf());
            }
            cur = p.parent();
        }
        None
    }

    fn has_low_label(p: &Path) -> bool {
        Command::new("icacls")
            .arg(p)
            .output()
            .map(|o| {
                // `icacls` localizes some words but not the label's name.
                String::from_utf8_lossy(&o.stdout).contains("Low Mandatory Level")
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        // Under the crate's own target dir: the system temp may be restricted.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new().tempdir_in(root).unwrap()
    }

    #[test]
    fn a_normal_folder_passes_and_leaves_nothing_behind() {
        let dir = temp();
        let target = dir.path().join("dados");
        let c = probe(&target);
        assert!(c.ok(), "{:?}", c.problem);
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn a_path_under_a_file_fails_and_names_the_step() {
        let dir = temp();
        let file = dir.path().join("arquivo");
        std::fs::write(&file, "x").unwrap();
        let c = probe(&file.join("sub"));
        let msg = c.problem.expect("must fail");
        assert!(msg.starts_with("criar a pasta"), "{msg}");
    }

    #[test]
    fn ensure_reports_the_folder_it_could_not_create() {
        let dir = temp();
        let file = dir.path().join("arquivo");
        std::fs::write(&file, "x").unwrap();
        let paths = crate::Paths::with_root(file.join("zerith"));
        let e = paths.ensure().unwrap_err().to_string();
        assert!(e.contains("não consegui criar a pasta"), "{e}");
        assert!(e.contains("zerith"), "{e}");
    }

    #[test]
    fn a_permission_hint_is_empty_for_other_errors() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(hint(Path::new("x"), &e), "");
    }

    #[test]
    fn repair_steps_never_run_anything() {
        // Pure text: calling it must not touch the folder.
        let dir = temp();
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let steps = repair_steps(dir.path());
        assert!(!steps.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
    }
}
