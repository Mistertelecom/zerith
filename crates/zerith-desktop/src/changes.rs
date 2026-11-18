//! What one answer did to the files: the per-response diff under each answer
//! and its undo. Both read the turn's checkpoint (`zerith_sessions::checkpoints`):
//! "before" is the copy saved before the first edit, "after" is what the turn
//! left (a later turn's copy when one exists, else the file on disk now).

use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use tauri::State;
use zerith_sessions::checkpoints;
use zerith_sessions::Store;

use crate::state::AppState;

type Cmd<T> = Result<T, String>;

/// Files above this get counts of nothing and a note instead of a diff.
const MAX_DIFF_BYTES: usize = 200 * 1024;
/// Context lines around each hunk, like `diff -u`.
const CTX: usize = 3;
// ponytail: LCS table capped at 4M cells (8 MB of u16) after trimming the
// common head/tail; past it the middle shows as one delete + one add block
// (`truncated`). Myers O(ND) if real edits ever hit the cap.
const MAX_CELLS: usize = 4_000_000;
// ponytail: "changed after the turn" for a file no later turn saved = mtime
// newer than the answer row (+ slack for coarse file clocks). A hash stored
// at turn end would be exact; add it when this guesses wrong.
const MTIME_SLACK_MS: i64 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Del,
    Add,
}

/// Line edit script from `a` to `b`, and whether the cap made it coarse.
fn script(a: &[&str], b: &[&str]) -> (Vec<Op>, bool) {
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut ops = vec![Op::Eq; head];
    let (n, m) = (am.len(), bm.len());
    let coarse = n.saturating_mul(m) > MAX_CELLS;
    if coarse {
        ops.extend(std::iter::repeat_n(Op::Del, n));
        ops.extend(std::iter::repeat_n(Op::Add, m));
    } else {
        // l[i][j] = LCS of am[i..] and bm[j..]; walking forward reads it.
        let w = m + 1;
        let mut l = vec![0u16; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                l[i * w + j] = if am[i] == bm[j] {
                    l[(i + 1) * w + j + 1] + 1
                } else {
                    l[(i + 1) * w + j].max(l[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                ops.push(Op::Eq);
                i += 1;
                j += 1;
            } else if j == m || (i < n && l[(i + 1) * w + j] >= l[i * w + j + 1]) {
                ops.push(Op::Del);
                i += 1;
            } else {
                ops.push(Op::Add);
                j += 1;
            }
        }
    }
    ops.extend(std::iter::repeat_n(Op::Eq, tail));
    (ops, coarse)
}

/// `diff -u` style hunks (no file header) plus the added/removed counts.
fn unified(before: &str, after: &str) -> (String, usize, usize, bool) {
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let (ops, coarse) = script(&a, &b);
    let mut pos = Vec::with_capacity(ops.len());
    let (mut i, mut j) = (0, 0);
    for op in &ops {
        pos.push((i, j));
        match op {
            Op::Eq => (i, j) = (i + 1, j + 1),
            Op::Del => i += 1,
            Op::Add => j += 1,
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k] != Op::Eq).collect();
    let added = ops.iter().filter(|o| **o == Op::Add).count();
    let removed = ops.iter().filter(|o| **o == Op::Del).count();
    let mut out = String::new();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(CTX);
        let mut end = changed[k] + 1;
        k += 1;
        while k < changed.len() && changed[k] - end <= 2 * CTX {
            end = changed[k] + 1;
            k += 1;
        }
        let end = (end + CTX).min(ops.len());
        let span = &ops[start..end];
        let old = span.iter().filter(|o| **o != Op::Add).count();
        let new = span.iter().filter(|o| **o != Op::Del).count();
        let (oi, ni) = pos[start];
        let at = |first: usize, count: usize| if count == 0 { first } else { first + 1 };
        out.push_str(&format!(
            "@@ -{},{old} +{},{new} @@\n",
            at(oi, old),
            at(ni, new)
        ));
        for (op, (pi, pj)) in span.iter().zip(&pos[start..end]) {
            let (sign, text) = match op {
                Op::Eq => (' ', a[*pi]),
                Op::Del => ('-', a[*pi]),
                Op::Add => ('+', b[*pj]),
            };
            out.push(sign);
            out.push_str(text);
            out.push('\n');
        }
    }
    (out, added, removed, coarse)
}

/// One file of an answer, as the bar under it draws it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// Absolute path (what undo touches).
    pub path: String,
    /// Relative to the conversation folder when inside it.
    pub rel: String,
    /// `criado` | `alterado` | `apagado`.
    pub status: &'static str,
    pub added: usize,
    pub removed: usize,
    /// Unified hunks; `None` for a binary or oversized file (see `note`).
    pub diff: Option<String>,
    pub note: Option<String>,
    pub truncated: bool,
    /// The file differs from what the turn left; `None` = cannot tell.
    pub changed_since: Option<bool>,
}

fn is_text(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_DIFF_BYTES && !bytes.contains(&0)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// The turn that produced `message_id` and the answer's timestamp.
fn turn_of(st: &Store, session: &str, message_id: i64) -> Cmd<Option<(i64, i64)>> {
    let Some(turn) =
        checkpoints::turn_for_message(st, session, message_id).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let at: Option<i64> = st
        .conn()
        .query_row(
            "SELECT created_at FROM messages WHERE id = ?1 AND session_id = ?2",
            params![message_id, session],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(at.map(|at| (turn, at)))
}

/// What the answer `message_id` changed, file by file.
pub fn changes_for(
    st: &Store,
    root: &Path,
    workspace: &Path,
    session: &str,
    message_id: i64,
) -> Cmd<Vec<FileChange>> {
    let Some((turn, answered_at)) = turn_of(st, session, message_id)? else {
        return Ok(Vec::new());
    };
    let files = checkpoints::turn_files(st, root, session, turn).map_err(|e| e.to_string())?;
    let read = |p: &Path| std::fs::read(p).ok();
    let mut out = Vec::new();
    for f in files {
        let before = f.before.as_deref().and_then(read);
        let disk = read(Path::new(&f.path));
        let (after, changed_since) = match &f.after {
            Some(left) => {
                let left = left.as_deref().and_then(read);
                let moved = left != disk;
                (left, Some(moved))
            }
            None => {
                let since = disk.as_ref().and_then(|_| {
                    mtime_ms(Path::new(&f.path)).map(|t| t > answered_at + MTIME_SLACK_MS)
                });
                (disk, since)
            }
        };
        if before == after {
            continue; // written back unchanged, or never created
        }
        let status = match (&before, &after) {
            (None, _) => "criado",
            (_, None) => "apagado",
            _ => "alterado",
        };
        let (b, a) = (before.unwrap_or_default(), after.unwrap_or_default());
        let (diff, note, added, removed, truncated) = if !is_text(&b) || !is_text(&a) {
            let big = b.len().max(a.len());
            let note = if big > MAX_DIFF_BYTES {
                format!("arquivo grande ({} KB): sem diff", big / 1024)
            } else {
                "arquivo binário: sem diff".to_string()
            };
            (None, Some(note), 0, 0, false)
        } else {
            let (d, added, removed, coarse) =
                unified(&String::from_utf8_lossy(&b), &String::from_utf8_lossy(&a));
            (Some(d), None, added, removed, coarse)
        };
        let ws = std::path::absolute(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let rel = Path::new(&f.path)
            .strip_prefix(&ws)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| f.path.clone());
        out.push(FileChange {
            path: f.path,
            rel,
            status,
            added,
            removed,
            diff,
            note,
            truncated,
            changed_since,
        });
    }
    Ok(out)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoView {
    /// False when it refused: `conflicts` changed after the answer.
    pub done: bool,
    pub conflicts: Vec<String>,
    pub files_restored: usize,
    pub files_removed: usize,
    pub errors: Vec<String>,
}

/// Puts the answer's files back. Without `force` it refuses when a file
/// changed after the answer, so later work is never overwritten silently.
pub fn undo_for(
    st: &Store,
    root: &Path,
    workspace: &Path,
    session: &str,
    message_id: i64,
    force: bool,
) -> Cmd<UndoView> {
    let Some((turn, _)) = turn_of(st, session, message_id)? else {
        return Err("esta resposta não tem alterações para desfazer".into());
    };
    if !force {
        let conflicts: Vec<String> = changes_for(st, root, workspace, session, message_id)?
            .into_iter()
            .filter(|c| c.changed_since == Some(true))
            .map(|c| c.rel)
            .collect();
        if !conflicts.is_empty() {
            return Ok(UndoView {
                conflicts,
                ..UndoView::default()
            });
        }
    }
    let r = checkpoints::restore_turn(st, root, session, turn).map_err(|e| e.to_string())?;
    Ok(UndoView {
        done: true,
        conflicts: Vec::new(),
        files_restored: r.files_restored,
        files_removed: r.files_removed,
        errors: r.errors,
    })
}

/// The files the answer `message_id` changed, with a diff each.
#[tauri::command]
pub async fn turn_changes(
    state: State<'_, AppState>,
    session_id: String,
    message_id: i64,
) -> Cmd<Vec<FileChange>> {
    let (db, root) = (state.db(), state.root().to_path_buf());
    tauri::async_runtime::spawn_blocking(move || {
        let st = Store::open(&db).map_err(|e| e.to_string())?;
        let ws = crate::turn::session_workspace(&db, &session_id);
        changes_for(&st, &root, &ws, &session_id, message_id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Restores the files of the answer `message_id` (the conversation stays).
#[tauri::command]
pub async fn turn_undo(
    state: State<'_, AppState>,
    session_id: String,
    message_id: i64,
    force: Option<bool>,
) -> Cmd<UndoView> {
    let (db, root) = (state.db(), state.root().to_path_buf());
    tauri::async_runtime::spawn_blocking(move || {
        let st = Store::open(&db).map_err(|e| e.to_string())?;
        let ws = crate::turn::session_workspace(&db, &session_id);
        undo_for(
            &st,
            &root,
            &ws,
            &session_id,
            message_id,
            force.unwrap_or(false),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_sessions::checkpoints::Checkpoint;
    use zerith_sessions::{NewMessage, Role, SessionSource};

    #[test]
    fn insert() {
        let (d, a, r, _) = unified("a\nb\nc\n", "a\nb\nX\nc\n");
        assert_eq!((a, r), (1, 0));
        assert_eq!(d, "@@ -1,3 +1,4 @@\n a\n b\n+X\n c\n");
    }

    #[test]
    fn delete() {
        let (d, a, r, _) = unified("1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\n2\n3\n4\n6\n7\n8\n9\n");
        assert_eq!((a, r), (0, 1));
        assert_eq!(d, "@@ -2,7 +2,6 @@\n 2\n 3\n 4\n-5\n 6\n 7\n 8\n");
    }

    #[test]
    fn change_and_far_apart_hunks() {
        let before: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let after: String = (1..=20)
            .map(|i| match i {
                2 => "dois\n".to_string(),
                19 => "dezenove\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let (d, a, r, coarse) = unified(&before, &after);
        assert_eq!((a, r, coarse), (2, 2, false));
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
        assert!(d.starts_with("@@ -1,5 +1,5 @@\n 1\n-2\n+dois\n 3\n"), "{d}");
        assert!(d.contains("-19\n+dezenove\n 20\n"), "{d}");
    }

    #[test]
    fn empty_before() {
        let (d, a, r, _) = unified("", "x\ny\n");
        assert_eq!((a, r), (2, 0));
        assert_eq!(d, "@@ -0,0 +1,2 @@\n+x\n+y\n");
    }

    #[test]
    fn identical() {
        assert_eq!(unified("a\nb\n", "a\nb\n"), (String::new(), 0, 0, false));
    }

    #[test]
    fn past_the_cap_the_middle_is_one_block() {
        let a: Vec<String> = (0..2_100).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..2_100).map(|i| format!("b{i}")).collect();
        let (ops, coarse) = script(
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
            &b.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert!(coarse);
        assert_eq!(ops.len(), 4_200);
    }

    #[test]
    fn an_answer_lists_its_files_and_undo_refuses_after_a_later_edit() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let (leia, notas) = (ws.join("LEIA.txt"), ws.join("notas.txt"));
        std::fs::write(&leia, "linha 1\nlinha 2\n").unwrap();
        let st = Store::open_in_memory().unwrap();
        let s = st.create_session("t", SessionSource::Native).unwrap().id;
        let say = |role, raw| {
            st.append(
                &s,
                &NewMessage {
                    raw,
                    ..NewMessage::new(role, "x")
                },
            )
            .unwrap()
        };

        let cp = Checkpoint::begin(&st, &root, &s, 1).unwrap();
        say(Role::User, None);
        cp.record_file(&leia).unwrap();
        std::fs::write(&leia, "linha 1\nlinha 2 editada\n").unwrap();
        cp.record_file(&notas).unwrap();
        std::fs::write(&notas, "a\nb\nc\n").unwrap();
        let answer = say(Role::Assistant, None);

        let mut got = changes_for(&st, &root, &ws, &s, answer).unwrap();
        got.sort_by(|a, b| a.rel.cmp(&b.rel));
        let summary: Vec<_> = got
            .iter()
            .map(|c| (c.rel.as_str(), c.status, c.added, c.removed))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("LEIA.txt", "alterado", 1, 1),
                ("notas.txt", "criado", 3, 0)
            ]
        );
        assert_eq!(got[0].changed_since, Some(false));

        // a later turn edits LEIA.txt again: undo must ask first
        let cp2 = Checkpoint::begin(&st, &root, &s, 2).unwrap();
        say(Role::User, None);
        cp2.record_file(&leia).unwrap();
        std::fs::write(&leia, "outra coisa\n").unwrap();
        say(Role::Assistant, None);
        let got = changes_for(&st, &root, &ws, &s, answer).unwrap();
        let l = got.iter().find(|c| c.rel == "LEIA.txt").unwrap();
        assert_eq!(
            (l.changed_since, l.added, l.removed),
            (Some(true), 1, 1),
            "diff is what turn 1 left"
        );
        let refused = undo_for(&st, &root, &ws, &s, answer, false).unwrap();
        assert_eq!(
            (refused.done, refused.conflicts.clone()),
            (false, vec!["LEIA.txt".to_string()])
        );
        assert!(notas.exists(), "refusal touches nothing");

        let done = undo_for(&st, &root, &ws, &s, answer, true).unwrap();
        assert_eq!(
            (done.done, done.files_restored, done.files_removed),
            (true, 1, 1)
        );
        assert_eq!(
            std::fs::read_to_string(&leia).unwrap(),
            "linha 1\nlinha 2\n"
        );
        assert!(!notas.exists());
        assert!(changes_for(&st, &root, &ws, &s, answer).unwrap().is_empty());
        assert!(undo_for(&st, &root, &ws, &s, answer + 99, false).is_err());
    }
}
