//! Spill: a tool output too big for the context goes to a file; the model gets
//! the head and the path, and reads the rest on demand.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// How much of a spilled output the model still sees.
pub const SPILL_HEAD_CHARS: usize = 8_000;

/// Spill files older than this are deleted when a turn starts.
pub const SPILL_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// A call id comes from the model: keep it to characters that are safe in a
/// file name, so it can never climb out of the spill directory.
fn safe_name(s: &str) -> String {
    let n: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if n.is_empty() {
        "call".into()
    } else {
        n
    }
}

/// A fresh file for the output of `call_id`. Call ids may repeat (fallback
/// ids, a resumed session), so every call gets its own name: an earlier spill
/// the model was told to read must never be overwritten.
pub fn spill_path(root: &Path, session_id: &str, call_id: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    root.join(safe_name(session_id))
        .join(format!("{}-{ms}-{n}.txt", safe_name(call_id)))
}

/// The directory of one session's spill files.
pub fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    root.join(safe_name(session_id))
}

/// Writes the full output and returns the text the model gets instead.
pub fn spill(
    root: &Path,
    session_id: &str,
    call_id: &str,
    full: &str,
) -> std::io::Result<(PathBuf, String)> {
    let path = spill_path(root, session_id, call_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, full)?;
    let head: String = full.chars().take(SPILL_HEAD_CHARS).collect();
    let lines = full.lines().count();
    let shown = format!(
        "{head}\n\n[saída completa: {} ({lines} linhas); use read_file com offset/limit ou grep]",
        path.display()
    );
    Ok((path, shown))
}

/// Is `p` a file under the spill root? Canonical on both sides, so `..` and
/// symlinks cannot fake it. A path that does not exist is not covered.
pub fn covers(root: &Path, p: &Path) -> bool {
    match (root.canonicalize(), p.canonicalize()) {
        (Ok(r), Ok(p)) => p.starts_with(r),
        _ => false,
    }
}

/// Deletes spill files older than `max_age` and the session directories they
/// leave empty. Errors are ignored: this is housekeeping.
pub fn prune(root: &Path, max_age: Duration) {
    let Ok(sessions) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for s in sessions.flatten() {
        let dir = s.path();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if old {
                let _ = std::fs::remove_file(f.path());
            }
        }
        // Fails (harmlessly) while files remain.
        let _ = std::fs::remove_dir(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_ids_cannot_escape_the_root() {
        let p = spill_path(Path::new("r"), "s", "../../etc/passwd");
        assert_eq!(p.parent().unwrap(), Path::new("r").join("s"));
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("______etc_passwd-"), "{name}");
    }

    #[test]
    fn a_repeated_call_id_never_overwrites_an_earlier_spill() {
        let a = spill_path(Path::new("r"), "s", "call_0");
        let b = spill_path(Path::new("r"), "s", "call_0");
        assert_ne!(a, b);
    }
}
