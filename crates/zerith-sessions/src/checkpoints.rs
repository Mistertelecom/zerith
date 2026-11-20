//! Checkpoints and rewind: undo what a turn did to files and to the chat.
//!
//! One checkpoint per turn that may edit files. Before a tool mutates a file it
//! calls [`Checkpoint::record_file`]; the first call per path copies the
//! pre-edit bytes into `<data root>/checkpoints/<session>/<turn>/`, or records
//! "did not exist" for a file the turn creates. [`restore`] puts that state back.
//!
//! The first mutating step of a turn (a file tool or a shell command that is
//! not read-only) also snapshots the whole workspace ([`Checkpoint::snapshot`],
//! see `shadow.rs`), so a code restore undoes what the shell changed too.
//!
//! Honest limits:
//! - ignored files (`.gitignore`, `node_modules/`, `target/`...) are outside
//!   the snapshot: only the per-file copies cover them;
//! - a restore overwrites whatever is on disk now, including edits the user
//!   made by hand after the turn;
//! - it is not version control. Keep using git.
//!
//! Conversation rewind is soft: rows after the boundary get `superseded = 1`
//! and disappear from every reader, but stay in the file until
//! [`purge_superseded`] is asked for explicitly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::migrations::now_ms;
use crate::store::Store;

mod shadow;

/// Turns kept per session; older ones lose their saved files.
pub const KEEP_TURNS: i64 = 100;
/// Largest single file a checkpoint will copy.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Largest total copied for one turn.
pub const MAX_TURN_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("banco de dados: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("e/s: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifesto inválido: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error(
        "{path}: {size} bytes excede o limite de {cap} bytes do checkpoint; a edição não foi feita"
    )]
    TooLarge { path: String, size: u64, cap: u64 },
    #[error("o turno {turn} já copiou {used} bytes; limite de {cap} bytes por turno atingido")]
    TurnTooLarge { turn: i64, used: u64, cap: u64 },
    #[error("{0} não é um arquivo regular; checkpoint recusado")]
    NotAFile(String),
    #[error("sem checkpoint para a sessão {session} no turno {turn}")]
    NoCheckpoint { session: String, turn: i64 },
    #[error("caminho não é UTF-8: {0}")]
    BadPath(String),
}

pub type Result<T> = std::result::Result<T, CheckpointError>;

/// What a restore puts back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Files only; the conversation stays as it is.
    Code,
    /// Messages only; files stay as they are.
    Conversation,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// Absolute path as the tool saw it.
    path: String,
    existed: bool,
    /// Blob file name inside the turn directory (only when `existed`).
    blob: Option<String>,
    size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    entries: Vec<Entry>,
    bytes: u64,
}

/// One row of [`list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub turn: i64,
    pub boundary_message_id: i64,
    pub created_at: i64,
    pub files: usize,
    /// Live messages after the boundary: what a conversation rewind removes.
    pub messages_after: i64,
    /// Files of this turn were already put back by a code restore.
    pub code_restored: bool,
}

/// What [`restore`] did, for the CLI and UI to print.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub files_restored: usize,
    /// Files the rewound turns created, now removed.
    pub files_removed: usize,
    pub messages_archived: usize,
    /// Per-file failures; the restore keeps going.
    pub errors: Vec<String>,
    /// Things the person should know (pt-BR): a turn without a workspace
    /// snapshot, where the replaced state was kept.
    pub notes: Vec<String>,
}

/// An open checkpoint for one turn. `Send + Sync`: tools may share it behind an
/// `Arc` and call `record_file` from parallel calls.
pub struct Checkpoint {
    dir: PathBuf,
    data_root: PathBuf,
    pub session: String,
    pub turn: i64,
    state: Mutex<(Manifest, HashSet<String>)>,
    snapped: Once,
}

fn session_dir(data_root: &Path, session: &str) -> PathBuf {
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    data_root.join("checkpoints").join(safe)
}

fn turn_dir(data_root: &Path, session: &str, turn: i64) -> PathBuf {
    session_dir(data_root, session).join(turn.to_string())
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    match std::fs::read_to_string(dir.join("manifest.json")) {
        Ok(t) => Ok(serde_json::from_str(&t)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e.into()),
    }
}

/// The turn number the next checkpoint of `session` should use.
pub fn next_turn(store: &Store, session: &str) -> Result<i64> {
    Ok(store.conn().query_row(
        "SELECT COALESCE(MAX(turn), 0) + 1 FROM checkpoints WHERE session_id = ?1",
        params![session],
        |r| r.get(0),
    )?)
}

impl Checkpoint {
    /// Opens the checkpoint for `turn`. Call it **before** the turn's user
    /// message is appended: the boundary is the newest live message at this
    /// moment, so a conversation rewind to this turn removes the prompt too.
    ///
    /// Also prunes turns older than [`KEEP_TURNS`]. Starting a turn number that
    /// already exists replaces it.
    pub fn begin(store: &Store, data_root: &Path, session: &str, turn: i64) -> Result<Self> {
        let boundary: i64 = store.conn().query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE session_id = ?1 AND superseded = 0",
            params![session],
            |r| r.get(0),
        )?;
        let dir = turn_dir(data_root, session, turn);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        store.conn().execute(
            "INSERT OR REPLACE INTO checkpoints(session_id, turn, boundary_message_id, created_at, rewound)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![session, turn, boundary, now_ms()],
        )?;
        prune(store, data_root, session, turn - KEEP_TURNS)?;
        Ok(Self {
            dir,
            data_root: data_root.to_path_buf(),
            session: session.to_string(),
            turn,
            state: Mutex::new((Manifest::default(), HashSet::new())),
            snapped: Once::new(),
        })
    }

    /// Snapshots `workspace` before the turn's first mutation; later calls
    /// are no-ops. Call it before a shell command that is not read-only and
    /// before a file tool writes. Never fails and never blocks the command:
    /// a refusal (home folder, drive root), a missing git or the 20 s budget
    /// is recorded, and a restore of this turn falls back to the per-file
    /// copies and says so.
    pub fn snapshot(&self, workspace: &Path) {
        self.snapped
            .call_once(|| shadow::snapshot(&self.data_root, workspace, &self.dir));
    }

    /// Saves the current state of `path`, once per file per turn. **Call it
    /// from `write_file` / `edit_file` before mutating**; if it returns an
    /// error, do not make the edit and show the error to the model.
    pub fn record_file(&self, path: &Path) -> Result<()> {
        let abs = std::path::absolute(path)?;
        let key = abs
            .to_str()
            .ok_or_else(|| CheckpointError::BadPath(abs.display().to_string()))?
            .to_string();
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (manifest, seen) = &mut *guard;
        if seen.contains(&key) {
            return Ok(());
        }
        let entry = match std::fs::metadata(&abs) {
            Ok(meta) if meta.is_file() => {
                if meta.len() > MAX_FILE_BYTES {
                    return Err(CheckpointError::TooLarge {
                        path: key,
                        size: meta.len(),
                        cap: MAX_FILE_BYTES,
                    });
                }
                if manifest.bytes + meta.len() > MAX_TURN_BYTES {
                    return Err(CheckpointError::TurnTooLarge {
                        turn: self.turn,
                        used: manifest.bytes,
                        cap: MAX_TURN_BYTES,
                    });
                }
                let blob = manifest.entries.len().to_string();
                std::fs::copy(&abs, self.dir.join(&blob))?;
                manifest.bytes += meta.len();
                Entry {
                    path: key.clone(),
                    existed: true,
                    blob: Some(blob),
                    size: meta.len(),
                }
            }
            Ok(_) => return Err(CheckpointError::NotAFile(key)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Entry {
                path: key.clone(),
                existed: false,
                blob: None,
                size: 0,
            },
            Err(e) => return Err(e.into()),
        };
        manifest.entries.push(entry);
        // Blob first, manifest second: a crash leaves an unreferenced blob,
        // never a manifest pointing at nothing.
        let tmp = self.dir.join("manifest.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&*manifest)?)?;
        std::fs::rename(&tmp, self.dir.join("manifest.json"))?;
        seen.insert(key);
        Ok(())
    }

    /// How many files this turn has recorded so far.
    pub fn recorded(&self) -> usize {
        self.state.lock().map(|g| g.0.entries.len()).unwrap_or(0)
    }
}

fn prune(store: &Store, data_root: &Path, session: &str, up_to_turn: i64) -> Result<()> {
    if up_to_turn < 1 {
        return Ok(());
    }
    store.conn().execute(
        "DELETE FROM checkpoints WHERE session_id = ?1 AND turn <= ?2",
        params![session, up_to_turn],
    )?;
    let root = session_dir(data_root, session);
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let old = e
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i64>().ok())
                .is_some_and(|n| n <= up_to_turn);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

/// The checkpoints of a session, oldest first.
pub fn list(store: &Store, data_root: &Path, session: &str) -> Result<Vec<CheckpointInfo>> {
    let mut st = store.conn().prepare(
        "SELECT turn, boundary_message_id, created_at, rewound FROM checkpoints
         WHERE session_id = ?1 ORDER BY turn",
    )?;
    let rows = st.query_map(params![session], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (turn, boundary_message_id, created_at, code_restored) = row?;
        let files = read_manifest(&turn_dir(data_root, session, turn))
            .map(|m| m.entries.len())
            .unwrap_or(0);
        let messages_after = store.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND id > ?2 AND superseded = 0",
            params![session, boundary_message_id],
            |r| r.get(0),
        )?;
        out.push(CheckpointInfo {
            turn,
            boundary_message_id,
            created_at,
            files,
            messages_after,
            code_restored,
        });
    }
    Ok(out)
}

/// What a restore to `turn` would touch. Read-only.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Preview {
    /// `(path, existed_before)`: `false` means a restore deletes the file.
    /// Includes what shell commands changed when the turn has a snapshot.
    pub files: Vec<(String, bool)>,
    pub messages_after: i64,
    /// Same as [`RestoreReport::notes`].
    pub notes: Vec<String>,
}

/// Shows what [`restore`] would do, without doing it.
pub fn preview(store: &Store, data_root: &Path, session: &str, turn: i64) -> Result<Preview> {
    let boundary: i64 = store
        .conn()
        .query_row(
            "SELECT boundary_message_id FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
            params![session, turn],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => CheckpointError::NoCheckpoint {
                session: session.to_string(),
                turn,
            },
            other => other.into(),
        })?;
    let mut out = Preview {
        messages_after: store.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND id > ?2 AND superseded = 0",
            params![session, boundary],
            |r| r.get(0),
        )?,
        ..Preview::default()
    };
    let turns: Vec<i64> = {
        let mut st = store.conn().prepare(
            "SELECT turn FROM checkpoints WHERE session_id = ?1 AND turn >= ?2 AND rewound = 0 ORDER BY turn",
        )?;
        let rows = st.query_map(params![session, turn], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    // First listed wins, mirroring `restore`: earlier turns over later ones,
    // a turn's snapshot over its own copies. Only the earliest snapshot is
    // diffed: it is the tracked state a restore ends on.
    let (mut seen, mut diffed) = (HashSet::new(), false);
    for t in &turns {
        let dir = turn_dir(data_root, session, *t);
        let mut found = Vec::new();
        match shadow::load(&dir) {
            Some(Ok(snap)) if !diffed => {
                diffed = true;
                match shadow::restore(data_root, &snap, false) {
                    Ok(o) => found = o.changes,
                    Err(e) => out
                        .notes
                        .push(format!("snapshot do workspace (turno {t}) ilegível: {e}")),
                }
            }
            Some(Err(why)) => out.notes.push(no_snapshot(*t, &why)),
            _ => {}
        }
        found.extend(
            read_manifest(&dir)?
                .entries
                .into_iter()
                .map(|e| (e.path, e.existed)),
        );
        for (path, existed) in found {
            if seen.insert(path.clone()) {
                out.files.push((path, existed));
            }
        }
    }
    Ok(out)
}

fn no_snapshot(turn: i64, why: &str) -> String {
    format!(
        "turno {turn} sem snapshot do workspace ({}): só as edições das ferramentas de arquivo voltam; o que o shell mudou fica",
        why.trim()
    )
}

/// Puts the session back to the state **before** `turn` started.
///
/// Code: for every file recorded by `turn` or any later turn that was not
/// already restored, the pre-edit state of the earliest turn wins (the file
/// comes back, or is removed if the turn created it). Conversation: messages
/// after the turn's boundary are marked superseded and the later checkpoints
/// are forgotten. Per-file failures go to the report and do not stop the rest.
pub fn restore(
    store: &Store,
    data_root: &Path,
    session: &str,
    turn: i64,
    mode: Mode,
) -> Result<RestoreReport> {
    let boundary: i64 = store
        .conn()
        .query_row(
            "SELECT boundary_message_id FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
            params![session, turn],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => CheckpointError::NoCheckpoint {
                session: session.to_string(),
                turn,
            },
            other => other.into(),
        })?;
    let mut report = RestoreReport::default();

    if matches!(mode, Mode::Code | Mode::Both) {
        let turns: Vec<i64> = {
            let mut st = store.conn().prepare(
                "SELECT turn FROM checkpoints WHERE session_id = ?1 AND turn >= ?2 AND rewound = 0 ORDER BY turn",
            )?;
            let rows = st.query_map(params![session, turn], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // Newest turn first, each one's copies before its snapshot: the
        // earliest state wins, and a snapshot (taken at the turn's first
        // mutation) wins over copies of the same turn, which can be later
        // (a file the shell deleted and `write_file` then recreated).
        // Copies still cover what a snapshot does not: ignored files and
        // paths outside the workspace.
        // Final outcome per path (restored or removed): the last write wins.
        let mut outcome: std::collections::HashMap<String, bool> = Default::default();
        let mut undo = None;
        for t in turns.iter().rev() {
            let dir = turn_dir(data_root, session, *t);
            for e in read_manifest(&dir)?.entries {
                match put_back(&dir, &e) {
                    Ok(Some(back)) => drop(outcome.insert(e.path, back)),
                    Ok(None) => {}
                    Err(err) => report.errors.push(err),
                }
            }
            match shadow::load(&dir) {
                Some(Ok(snap)) => match shadow::restore(data_root, &snap, true) {
                    Ok(o) => {
                        outcome.extend(o.changes);
                        // The first one holds the state from before the rewind.
                        undo = undo.or(o.undo);
                    }
                    Err(e) => report
                        .errors
                        .push(format!("snapshot do workspace (turno {t}): {e}")),
                },
                Some(Err(why)) => report.notes.push(no_snapshot(*t, &why)),
                None => {}
            }
        }
        report.files_restored = outcome.values().filter(|b| **b).count();
        report.files_removed = outcome.len() - report.files_restored;
        report.notes.extend(undo);
        // Consumed: re-applying these blobs later would clobber newer work.
        for t in &turns {
            let dir = turn_dir(data_root, session, *t);
            if report.errors.is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
            }
            store.conn().execute(
                "UPDATE checkpoints SET rewound = ?3 WHERE session_id = ?1 AND turn = ?2",
                params![session, t, report.errors.is_empty() as i64],
            )?;
        }
    }

    if matches!(mode, Mode::Conversation | Mode::Both) {
        let tx = store.write_tx()?;
        report.messages_archived = tx.execute(
            "UPDATE messages SET superseded = 1 WHERE session_id = ?1 AND id > ?2 AND superseded = 0",
            params![session, boundary],
        )?;
        tx.execute(
            "DELETE FROM checkpoints WHERE session_id = ?1 AND turn >= ?2",
            params![session, turn],
        )?;
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session, now_ms()],
        )?;
        tx.commit()?;
        for e in std::fs::read_dir(session_dir(data_root, session))
            .into_iter()
            .flatten()
            .flatten()
        {
            let later = e
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i64>().ok())
                .is_some_and(|n| n >= turn);
            if later {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(report)
}

/// Puts one recorded file back to its pre-turn state (or removes it when the
/// turn created it): `Some(true)` restored, `Some(false)` removed, `None`
/// nothing to do.
fn put_back(dir: &Path, e: &Entry) -> std::result::Result<Option<bool>, String> {
    let target = PathBuf::from(&e.path);
    let outcome = match (&e.blob, e.existed) {
        (Some(blob), true) => {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::copy(dir.join(blob), &target).map(|_| Some(true))
        }
        _ => match std::fs::remove_file(&target) {
            Ok(()) => Ok(Some(false)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        },
    };
    outcome.map_err(|err| format!("{}: {err}", e.path))
}

/// The checkpoint turn whose final answer is `message_id`, if any.
///
/// The turn is the newest checkpoint opened before the message, provided no
/// other prompt came in between (a turn without tools opens no checkpoint)
/// and the message is the last assistant row before the next prompt (the
/// intermediate tool-call steps do not own the diff). Prompts are user rows
/// without `raw`; steering, hook/tool context and summaries are tagged there.
pub fn turn_for_message(store: &Store, session: &str, message_id: i64) -> Result<Option<i64>> {
    let found: Option<(i64, i64)> = store
        .conn()
        .query_row(
            "SELECT turn, boundary_message_id FROM checkpoints
             WHERE session_id = ?1 AND boundary_message_id < ?2
             ORDER BY boundary_message_id DESC, turn DESC LIMIT 1",
            params![session, message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((turn, boundary)) = found else {
        return Ok(None);
    };
    let (answer, prompts, later): (bool, i64, i64) = store.conn().query_row(
        "SELECT
           EXISTS(SELECT 1 FROM messages WHERE id = ?2 AND session_id = ?1
                  AND role = 'assistant' AND superseded = 0),
           (SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND superseded = 0
              AND role = 'user' AND raw IS NULL AND id > ?3 AND id < ?2),
           (SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND superseded = 0
              AND role = 'assistant' AND id > ?2
              AND id < COALESCE((SELECT MIN(id) FROM messages WHERE session_id = ?1
                  AND superseded = 0 AND role = 'user' AND raw IS NULL AND id > ?2),
                  9223372036854775807))",
        params![session, message_id, boundary],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((answer && prompts <= 1 && later == 0).then_some(turn))
}

/// A file one turn recorded, for a per-turn diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFile {
    /// Absolute path as the tool saw it.
    pub path: String,
    /// Copy of the file from before the turn; `None` = the turn created it.
    pub before: Option<PathBuf>,
    /// What the turn left, known when a later live turn saved the file before
    /// editing it again: `Some(None)` = it did not exist then. `None` = no
    /// later turn touched it; the disk is the best answer.
    pub after: Option<Option<PathBuf>>,
}

/// The files `turn` recorded, with the copies a diff needs. Empty once the
/// turn's files were restored (the copies are consumed).
pub fn turn_files(
    store: &Store,
    data_root: &Path,
    session: &str,
    turn: i64,
) -> Result<Vec<TurnFile>> {
    let dir = turn_dir(data_root, session, turn);
    let mut out: Vec<TurnFile> = read_manifest(&dir)?
        .entries
        .into_iter()
        .map(|e| TurnFile {
            before: e.blob.filter(|_| e.existed).map(|b| dir.join(b)),
            path: e.path,
            after: None,
        })
        .collect();
    let later: Vec<i64> = {
        let mut st = store.conn().prepare(
            "SELECT turn FROM checkpoints WHERE session_id = ?1 AND turn > ?2 AND rewound = 0 ORDER BY turn",
        )?;
        let rows = st.query_map(params![session, turn], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for t in later {
        let dir = turn_dir(data_root, session, t);
        for e in read_manifest(&dir)?.entries {
            if let Some(f) = out
                .iter_mut()
                .find(|f| f.path == e.path && f.after.is_none())
            {
                f.after = Some(e.blob.filter(|_| e.existed).map(|b| dir.join(b)));
            }
        }
    }
    Ok(out)
}

/// Puts back only the files `turn` recorded, each to its state before that
/// turn. Later turns and the conversation stay as they are; the caller checks
/// that a later edit is not about to be overwritten (see [`turn_files`]).
pub fn restore_turn(
    store: &Store,
    data_root: &Path,
    session: &str,
    turn: i64,
) -> Result<RestoreReport> {
    store
        .conn()
        .query_row(
            "SELECT 1 FROM checkpoints WHERE session_id = ?1 AND turn = ?2",
            params![session, turn],
            |_| Ok(()),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => CheckpointError::NoCheckpoint {
                session: session.to_string(),
                turn,
            },
            other => other.into(),
        })?;
    let dir = turn_dir(data_root, session, turn);
    let mut report = RestoreReport::default();
    for e in read_manifest(&dir)?.entries {
        match put_back(&dir, &e) {
            Ok(Some(true)) => report.files_restored += 1,
            Ok(Some(false)) => report.files_removed += 1,
            Ok(None) => {}
            Err(err) => report.errors.push(err),
        }
    }
    // Consumed, as in `restore`: re-applying these copies would clobber work.
    if report.errors.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
    }
    store.conn().execute(
        "UPDATE checkpoints SET rewound = ?3 WHERE session_id = ?1 AND turn = ?2",
        params![session, turn, report.errors.is_empty() as i64],
    )?;
    Ok(report)
}

/// Hard-deletes the messages a conversation rewind set aside. Irreversible;
/// the CLI gates it behind an explicit flag.
pub fn purge_superseded(store: &Store, session: &str) -> Result<usize> {
    Ok(store.conn().execute(
        "DELETE FROM messages WHERE session_id = ?1 AND superseded = 1",
        params![session],
    )?)
}

/// Deletes every saved file of a session (the session itself is untouched).
pub fn delete_all(data_root: &Path, session: &str) -> Result<()> {
    let dir = session_dir(data_root, session);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewMessage, Role, SessionSource};

    fn scratch(tag: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("cp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn add(st: &Store, s: &str, role: Role, raw: Option<serde_json::Value>) -> i64 {
        st.append(
            s,
            &NewMessage {
                raw,
                ..NewMessage::new(role, "x")
            },
        )
        .unwrap()
    }

    #[test]
    fn the_final_answer_of_a_turn_finds_its_checkpoint() {
        let root = scratch("lookup");
        let st = Store::open_in_memory().unwrap();
        let s = st.create_session("t", SessionSource::Native).unwrap().id;
        Checkpoint::begin(&st, &root, &s, 1).unwrap();
        let p1 = add(&st, &s, Role::User, None);
        let step = add(&st, &s, Role::Assistant, None);
        let tool = add(&st, &s, Role::Tool, None);
        let a1 = add(&st, &s, Role::Assistant, None);
        // a turn without tools opens no checkpoint
        add(&st, &s, Role::User, None);
        let a2 = add(&st, &s, Role::Assistant, None);
        Checkpoint::begin(&st, &root, &s, 2).unwrap();
        add(&st, &s, Role::User, None);
        add(
            &st,
            &s,
            Role::User,
            Some(serde_json::json!({"tool_context": true})),
        );
        let a3 = add(&st, &s, Role::Assistant, None);

        assert_eq!(turn_for_message(&st, &s, a1).unwrap(), Some(1));
        assert_eq!(turn_for_message(&st, &s, a3).unwrap(), Some(2));
        assert_eq!(
            turn_for_message(&st, &s, step).unwrap(),
            None,
            "not the final answer"
        );
        assert_eq!(
            turn_for_message(&st, &s, a2).unwrap(),
            None,
            "another prompt in between"
        );
        assert_eq!(
            turn_for_message(&st, &s, p1).unwrap(),
            None,
            "a prompt is not an answer"
        );
        assert_eq!(turn_for_message(&st, &s, tool).unwrap(), None);
        assert_eq!(turn_for_message(&st, "other", a1).unwrap(), None);
    }

    #[test]
    fn restore_turn_puts_back_one_turn_and_reports_what_it_left() {
        let root = scratch("turn");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (a, b) = (work.join("a.txt"), work.join("b.txt"));
        std::fs::write(&a, "v0").unwrap();
        let st = Store::open_in_memory().unwrap();
        let s = st.create_session("t", SessionSource::Native).unwrap().id;

        let t1 = Checkpoint::begin(&st, &root, &s, 1).unwrap();
        t1.record_file(&a).unwrap();
        std::fs::write(&a, "v1").unwrap();
        t1.record_file(&b).unwrap();
        std::fs::write(&b, "novo").unwrap();
        let t2 = Checkpoint::begin(&st, &root, &s, 2).unwrap();
        t2.record_file(&a).unwrap();
        std::fs::write(&a, "v2").unwrap();

        let files = turn_files(&st, &root, &s, 1).unwrap();
        assert_eq!(files.len(), 2);
        let fa = files.iter().find(|f| f.path.ends_with("a.txt")).unwrap();
        assert_eq!(
            std::fs::read_to_string(fa.before.as_ref().unwrap()).unwrap(),
            "v0"
        );
        let left = fa.after.clone().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(left).unwrap(),
            "v1",
            "what turn 1 left"
        );
        let fb = files.iter().find(|f| f.path.ends_with("b.txt")).unwrap();
        assert_eq!((fb.before.clone(), fb.after.clone()), (None, None));
        assert!(turn_files(&st, &root, &s, 2).unwrap()[0].after.is_none());

        let r = restore_turn(&st, &root, &s, 1).unwrap();
        assert_eq!(
            (r.files_restored, r.files_removed, r.errors.len()),
            (1, 1, 0)
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v0");
        assert!(!b.exists());
        assert!(
            turn_files(&st, &root, &s, 1).unwrap().is_empty(),
            "copies consumed"
        );
        let restored: Vec<bool> = list(&st, &root, &s)
            .unwrap()
            .iter()
            .map(|c| c.code_restored)
            .collect();
        assert_eq!(restored, vec![true, false], "turn 2 untouched");
        assert!(matches!(
            restore_turn(&st, &root, &s, 9),
            Err(CheckpointError::NoCheckpoint { .. })
        ));
    }
}
