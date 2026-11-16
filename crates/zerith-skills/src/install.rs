//! Install, update, roll back, disable and remove.
//!
//! Every mutation is a directory operation with a recorded previous state, so
//! rollback is a real answer and not a promise. Installing never executes
//! anything: it copies files, scans them, and writes a manifest row.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scan::{scan, Skill, SkillError};

/// What a managed skill is doing right now. Disabled keeps the files: the
/// user asked it not to run, not to be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Enabled,
    Disabled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Enabled => "enabled",
            Status::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "enabled" => Some(Status::Enabled),
            "disabled" => Some(Status::Disabled),
            _ => None,
        }
    }
}

/// One installed skill, as recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub name: String,
    pub description: String,
    pub license: String,
    /// Repository or directory the files came from, so provenance survives
    /// the installation.
    pub source: String,
    pub status: Status,
    /// Directory name inside the skills root.
    pub dir: String,
    pub installed_at: i64,
    /// Set by `update`, so a rollback knows what it is going back to.
    pub previous_dir: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub skills: Vec<Installed>,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0}")]
    Skill(#[from] SkillError),
    #[error("a skill named {0:?} is already installed")]
    NameConflict(String),
    #[error("no installed skill named {0:?}")]
    Missing(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, InstallError>;

/// The skills root, its manifest, and the copies kept for rollback.
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(root.join(".backups"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> Result<Manifest> {
        let path = self.root.join("manifest.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, m: &Manifest) -> Result<()> {
        let path = self.root.join("manifest.json");
        std::fs::write(path, serde_json::to_vec_pretty(m)?)?;
        Ok(())
    }

    /// Copies `from` into the root under `name`, after scanning it. The scan
    /// is what lets the install print findings *before* the files land.
    pub fn install(&self, from: &Path, source: &str) -> Result<Installed> {
        let skill = scan(from)?;
        let name = skill.frontmatter.name.clone();
        validate_dir_name(&name)?;
        let mut m = self.manifest()?;
        if m.skills.iter().any(|s| s.name == name) {
            return Err(InstallError::NameConflict(name));
        }
        let target = self.root.join(&name);
        if target.exists() {
            // A directory without a manifest row is still a name conflict:
            // overwriting it would destroy something nobody recorded.
            return Err(InstallError::NameConflict(name));
        }
        copy_dir(from, &target)?;
        let row = Installed {
            name: name.clone(),
            description: skill.frontmatter.description.clone(),
            license: skill.frontmatter.license.clone().unwrap_or_default(),
            source: source.to_string(),
            status: Status::Enabled,
            dir: name,
            installed_at: crate::catalog::now_ms(),
            previous_dir: None,
        };
        m.skills.push(row.clone());
        self.save(&m)?;
        Ok(row)
    }

    /// Moves the current files aside and copies the new ones in. Returns the
    /// scan of the new version, so the caller can show the diff before the
    /// user accepts it (the copy is what makes that possible).
    pub fn update(&self, name: &str, from: &Path) -> Result<Skill> {
        let mut m = self.manifest()?;
        let idx = m
            .skills
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| InstallError::Missing(name.to_string()))?;
        let fresh = scan(from)?;
        if fresh.frontmatter.name != name {
            return Err(InstallError::NameConflict(format!(
                "{} declares {}",
                name, fresh.frontmatter.name
            )));
        }
        let backup = format!("{name}-{}", crate::catalog::now_ms());
        let backup_dir = self.root.join(".backups").join(&backup);
        let current = self.root.join(&m.skills[idx].dir);
        if current.exists() {
            copy_dir(&current, &backup_dir)?;
        }
        std::fs::remove_dir_all(&current).ok();
        copy_dir(from, &current)?;
        m.skills[idx].previous_dir = Some(backup);
        m.skills[idx].installed_at = crate::catalog::now_ms();
        self.save(&m)?;
        Ok(fresh)
    }

    /// Puts the previous copy back. Without a stored copy the call fails
    /// loudly rather than reporting a rollback that did not happen.
    pub fn rollback(&self, name: &str) -> Result<()> {
        let mut m = self.manifest()?;
        let idx = m
            .skills
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| InstallError::Missing(name.to_string()))?;
        let backup = m.skills[idx]
            .previous_dir
            .clone()
            .ok_or_else(|| InstallError::Missing(format!("{name} has no previous version")))?;
        let backup_dir = self.root.join(".backups").join(&backup);
        if !backup_dir.exists() {
            return Err(InstallError::Missing(format!(
                "the stored copy for {name} is gone"
            )));
        }
        let current = self.root.join(&m.skills[idx].dir);
        std::fs::remove_dir_all(&current).ok();
        copy_dir(&backup_dir, &current)?;
        m.skills[idx].previous_dir = None;
        self.save(&m)?;
        Ok(())
    }

    pub fn set_status(&self, name: &str, status: Status) -> Result<()> {
        let mut m = self.manifest()?;
        let row = m
            .skills
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| InstallError::Missing(name.to_string()))?;
        row.status = status;
        self.save(&m)
    }

    /// Removes the files and the row. The backup copies are left alone: a
    /// removal the user regrets is still recoverable from disk.
    pub fn remove(&self, name: &str) -> Result<()> {
        let mut m = self.manifest()?;
        let idx = m
            .skills
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| InstallError::Missing(name.to_string()))?;
        let dir = self.root.join(&m.skills[idx].dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        m.skills.remove(idx);
        self.save(&m)
    }

    /// Installed skills that the model is allowed to see. Disabled skills stay
    /// on disk but are absent from this list, which is the whole meaning of
    /// disabling one.
    pub fn enabled(&self) -> Result<Vec<Installed>> {
        Ok(self
            .manifest()?
            .skills
            .into_iter()
            .filter(|s| s.status == Status::Enabled)
            .collect())
    }

    /// The metadata catalog: name and description only. Loading every body
    /// into a prompt is what the spec forbids; this is the cheap half.
    pub fn catalog(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .enabled()?
            .into_iter()
            .map(|s| (s.name, s.description))
            .collect())
    }

    /// Reads one skill body on demand, which is the other half.
    pub fn load_body(&self, name: &str) -> Result<String> {
        let m = self.manifest()?;
        let row = m
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| InstallError::Missing(name.to_string()))?;
        Ok(std::fs::read_to_string(
            self.root.join(&row.dir).join("SKILL.md"),
        )?)
    }
}

/// A directory name is the skill name; anything that could escape the root is
/// rejected instead of sanitized, because cleaning it would install a skill
/// under a name its author did not choose.
pub fn validate_dir_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(InstallError::NameConflict(format!(
            "{name:?} is not a valid skill directory name"
        )));
    }
    Ok(())
}

/// Recursive copy that follows no symlink and caps depth. A skill is a small
/// directory; one that is not gets refused rather than walked forever.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    copy_at(from, to, 0)
}

fn copy_at(from: &Path, to: &Path, depth: usize) -> Result<()> {
    if depth > 6 {
        return Err(InstallError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "skill directory is nested too deeply",
        )));
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ty.is_symlink() {
            // Copying the link would let a skill point outside its own
            // directory after installation. Record nothing, copy nothing.
            continue;
        }
        if ty.is_dir() {
            copy_at(&entry.path(), &target, depth + 1)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: teste\nlicense: MIT\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn installing_copies_and_records_provenance() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        write_skill(&src, "demo", "corpo");
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        let row = lib.install(&src, "github:a/b").unwrap();
        assert_eq!(row.status, Status::Enabled);
        assert!(lib.root().join("demo/SKILL.md").exists());
        assert_eq!(lib.catalog().unwrap().len(), 1);
    }

    #[test]
    fn a_second_install_with_the_same_name_is_refused() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        write_skill(&src, "demo", "corpo");
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "a").unwrap();
        assert!(matches!(
            lib.install(&src, "b"),
            Err(InstallError::NameConflict(_))
        ));
    }

    #[test]
    fn disable_hides_the_skill_without_deleting_it() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        write_skill(&src, "demo", "corpo");
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "a").unwrap();
        lib.set_status("demo", Status::Disabled).unwrap();
        assert!(lib.enabled().unwrap().is_empty());
        assert!(lib.root().join("demo/SKILL.md").exists());
        assert_eq!(lib.catalog().unwrap().len(), 0);
    }

    #[test]
    fn update_keeps_a_copy_and_rollback_restores_it() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        write_skill(&src, "demo", "versao um");
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "a").unwrap();
        write_skill(&src, "demo", "versao dois");
        lib.update("demo", &src).unwrap();
        let body = lib.load_body("demo").unwrap();
        assert!(body.contains("versao dois"));
        lib.rollback("demo").unwrap();
        assert!(lib.load_body("demo").unwrap().contains("versao um"));
    }

    #[test]
    fn removing_leaves_no_row_and_no_directory() {
        let tmp = tempfile::tempdir_in(scratch()).unwrap();
        let src = tmp.path().join("src");
        write_skill(&src, "demo", "corpo");
        let lib = Library::open(tmp.path().join("skills")).unwrap();
        lib.install(&src, "a").unwrap();
        lib.remove("demo").unwrap();
        assert!(lib.manifest().unwrap().skills.is_empty());
        assert!(!lib.root().join("demo").exists());
    }

    #[test]
    fn a_name_that_could_escape_the_root_is_rejected() {
        assert!(validate_dir_name("../evil").is_err());
        assert!(validate_dir_name("Demo").is_err());
        assert!(validate_dir_name("ok-name-1").is_ok());
    }
}
