//! Whole-workspace snapshots for rewind, kept in a private "shadow" git
//! repository, so what a shell command changed (`rm`, `sed -i`, `mv`,
//! `git checkout`, a build) is undone too.
//!
//! The user's own `.git` and index are never touched: every git call sets
//! `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE` itself. The repository is
//! `<data root>/checkpoints/.shadow/<hash of the workspace>/`; its `index`
//! persists, so git's stat cache keeps later snapshots fast. A turn's
//! snapshot goes to `<turn dir>/shadow.tree`; a refusal or failure to
//! `<turn dir>/shadow.failed` (the reason, in pt-BR).
//!
//! Ignored files (`.gitignore`, the global excludes and [`EXCLUDE`]) are not
//! in the snapshot: a restore neither brings them back nor deletes them.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const TREE: &str = "shadow.tree";
const FAILED: &str = "shadow.failed";
/// Budget of one snapshot; past it the command runs without one.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(20);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);
/// After a timeout, snapshots of that workspace stay off this long: a killed
/// `git add` never saves its index, so every later turn would pay the whole
/// budget again.
const SLOW_PAUSE: Duration = Duration::from_secs(24 * 60 * 60);
/// Heavy folders nobody wants snapshotted; `.gitignore` files apply on top.
const EXCLUDE: &str = "node_modules/\ntarget/\n.venv/\nvenv/\n__pycache__/\n.gradle/\ndist/\n";

/// Per-workspace lock so concurrent turns in different workspaces never wait
/// for each other's shadow git snapshot or restore.
fn workspace_lock(work: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let abs = std::path::absolute(work).unwrap_or_else(|_| work.to_path_buf());
    let key = plain(std::fs::canonicalize(&abs).unwrap_or(abs));
    let map = LOCKS.get_or_init(Default::default);
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(guard.entry(key).or_default())
}

/// What `shadow.tree` holds.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Snap {
    tree: String,
    pub workspace: PathBuf,
}

/// What restoring a snapshot changes (or would change).
pub(super) struct Outcome {
    /// `(absolute path, existed in the snapshot)`: `false` = it gets deleted.
    pub changes: Vec<(String, bool)>,
    /// After an applied restore: how to get the replaced state back.
    pub undo: Option<String>,
}

/// The turn's snapshot: `None` when the turn mutated nothing,
/// `Some(Err(reason))` when it was refused or failed.
pub(super) fn load(turn_dir: &Path) -> Option<Result<Snap, String>> {
    if let Ok(why) = std::fs::read_to_string(turn_dir.join(FAILED)) {
        return Some(Err(why));
    }
    let text = std::fs::read_to_string(turn_dir.join(TREE)).ok()?;
    Some(serde_json::from_str(&text).map_err(|e| format!("{TREE} inválido: {e}")))
}

/// Snapshots `work` and records the outcome in `turn_dir`. Never fails: the
/// caller's command runs either way.
pub(super) fn snapshot(data_root: &Path, work: &Path, turn_dir: &Path) {
    let snap = match refused(work, data_root) {
        Some(why) => Err(why),
        None => take(data_root, work),
    };
    let written = snap.and_then(|s| {
        let text = serde_json::to_string(&s).map_err(|e| e.to_string())?;
        std::fs::write(turn_dir.join(TREE), text).map_err(|e| e.to_string())
    });
    if let Err(why) = written {
        let _ = std::fs::write(turn_dir.join(FAILED), why);
    }
}

/// Puts the workspace back to `snap` (`apply`), or only lists what that
/// would change.
pub(super) fn restore(data_root: &Path, snap: &Snap, apply: bool) -> Result<Outcome, String> {
    let lock = workspace_lock(&snap.workspace);
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut repo = open(data_root, &snap.workspace, RESTORE_TIMEOUT)?;
    // A scratch copy of the index: same stat cache, the persistent one stays.
    let scratch = repo
        .git_dir
        .join(format!("index.rewind.{}", std::process::id()));
    let _ = std::fs::copy(&repo.index, &scratch);
    repo.index = scratch;
    let out = repo.restore(&snap.tree, apply);
    let _ = std::fs::remove_file(&repo.index);
    out
}

/// Why `work` must not be snapshotted, if it must not.
fn refused(work: &Path, data_root: &Path) -> Option<String> {
    let abs = std::path::absolute(work).ok()?;
    if abs.parent().is_none() {
        return Some("o workspace é a raiz de um drive; snapshot desligado".into());
    }
    let real = std::fs::canonicalize(&abs).ok()?;
    let same = |p: &Path| std::fs::canonicalize(p).is_ok_and(|p| p == real);
    if ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|h| same(Path::new(&h)))
    {
        return Some("o workspace é a pasta pessoal do usuário; snapshot desligado".into());
    }
    // Restoring it would roll back the live database.
    same(data_root).then(|| "o workspace é a pasta de dados do Zerith; snapshot desligado".into())
}

fn take(data_root: &Path, work: &Path) -> Result<Snap, String> {
    let lock = workspace_lock(work);
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let repo = open(data_root, work, SNAPSHOT_TIMEOUT)?;
    let slow = repo.git_dir.join("too-slow");
    let recent = std::fs::metadata(&slow)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < SLOW_PAUSE);
    if recent {
        return Err(format!(
            "o snapshot deste workspace passou de {} s; desligado por 24 h (apague {} para tentar antes)",
            SNAPSHOT_TIMEOUT.as_secs(),
            slow.display()
        ));
    }
    let tree = repo.add_all().and_then(|()| repo.git(&["write-tree"]));
    match tree {
        Ok(t) => Ok(Snap {
            tree: t.trim().to_string(),
            workspace: repo.work,
        }),
        Err(e) => {
            if e.starts_with("tempo esgotado") {
                let _ = std::fs::write(&slow, &e);
            }
            Err(e)
        }
    }
}

/// FNV-1a: a stable folder name per workspace without another crate.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// `\\?\C:\x` as `C:\x`: git does not understand verbatim paths.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

fn open(data_root: &Path, work: &Path, timeout: Duration) -> Result<Repo, String> {
    let work = plain(std::path::absolute(work).map_err(|e| format!("workspace: {e}"))?);
    if !work.is_dir() {
        return Err(format!("workspace não existe: {}", work.display()));
    }
    let key = std::fs::canonicalize(&work).unwrap_or_else(|_| work.clone());
    // Absolute: git runs inside the workspace, where a relative one breaks.
    let root = plain(std::path::absolute(data_root).map_err(|e| e.to_string())?);
    let git_dir = root
        .join("checkpoints")
        .join(".shadow")
        .join(format!("{:016x}", fnv(&key.to_string_lossy())));
    let repo = Repo {
        index: git_dir.join("index"),
        git_dir,
        work,
        timeout,
        deadline: Instant::now() + timeout,
    };
    if !repo.git_dir.join("HEAD").exists() {
        std::fs::create_dir_all(&repo.git_dir).map_err(|e| e.to_string())?;
        repo.git(&["init", "-q", "--template="])?;
        let info = repo.git_dir.join("info");
        std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
        let mut exclude = EXCLUDE.to_string();
        // A data root inside the workspace (the database!) is never restored.
        if let Ok(rel) = root.strip_prefix(&repo.work) {
            exclude.push_str(&format!("/{}/\n", rel.to_string_lossy().replace('\\', "/")));
        }
        std::fs::write(info.join("exclude"), exclude).map_err(|e| e.to_string())?;
        // Bytes as they are on disk: no eol conversion, no LFS or other filter.
        std::fs::write(
            info.join("attributes"),
            "* -text -filter -ident -working-tree-encoding\n",
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(repo)
}

struct Repo {
    git_dir: PathBuf,
    work: PathBuf,
    index: PathBuf,
    timeout: Duration,
    deadline: Instant,
}

impl Repo {
    /// Runs git on the shadow repository: `(exit code, stdout, stderr)`.
    /// Killed at the deadline.
    fn run(&self, args: &[&str]) -> Result<(i32, String, String), String> {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(k);
            }
        }
        c.env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.work)
            .env("GIT_INDEX_FILE", &self.index)
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .args(["-c", "core.autocrlf=false", "-c", "core.safecrlf=false"])
            .args(["-c", "core.longpaths=true", "-c", "core.fsmonitor=false"])
            .args(["-c", "core.quotepath=false", "-c", "gc.auto=0"])
            .args([
                "-c",
                "maintenance.auto=false",
                "-c",
                "safe.directory=*",
                "-c",
            ])
            // The user's global hooksPath must not run here.
            .arg(format!(
                "core.hooksPath={}",
                self.git_dir.join("hooks").display()
            ))
            .args(args)
            .current_dir(&self.work)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = c.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "git não encontrado no PATH".to_string(),
            _ => format!("git: {e}"),
        })?;
        let drain = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut v = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut v);
                }
                String::from_utf8_lossy(&v).into_owned()
            })
        };
        let out = drain(child.stdout.take().map(|s| Box::new(s) as _));
        let err = drain(child.stderr.take().map(|s| Box::new(s) as _));
        let status = loop {
            if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            if Instant::now() >= self.deadline {
                let _ = child.kill();
                let _ = child.wait();
                // A killed `git add` leaves its lock behind, and every later
                // snapshot would fail on it.
                let mut lock = self.index.clone().into_os_string();
                lock.push(".lock");
                let _ = std::fs::remove_file(lock);
                return Err(format!(
                    "tempo esgotado ({} s) em git {}",
                    self.timeout.as_secs(),
                    args[0]
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let (out, err) = (
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default(),
        );
        Ok((status.code().unwrap_or(-1), out, err))
    }

    /// [`Repo::run`], an error unless git exits 0.
    fn git(&self, args: &[&str]) -> Result<String, String> {
        match self.run(args)? {
            (0, out, _) => Ok(out),
            (_, _, err) => Err(format!("git {}: {}", args[0], err.trim())),
        }
    }

    /// The index becomes the workspace as it is now. Files git cannot read
    /// (locked, a nested repository without a commit) are left out instead
    /// of failing the snapshot.
    fn add_all(&self) -> Result<(), String> {
        match self.run(&["add", "-A", "--ignore-errors"])? {
            (0 | 1, _, _) => Ok(()),
            (_, _, err) => Err(format!("git add: {}", err.trim())),
        }
    }

    /// What differs between the workspace and `tree`; with `apply`, puts the
    /// workspace back (git rewrites changed and deleted files, removes the
    /// new ones and the folders they leave empty).
    fn restore(&self, tree: &str, apply: bool) -> Result<Outcome, String> {
        if apply {
            // The old ignore rules first: with a deleted `.gitignore`, every
            // file it ignored (`.env`, a build) would look new and be removed.
            let _ = self.run(&["checkout", tree, "--", ":(glob)**/.gitignore"]);
        }
        self.add_all()?;
        let diff = self.git(&[
            "diff-index",
            "--cached",
            "--no-renames",
            "--name-status",
            "-z",
            tree,
        ])?;
        let mut parts = diff.split('\0').filter(|s| !s.is_empty());
        let mut changes = Vec::new();
        while let (Some(status), Some(rel)) = (parts.next(), parts.next()) {
            let abs = rel.split('/').fold(self.work.clone(), |p, c| p.join(c));
            changes.push((abs.to_string_lossy().into_owned(), !status.starts_with('A')));
        }
        if !apply || changes.is_empty() {
            return Ok(Outcome {
                changes,
                undo: None,
            });
        }
        // Later work the restore overwrites stays reachable: the shadow
        // repository never collects garbage.
        let now = self.git(&["write-tree"])?;
        self.git(&["read-tree", "--reset", "-u", tree])?;
        Ok(Outcome {
            changes,
            undo: Some(format!(
                "o estado substituído ficou guardado; para recuperá-lo: git --git-dir=\"{}\" --work-tree=\"{}\" checkout {} -- .",
                self.git_dir.display(),
                self.work.display(),
                now.trim()
            )),
        })
    }
}
