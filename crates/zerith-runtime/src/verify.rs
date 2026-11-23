//! Did the turn check the code it edited? Not by keywords ("test" anywhere
//! in a command matched `cat test.log`, and `python app.py` passed for a
//! test): after Hermes Agent's verification ledger (MIT), a command counts
//! only when
//!
//! 1. it is one of the project's own verification commands, detected from
//!    its files (`cargo test`, `npm run test`, `pytest`, `make check`...), or,
//!    when the project declares none, it runs a file the turn just edited or a
//!    test file;
//! 2. its exit status is that command's own (last of a `;` list, an `&&`
//!    chain that succeeded; never behind `|`, `||` or `&`);
//! 3. it passed (exit 0).
//!
//! Idea and rules from Hermes (`verification_evidence.py`,
//! `coding_context.py`); the code is Zerith's.

use std::path::{Path, PathBuf};

/// package.json scripts / Makefile targets that verify.
const TARGETS: &[&str] = &["test", "tests", "lint", "typecheck", "check", "build"];

/// The project's verification commands, from the files at `root`.
pub(crate) fn project_commands(root: &Path) -> Vec<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let has = |f: &str| root.join(f).is_file();
    let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap_or_default();
    if has("Cargo.toml") {
        out.extend(
            [
                "cargo test",
                "cargo check",
                "cargo build",
                "cargo clippy",
                "cargo nextest run",
            ]
            .map(String::from),
        );
    }
    if has("package.json") {
        let pm = [
            ("pnpm-lock.yaml", "pnpm"),
            ("bun.lockb", "bun"),
            ("bun.lock", "bun"),
            ("yarn.lock", "yarn"),
        ]
        .iter()
        .find(|(lock, _)| has(lock))
        .map_or("npm", |(_, pm)| pm);
        let scripts: Vec<String> = serde_json::from_str::<serde_json::Value>(&read("package.json"))
            .ok()
            .and_then(|v| {
                v["scripts"]
                    .as_object()
                    .map(|o| o.keys().cloned().collect())
            })
            .unwrap_or_default();
        for t in TARGETS {
            if scripts.iter().any(|s| s == t) {
                out.push(format!("{pm} run {t}"));
                if *t == "test" {
                    out.push(format!("{pm} test"));
                }
            }
        }
        if has("tsconfig.json") {
            out.push("npx tsc".into());
            out.push("tsc".into());
        }
    }
    let pyproject = read("pyproject.toml");
    if has("pytest.ini")
        || pyproject.contains("[tool.pytest")
        || has("conftest.py")
        || root.join("tests").is_dir()
            && (has("setup.py") || has("pyproject.toml") || has("requirements.txt"))
    {
        out.extend(
            [
                "pytest",
                "python -m pytest",
                "python3 -m pytest",
                "py -m pytest",
            ]
            .map(String::from),
        );
    }
    if has("pyproject.toml") || has("setup.py") || has("requirements.txt") {
        out.extend(
            [
                "python -m unittest",
                "python3 -m unittest",
                "ruff check",
                "mypy",
            ]
            .map(String::from),
        );
    }
    if has("go.mod") {
        out.extend(["go test", "go build", "go vet"].map(String::from));
    }
    if has("pom.xml") {
        out.extend(["mvn test", "mvn verify", "mvn package"].map(String::from));
    }
    if has("build.gradle") || has("build.gradle.kts") {
        out.extend(
            [
                "gradle test",
                "./gradlew test",
                "gradlew test",
                "gradle build",
                "./gradlew build",
            ]
            .map(String::from),
        );
    }
    let makefile = read("Makefile");
    for t in TARGETS {
        if makefile.lines().any(|l| l.starts_with(&format!("{t}:"))) {
            out.push(format!("make {t}"));
        }
    }
    if has("scripts/run_tests.sh") {
        out.push("scripts/run_tests.sh".into());
    }
    let mut seen = Vec::new();
    for c in out {
        let toks = tokens(&c);
        if !seen.contains(&toks) {
            seen.push(toks);
        }
    }
    seen
}

/// The project the turn works in: the workspace itself, when it holds a
/// manifest. Never a folder above it: a scratch folder inside another repo
/// (or a temp dir under `target/`) must not inherit that repo's commands.
pub(crate) fn project_root(workspace: &Path) -> Option<PathBuf> {
    const MARKERS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "Makefile",
    ];
    MARKERS
        .iter()
        .any(|m| workspace.join(m).exists())
        .then(|| workspace.to_path_buf())
}

/// One simple command of a shell line, and the operator that follows it.
#[derive(Debug, PartialEq)]
struct Segment {
    tokens: Vec<String>,
    next: Option<&'static str>,
}

/// Splits `line` on `;`, `&&`, `||`, `|`, `&` and newlines outside quotes.
fn segments(line: &str) -> Vec<Segment> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    let push = |cur: &mut String, next: Option<&'static str>, out: &mut Vec<Segment>| {
        let t = tokens(cur);
        if !t.is_empty() {
            out.push(Segment { tokens: t, next });
        } else if let (Some(n), Some(last)) = (next, out.last_mut()) {
            last.next = Some(n);
        }
        cur.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' | '\n' => push(&mut cur, Some(";"), &mut out),
                '&' if chars.get(i + 1) == Some(&'&') => {
                    push(&mut cur, Some("&&"), &mut out);
                    i += 1;
                }
                '|' if chars.get(i + 1) == Some(&'|') => {
                    push(&mut cur, Some("||"), &mut out);
                    i += 1;
                }
                '|' => push(&mut cur, Some("|"), &mut out),
                '&' if cur.ends_with('>') || chars.get(i + 1) == Some(&'>') => cur.push(c),
                '&' => push(&mut cur, Some("&"), &mut out),
                _ => cur.push(c),
            },
        }
        i += 1;
    }
    push(&mut cur, None, &mut out);
    if let Some(last) = out.last_mut() {
        if last.next == Some(";") {
            last.next = None;
        }
    }
    out
}

/// Words of one simple command: quotes removed, `./` dropped, env
/// assignments and wrapper programs (`timeout 60`, `env`, `sudo`) skipped.
fn tokens(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut w: Vec<String> = words
        .into_iter()
        .map(|t| t.trim_start_matches("./").replace('\\', "/"))
        .collect();
    loop {
        match w.first().map(String::as_str) {
            Some(first) if first.contains('=') && !first.starts_with('-') => {
                w.remove(0);
            }
            Some("env" | "sudo" | "time" | "nice" | "command") => {
                w.remove(0);
            }
            Some("timeout") => {
                w.remove(0);
                if w.first()
                    .is_some_and(|d| d.chars().next().is_some_and(|c| c.is_ascii_digit()))
                {
                    w.remove(0);
                }
            }
            _ => break,
        }
    }
    // `npm.cmd`, `cargo.exe`, `C:/x/python.exe` read as the plain program.
    if let Some(first) = w.first_mut() {
        let name = first.rsplit('/').next().unwrap_or(first).to_string();
        let lower = name.to_ascii_lowercase();
        let bare = [".exe", ".cmd", ".bat"]
            .iter()
            .find_map(|e| lower.strip_suffix(e).map(str::to_string))
            .unwrap_or(lower);
        if bare != *first {
            *first = bare;
        }
    }
    w
}

/// Whether the shell's exit status is this segment's own (Hermes' rule).
fn attributable(segs: &[Segment], i: usize, exit: i64) -> bool {
    if segs.iter().any(|s| s.next == Some("&")) {
        return false;
    }
    let start = segs[..segs.len().saturating_sub(1)]
        .iter()
        .enumerate()
        .filter(|(_, s)| s.next == Some(";"))
        .map(|(k, _)| k + 1)
        .max()
        .unwrap_or(0);
    if i < start {
        return false;
    }
    let ops: Vec<&str> = segs[start..segs.len() - 1]
        .iter()
        .filter_map(|s| s.next)
        .collect();
    if ops.iter().any(|o| matches!(*o, "|" | "||")) {
        return false;
    }
    ops.is_empty() || (exit == 0 && ops.iter().all(|o| *o == "&&"))
}

const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "py",
    "node",
    "bun",
    "deno",
    "bash",
    "sh",
    "pwsh",
    "powershell",
    "ruby",
    "perl",
    "go",
];

/// A test file by its name, or one of `edited`.
fn is_target(arg: &str, edited: &[String]) -> bool {
    let a = arg.replace('\\', "/");
    let name = a.rsplit('/').next().unwrap_or(&a).to_ascii_lowercase();
    name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || a.split('/')
            .any(|p| matches!(p, "tests" | "test" | "__tests__" | "spec"))
        || edited.iter().any(|e| {
            let e = e.replace('\\', "/");
            e == a || e.ends_with(&format!("/{a}")) || a.ends_with(&format!("/{e}"))
        })
}

/// Whether `line` (exit status `exit`) checked the code: a project command
/// or, when the project has none, a script that is a test file or one the
/// turn edited.
pub(crate) fn is_verification(
    line: &str,
    exit: i64,
    project: &[Vec<String>],
    edited: &[String],
) -> bool {
    if exit != 0 {
        return false;
    }
    let segs = segments(line);
    segs.iter().enumerate().any(|(i, s)| {
        if !attributable(&segs, i, exit) {
            return false;
        }
        let t = &s.tokens;
        if project
            .iter()
            .any(|p| t.len() >= p.len() && t[..p.len()] == p[..])
        {
            return true;
        }
        if !project.is_empty() {
            return false;
        }
        let Some(first) = t.first() else { return false };
        if is_target(first, edited) {
            return true;
        }
        if INTERPRETERS.contains(&first.as_str()) {
            // `go run x.go` / `go test` read past the subcommand.
            let rest = if first == "go" {
                t.get(2..).unwrap_or_default()
            } else {
                &t[1..]
            };
            if first == "go" && t.get(1).is_some_and(|s| s == "test") {
                return true;
            }
            if let Some(script) = rest.iter().find(|a| !a.starts_with('-')) {
                return is_target(script, edited);
            }
        }
        false
    })
}

/// The exit status a `shell` result reports on its first line (`exit N`).
pub(crate) fn exit_of(output: &str) -> Option<i64> {
    output
        .lines()
        .next()?
        .trim()
        .strip_prefix("exit ")?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(list: &[&str]) -> Vec<Vec<String>> {
        list.iter().map(|c| tokens(c)).collect()
    }

    #[test]
    fn project_commands_count_and_lookalikes_do_not() {
        let p = cmds(&["cargo test", "npm run test", "pytest"]);
        let ok = |l: &str| is_verification(l, 0, &p, &[]);
        assert!(ok("cargo test -q -p x"));
        assert!(ok("cd crates/x && cargo test"));
        assert!(ok("RUST_LOG=1 timeout 300 cargo test"));
        assert!(ok("npm.cmd run test"));
        assert!(ok("pytest tests/test_calc.py -x"));
        // The old keyword list said yes to all of these.
        assert!(!ok("cat test.log"));
        assert!(!ok("grep -r test src"));
        assert!(!ok("cargo test | tail -5"), "a pipe hides the status");
        assert!(!ok("cargo test || true"));
        assert!(!ok("cargo test & echo"));
        assert!(!ok("cargo test; echo feito"), "the status is echo's");
        assert!(!is_verification("cargo test", 101, &p, &[]), "failed");
        assert!(!ok("echo 'cargo test'"));
    }

    #[test]
    fn without_project_commands_only_tests_or_edited_scripts_count() {
        let edited = vec!["app/calc.py".to_string()];
        let ok = |l: &str| is_verification(l, 0, &[], &edited);
        assert!(ok("python app/calc.py"));
        assert!(ok("python3 -u tests/test_calc.py"));
        assert!(ok("node calc.test.js"));
        assert!(ok("go test ./..."));
        assert!(!ok("python outro.py"), "a script the turn did not touch");
        assert!(!ok("python -c 'print(1)'"));
        assert!(!ok("ls"));
    }

    #[test]
    fn a_bare_script_name_matches_the_bare_edited_path() {
        let edited = vec!["calc.py".to_string()];
        assert!(is_verification("python calc.py", 0, &[], &edited));
    }

    #[test]
    fn segments_and_status_attribution() {
        let s = segments("cd a && cargo test; echo x | grep y");
        assert_eq!(s.len(), 4);
        assert_eq!(s[1].next, Some(";"));
        assert!(!attributable(&s, 1, 0));
        assert!(attributable(&segments("cd a && cargo test"), 1, 0));
        assert!(
            !attributable(&segments("cd a && cargo test"), 1, 1),
            "a failed chain proves nothing"
        );
        assert_eq!(exit_of("exit 0\nok"), Some(0));
        assert_eq!(exit_of("exit 101\n"), Some(101));
        assert_eq!(exit_of("sem cabeçalho"), None);
    }

    #[test]
    fn project_detection_reads_the_manifests() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(d.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(d.path().join("Makefile"), "check:\n\tgo vet\n").unwrap();
        let c = project_commands(d.path());
        assert!(c.contains(&tokens("pnpm run test")) && c.contains(&tokens("pnpm test")));
        assert!(!c.contains(&tokens("pnpm run dev")));
        assert!(c.contains(&tokens("make check")));
        assert_eq!(project_root(d.path()).as_deref(), Some(d.path()));
        // A folder without a manifest is not a project, even inside one.
        let sub = d.path().join("rascunho");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_root(&sub), None);
    }
}
