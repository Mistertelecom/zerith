//! Text-level analysis of a shell command line, for permission rules only.
//!
//! **This is not a security boundary.** It reads the command as text: it
//! splits compound commands, peels common wrappers and looks inside
//! `sh -c "..."` and `$(...)`, but it cannot see aliases, functions, variable
//! expansion, `eval`, scripts the command runs, or anything the shell decides
//! at run time. A rule can be dodged by a command this module cannot parse.
//! The boundary is the OS sandbox (`zerith_security::jail`) and the user's approval;
//! rules are there to save prompts and to catch honest mistakes.

/// A command line broken into the simple commands it would run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Each simple command as written (trimmed).
    pub raw: Vec<String>,
    /// The same commands with wrappers (`timeout`, `env X=1`, `xargs`...)
    /// removed. `raw[i]` and `inner[i]` describe the same command.
    pub inner: Vec<String>,
}

const MAX_DEPTH: usize = 3;

pub fn analyze(cmd: &str) -> Analysis {
    let mut a = Analysis::default();
    collect(cmd, 0, &mut a);
    a
}

fn collect(cmd: &str, depth: usize, a: &mut Analysis) {
    for part in split_compound(cmd) {
        let inner = strip_wrappers(&part);
        if depth < MAX_DEPTH {
            if let Some(script) = nested_script(&inner) {
                collect(&script, depth + 1, a);
            }
        }
        a.raw.push(part);
        a.inner.push(inner);
    }
}

/// Splits on `&&`, `||`, `;`, `|`, `&`, newlines and subshell parentheses
/// outside quotes. `$(...)` and backtick bodies become commands of their own
/// (in double quotes too: the shell runs them there), and the text left behind
/// keeps a `$()` marker so the outer command never looks like it ran nothing.
pub fn split_compound(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    fn flush(cur: &mut String, out: &mut Vec<String>) {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
        cur.clear();
    }
    while i < chars.len() {
        let c = chars[i];
        if quote != Some('\'') {
            if c == '$' && chars.get(i + 1) == Some(&'(') {
                let (inner, next) = matching_paren(&chars, i + 2);
                out.extend(split_compound(&inner));
                cur.push_str("$()");
                i = next;
                continue;
            }
            if c == '`' {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&x| x == '`')
                    .map(|p| i + 1 + p)
                    .unwrap_or(chars.len());
                let inner: String = chars[i + 1..end].iter().collect();
                out.extend(split_compound(&inner));
                cur.push_str("$()");
                i = (end + 1).min(chars.len());
                continue;
            }
        }
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                } else if q == '"' && c == '\\' {
                    if let Some(n) = chars.get(i + 1) {
                        cur.push(*n);
                        i += 1;
                    }
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.get(i + 1) {
                        cur.push(*n);
                        i += 1;
                    }
                }
                // `2>&1` and `&>` are redirections, not separators.
                '&' if cur.ends_with('>') || chars.get(i + 1) == Some(&'>') => cur.push(c),
                '&' | '|' | ';' | '\n' | '\r' | '(' | ')' => flush(&mut cur, &mut out),
                _ => cur.push(c),
            },
        }
        i += 1;
    }
    flush(&mut cur, &mut out);
    out
}

/// Text up to the `)` that closes an already-open `(`, and the index after it.
fn matching_paren(chars: &[char], from: usize) -> (String, usize) {
    let mut depth = 1;
    let mut i = from;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (chars[from..i].iter().collect(), i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    (chars[from.min(chars.len())..].iter().collect(), chars.len())
}

/// Whitespace split that keeps quoted runs together (quotes stay in the token).
fn tokens(part: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in part.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                cur.push(c);
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(t: &str) -> String {
    let b = t.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        let inner = &t[1..t.len() - 1];
        if b[0] == b'"' {
            return inner.replace("\\\"", "\"").replace("\\\\", "\\");
        }
        return inner.to_string();
    }
    t.to_string()
}

/// `C:\bin\Git.EXE` -> `git`.
fn base_name(t: &str) -> String {
    let t = unquote(t);
    let name = t.rsplit(['/', '\\']).next().unwrap_or(&t).to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn is_assignment(t: &str) -> bool {
    match t.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Drops leading `-x` flags; a flag listed in `takes_value` (without `=`)
/// also drops the token after it.
fn skip_flags<'a>(mut t: &'a [String], takes_value: &[&str]) -> &'a [String] {
    while let Some(f) = t.first() {
        if !f.starts_with('-') || f == "-" {
            break;
        }
        let eats = takes_value.contains(&f.as_str());
        t = &t[1..];
        if eats && !t.is_empty() {
            t = &t[1..];
        }
    }
    t
}

/// Removes the wrappers that only change *how* a command runs, so a rule for
/// `cargo test *` also sees `timeout 60 env RUST_LOG=x cargo test -p a`.
/// `sudo` and friends are deliberately kept: they change *who* runs it.
pub fn strip_wrappers(part: &str) -> String {
    let all = tokens(part);
    let mut t: &[String] = &all;
    while let Some(first) = t.first() {
        let rest = &t[1..];
        match base_name(first).as_str() {
            "timeout" => {
                let r = skip_flags(rest, &["-s", "-k", "--signal", "--kill-after"]);
                t = if r.is_empty() { r } else { &r[1..] }; // the duration
            }
            "nice" => t = skip_flags(rest, &["-n", "--adjustment"]),
            "nohup" | "time" | "command" | "builtin" => t = skip_flags(rest, &[]),
            "xargs" => {
                t = skip_flags(
                    rest,
                    &["-I", "-n", "-P", "-d", "-L", "-s", "-E", "-a", "-l"],
                )
            }
            "env" => {
                let mut r = skip_flags(rest, &["-u", "-C", "-S", "--unset", "--chdir"]);
                while r.first().is_some_and(|x| is_assignment(x)) {
                    r = &r[1..];
                }
                t = r;
            }
            _ if is_assignment(first) => t = rest,
            _ => break,
        }
    }
    if t.is_empty() {
        part.trim().to_string()
    } else {
        t.join(" ")
    }
}

/// The script an interpreter would run (`bash -c "..."`, `cmd /c ...`,
/// `powershell -Command ...`), so rules also see what is inside.
fn nested_script(part: &str) -> Option<String> {
    let t = tokens(part);
    let name = base_name(t.first()?);
    match name.as_str() {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            let i = t
                .iter()
                .position(|x| x.len() >= 2 && x.starts_with('-') && x.ends_with('c'))?;
            t.get(i + 1).map(|s| unquote(s))
        }
        "cmd" => {
            let i = t
                .iter()
                .position(|x| x.eq_ignore_ascii_case("/c") || x.eq_ignore_ascii_case("/k"))?;
            Some(unquote(&t[i + 1..].join(" ")))
        }
        "powershell" | "pwsh" => {
            let i = t.iter().position(|x| {
                let x = x.to_lowercase();
                x == "-c" || x == "-command"
            })?;
            Some(unquote(&t[i + 1..].join(" ")))
        }
        _ => None,
    }
}

const READ_ONLY: &[&str] = &[
    "ls", "dir", "cat", "head", "tail", "pwd", "echo", "wc", "which", "where", "whoami", "stat",
    "type", "uname", "hostname", "diff", "cmp", "du", "df", "basename", "dirname", "realpath",
    "grep", "egrep", "fgrep", "rg", "true", "false",
];

const GIT_READ_ONLY: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "blame",
    "shortlog",
    "describe",
];

/// One simple command that cannot write or run other programs, as far as
/// its text shows. A redirection (`>`) always disqualifies it.
pub fn is_read_only(part: &str) -> bool {
    if part.contains('>') {
        return false;
    }
    let t = tokens(part);
    let Some(first) = t.first() else {
        return false;
    };
    let name = base_name(first);
    let args = &t[1..];
    let bad_arg = |prefixes: &[&str]| {
        args.iter()
            .any(|a| prefixes.iter().any(|p| a.starts_with(p)))
    };
    match name.as_str() {
        "git" => {
            // `-c k=v` can name a program to run; `-C dir` only moves.
            let mut a = args;
            while let Some(f) = a.first() {
                match f.as_str() {
                    "--no-pager" => a = &a[1..],
                    "-C" if a.len() >= 2 => a = &a[2..],
                    _ => break,
                }
            }
            match a.first() {
                Some(sub) if GIT_READ_ONLY.contains(&sub.as_str()) => !a[1..]
                    .iter()
                    .any(|x| x.starts_with("--output") || x.starts_with("--ext-diff") || x == "-o"),
                _ => false,
            }
        }
        "rg" | "grep" | "egrep" | "fgrep" => !bad_arg(&["--pre", "--hostname-bin"]),
        n => READ_ONLY.contains(&n),
    }
}

/// Every simple command in `cmd` is read-only (and there is at least one).
pub fn all_read_only(cmd: &str) -> bool {
    let a = analyze(cmd);
    !a.inner.is_empty() && a.inner.iter().all(|p| is_read_only(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_commands_split_outside_quotes_only() {
        assert_eq!(
            split_compound("a && b || c; d | e\nf & g"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(split_compound(r#"echo "a && b" 'c; d'"#).len(), 1);
        assert_eq!(
            split_compound("cargo test 2>&1 | tail"),
            ["cargo test 2>&1", "tail"]
        );
    }

    #[test]
    fn substitutions_and_subshells_are_commands_too() {
        let p = split_compound("echo $(rm -rf x) `id`");
        assert!(p.contains(&"rm -rf x".to_string()), "{p:?}");
        assert!(p.contains(&"id".to_string()), "{p:?}");
        assert!(p.iter().any(|x| x.starts_with("echo")));
        assert_eq!(split_compound("(cd a; make)"), ["cd a", "make"]);
        // Single quotes keep `$(` literal.
        assert_eq!(split_compound("echo '$(rm x)'").len(), 1);
    }

    #[test]
    fn wrappers_are_peeled() {
        assert_eq!(strip_wrappers("timeout 60 cargo test"), "cargo test");
        assert_eq!(strip_wrappers("timeout -s KILL 5 make"), "make");
        assert_eq!(strip_wrappers("nice -n 5 make"), "make");
        assert_eq!(strip_wrappers("env -i A=1 B=2 make -j4"), "make -j4");
        assert_eq!(strip_wrappers("FOO=1 BAR=2 make"), "make");
        assert_eq!(strip_wrappers("xargs -n 1 rm"), "rm");
        assert_eq!(strip_wrappers("timeout 5 env X=1 nice make"), "make");
        assert_eq!(strip_wrappers("sudo make"), "sudo make");
        assert_eq!(strip_wrappers("timeout 5"), "timeout 5");
    }

    #[test]
    fn interpreters_expose_their_script() {
        let a = analyze(r#"bash -c "rm -rf x && ls""#);
        assert!(a.inner.contains(&"rm -rf x".to_string()), "{a:?}");
        let a = analyze("cmd /c del a.txt");
        assert!(a.inner.contains(&"del a.txt".to_string()), "{a:?}");
        let a = analyze("powershell -Command Remove-Item a");
        assert!(a.inner.contains(&"Remove-Item a".to_string()), "{a:?}");
    }

    #[test]
    fn read_only_commands() {
        for ok in [
            "ls -la",
            "cat a.txt | grep x",
            "git status",
            "git -C sub diff --stat",
            "git --no-pager log -n 3",
            "pwd",
            "timeout 5 ls",
            "C:\\tools\\GIT.exe status",
        ] {
            assert!(all_read_only(ok), "{ok}");
        }
        for bad in [
            "ls > out.txt",
            "echo hi >> a",
            "git push",
            "git -c core.pager=evil log",
            "git diff --output=x",
            "git branch -D x",
            "rg --pre ./x foo",
            "cat a && rm a",
            "ls $(rm a)",
            "bash -c ls",
            "cargo test",
            "",
        ] {
            assert!(!all_read_only(bad), "{bad}");
        }
    }
}
