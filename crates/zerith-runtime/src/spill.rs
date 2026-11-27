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
    let (head, tail) = zerith_tools::head_tail(full, SPILL_HEAD_CHARS);
    let lines = full.lines().count();
    let head_lines = head.lines().count();
    let tail_lines = tail.lines().count();
    let skip_start = head_lines + 1;
    let skip_end = lines.saturating_sub(tail_lines);
    let range_note = if skip_start <= skip_end {
        format!(
            "linhas {skip_start}–{skip_end} omitidas; use read_file com offset={skip_start} e limit=200 ou grep"
        )
    } else {
        "meio omitido; use read_file com offset/limit ou grep".to_string()
    };
    let shown = format!(
        "{head}\n\n[saída completa: {} ({lines} linhas), {range_note}]\n\n{tail}",
        path.display()
    );
    Ok((path, shown))
}

/// Room each cut command takes for its note (path, line range, hint).
const CUT_NOTE_CHARS: usize = 600;

/// A terminal output over `cap`, cut per command instead of as one block.
/// `terminal_batch` writes each command as `### <cmd>` and its output; every
/// command keeps the head and tail of its own output, small ones stay whole,
/// and only a command too big for its share (a 4k-line `/ip route print`) has
/// its middle written to a file of its own. The note says which lines are
/// missing, how to read them back (`read_file` with offset) and, better, how
/// to ask the device for less. Output with no `### ` header is one command.
/// Without `root` the middle is only dropped. Returns the text and the files.
pub fn cut_sections(
    root: Option<&Path>,
    session_id: &str,
    call_id: &str,
    full: &str,
    cap: usize,
) -> (String, Vec<PathBuf>) {
    let lines: Vec<&str> = full.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("### "))
        .collect();
    // (header, body) per command; the text before the first header stays.
    type Section<'a> = (Option<&'a str>, &'a [&'a str]);
    let (preamble, sections): (&[&str], Vec<Section>) = if starts.is_empty() {
        (&[], vec![(None, &lines[..])])
    } else {
        let mut v = Vec::new();
        for (k, &s) in starts.iter().enumerate() {
            let end = starts.get(k + 1).copied().unwrap_or(lines.len());
            v.push((Some(lines[s]), &lines[s + 1..end]));
        }
        (&lines[..starts[0]], v)
    };
    let chars = |ls: &[&str]| ls.iter().map(|l| l.chars().count() + 1).sum::<usize>();
    let sizes: Vec<usize> = sections.iter().map(|(_, b)| chars(b)).collect();
    let fixed = chars(preamble)
        + sections
            .iter()
            .map(|(h, _)| h.map_or(0, |h| h.chars().count() + 2))
            .sum::<usize>();
    // Water-filling: the smallest commands take what they need, the rest
    // split what is left evenly.
    let mut budget = cap.saturating_sub(fixed);
    let mut alloc = vec![0usize; sections.len()];
    let mut order: Vec<usize> = (0..sections.len()).collect();
    order.sort_by_key(|&i| sizes[i]);
    for (done, &i) in order.iter().enumerate() {
        let share = budget / (sections.len() - done);
        alloc[i] = if sizes[i] <= share {
            sizes[i]
        } else {
            share.saturating_sub(CUT_NOTE_CHARS)
        };
        budget -= alloc[i].min(budget);
    }
    let mut out: Vec<String> = preamble.iter().map(|l| l.to_string()).collect();
    let mut files = Vec::new();
    for (k, (header, body)) in sections.iter().enumerate() {
        if let Some(h) = header {
            out.push(h.to_string());
        }
        if sizes[k] <= alloc[k] {
            out.extend(body.iter().map(|l| l.to_string()));
            continue;
        }
        let (head_budget, tail_budget) = (alloc[k] * 2 / 3, alloc[k] / 3);
        let take = |it: &mut dyn Iterator<Item = &&str>, budget: usize| {
            let mut used = 0;
            it.take_while(|l| {
                used += l.chars().count() + 1;
                used <= budget
            })
            .count()
        };
        let head = take(&mut body.iter(), head_budget);
        let tail = take(&mut body.iter().rev(), tail_budget).min(body.len() - head);
        let cmd = header.map(|h| h[4..].trim()).unwrap_or("");
        let file = root.and_then(|r| {
            let id = format!("{call_id}-cmd{}", k + 1);
            let p = spill_path(r, session_id, &id);
            std::fs::create_dir_all(p.parent()?).ok()?;
            std::fs::write(&p, body.join("\n")).ok()?;
            Some(p)
        });
        out.extend(body[..head].iter().map(|l| l.to_string()));
        out.push(cut_note(cmd, body.len(), head, tail, file.as_deref()));
        out.extend(body[body.len() - tail..].iter().map(|l| l.to_string()));
        files.extend(file);
    }
    (out.join("\n"), files)
}

/// `[comando X: 4.429 linhas; …]` for a command whose middle was cut.
fn cut_note(cmd: &str, total: usize, head: usize, tail: usize, file: Option<&Path>) -> String {
    let what = if cmd.is_empty() {
        "saída".to_string()
    } else {
        format!("comando `{cmd}`")
    };
    let (first, last) = (head + 1, total - tail);
    let gone = format!("linhas {}–{} omitidas", thousands(first), thousands(last));
    let filter = filter_hint(cmd);
    match file {
        Some(p) => format!(
            "[{what}: {} linhas; {gone}; saída completa: {}; {filter}; para ler o trecho use read_file nesse arquivo com offset={first} e limit (não leia pelo shell)]",
            thousands(total),
            p.display()
        ),
        None => format!(
            "[{what}: {} linhas; {gone}; {filter}]",
            thousands(total)
        ),
    }
}

/// How to ask the device for less instead of reading the whole table.
fn filter_hint(cmd: &str) -> String {
    // RouterOS paths start with `/` (`/ip route print`) or `:` (scripting).
    if cmd.starts_with('/') || cmd.starts_with(':') {
        let base = cmd
            .split_once(" print")
            .map(|(path, _)| format!("{path} print"))
            .unwrap_or_else(|| cmd.to_string());
        format!(
            "filtre no RouterOS em vez de ler tudo: `{base} where …` (ex.: where dst-address~\"^10\\\\.\", where chain=forward, where !dynamic) ou `{base} count-only`"
        )
    } else {
        "filtre no equipamento em vez de ler tudo (`| include …` no Cisco/Huawei, `| match …` no Junos, `| grep …` no Linux)".into()
    }
}

/// `4429` as `4.429`.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
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
pub(crate) mod tests {
    use super::*;

    #[test]
    fn call_ids_cannot_escape_the_root() {
        let p = spill_path(Path::new("r"), "s", "../../etc/passwd");
        assert_eq!(p.parent().unwrap(), Path::new("r").join("s"));
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("______etc_passwd-"), "{name}");
    }

    /// A batch as `terminal_batch` returns it from a MikroTik edge router:
    /// 60 firewall rules, a 4,000-route table, then a small command.
    pub(crate) fn routeros_batch() -> String {
        let mut s = String::from("### /ip firewall filter print terse\n");
        for i in 0..60 {
            let (flag, chain, action) = match i % 4 {
                0 => ("  ", "input", "accept"),
                1 => ("X ", "forward", "drop"),
                2 => ("  ", "forward", "fasttrack-connection"),
                _ => ("  ", "input", "drop"),
            };
            s += &format!(
                "{i:>3} {flag} chain={chain} action={action} protocol=tcp src-address-list=lista-{i} dst-port={} in-interface-list=WAN log=no log-prefix=\"\" comment=\"regra {i}\"\n",
                1000 + i
            );
        }
        s += "\n### /ip route print terse\n";
        for i in 0..4000 {
            s += &format!(
                "{i:>4} D b  dst-address=10.{}.{}.0/24 routing-table=main gateway=179.108.156.{} immediate-gw=179.108.156.{}%sfp-sfpplus1 distance=20 scope=40 target-scope=10\n",
                i / 256,
                i % 256,
                1 + i % 4,
                1 + i % 4
            );
        }
        s += "\n### /system resource print\n   uptime: 12w3d\n  version: 7.15.3 (stable)\n";
        s
    }

    #[test]
    fn a_big_batch_is_cut_per_command_and_keeps_every_firewall_rule() {
        let dir = tempfile::tempdir().unwrap();
        let full = routeros_batch();
        let cap = zerith_tools::TERMINAL_OUTPUT_CAP_CHARS;
        assert!(full.chars().count() > cap);
        let (out, files) = cut_sections(Some(dir.path()), "s", "c1", &full, cap);
        assert!(out.chars().count() <= cap, "{}", out.chars().count());
        // The firewall and the small command arrive whole.
        let fw = out.split("### /ip route").next().unwrap();
        assert_eq!(fw.matches("chain=").count(), 60, "{fw}");
        assert!(out.contains("version: 7.15.3"));
        // Only the route table went to a file, whole, with a note that says
        // how to filter it on the device and how to read the rest.
        assert_eq!(files.len(), 1);
        let file = std::fs::read_to_string(&files[0]).unwrap();
        assert_eq!(file.matches("dst-address=").count(), 4000);
        let note = out
            .lines()
            .find(|l| l.starts_with("[comando `/ip route print terse`: 4.001 linhas;"))
            .unwrap_or_else(|| panic!("{out}"));
        assert!(note.contains("/ip route print where"), "{note}");
        assert!(note.contains("count-only"), "{note}");
        assert!(
            note.contains("read_file") && note.contains("offset="),
            "{note}"
        );
        assert!(note.contains("não leia pelo shell"), "{note}");
        // Head and tail of the table itself.
        assert!(out.contains("dst-address=10.0.0.0/24"));
        assert!(out.contains("dst-address=10.15.159.0/24"));
    }

    #[test]
    fn a_single_command_without_headers_is_one_section() {
        let full = "linha\n".repeat(20_000);
        let (out, files) = cut_sections(None, "s", "c1", &full, 10_000);
        assert!(files.is_empty());
        assert!(out.chars().count() <= 10_000);
        assert!(out.contains("[saída: 20.000 linhas; linhas "), "{out}");
        assert!(out.contains("| include"), "{out}");
    }

    #[test]
    fn a_repeated_call_id_never_overwrites_an_earlier_spill() {
        let a = spill_path(Path::new("r"), "s", "call_0");
        let b = spill_path(Path::new("r"), "s", "call_0");
        assert_ne!(a, b);
    }

    #[test]
    fn spill_reports_exact_omitted_line_range_with_offset_hint() {
        let dir = tempfile::tempdir().unwrap();
        let mut full = String::new();
        for i in 1..=600 {
            full.push_str(&format!(
                "{i}\tlinha de teste com conteudo suficiente numero {i:04}\n"
            ));
        }
        let (path, shown) = spill(dir.path(), "s1", "c1", &full).unwrap();
        assert!(path.is_file());
        assert!(shown.contains("600 linhas"), "{shown}");
        assert!(
            shown.contains("linhas ") && shown.contains("omitidas; use read_file com offset="),
            "{shown}"
        );
    }
}
