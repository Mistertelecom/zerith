//! Policy and redaction.
//!
//! Two small things that are easy to get wrong in a way the user cannot see:
//!
//! 1. **A secret that reaches a log is leaked, not protected.** Redaction runs
//!    on the way out, at the point text is assembled, not at the point someone
//!    remembers to call it.
//! 2. **A sandbox is an enforcement level, not a promise.** The policy says
//!    what the host asked for and what it can actually enforce; the gap is
//!    reported, so a partial jail is never described as full.

use std::path::{Path, PathBuf};

/// How much a host can actually confine. Freedom is a level, and a level taken
/// by accident is worse than one taken on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    /// Reads only; writes are denied.
    ReadOnly,
    /// Writes inside the workspace, nothing outside.
    WorkspaceWrite,
    /// No confinement. Only a user's explicit choice reaches here.
    DangerFullAccess,
}

impl Sandbox {
    pub fn as_str(self) -> &'static str {
        match self {
            Sandbox::ReadOnly => "read-only",
            Sandbox::WorkspaceWrite => "workspace-write",
            Sandbox::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "read-only" => Sandbox::ReadOnly,
            "workspace-write" => Sandbox::WorkspaceWrite,
            "danger-full-access" => Sandbox::DangerFullAccess,
            _ => return None,
        })
    }
}

/// Whether the host can enforce the level it reports. Partial is a reported
/// fact, not a failure: a restricted token on Windows can block writes without
/// blocking every read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    Full,
    Partial,
}

impl Enforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Enforcement::Full => "full",
            Enforcement::Partial => "partial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Ask the user every time a write outside or a command is requested.
    Ask,
    /// Never ask; the user accepted the consequence once.
    Never,
}

/// A named bundle of (sandbox, approval). The default is the middle: writes in
/// the workspace, a prompt before anything leaves it.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub sandbox: Sandbox,
    pub approval: Approval,
    pub description: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "read-only",
        sandbox: Sandbox::ReadOnly,
        approval: Approval::Ask,
        description: "leitura apenas; nenhuma escrita e permitida",
    },
    Preset {
        name: "workspace-write",
        sandbox: Sandbox::WorkspaceWrite,
        approval: Approval::Ask,
        description: "escreve dentro do workspace; pede confirmacao para o resto",
    },
    Preset {
        name: "danger-full-access",
        sandbox: Sandbox::DangerFullAccess,
        approval: Approval::Never,
        description: "sem confinamento; use so quando o risco for aceito",
    },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// What a caller asked to do, in the only shapes that matter for policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Read(PathBuf),
    Write(PathBuf),
    /// A shell command, kept whole: classifying it after splitting on spaces
    /// is how a policy gets bypassed with quoting.
    Shell(String),
    Network(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Needs an interactive answer. The reason is shown to the user.
    Ask(String),
    Deny(String),
}

/// How eagerly the agent may act without asking. The preset is the ceiling
/// (what the sandbox permits at all); the mode is how often the user is asked
/// below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Reads inside the workspace run; writes, commands and network ask.
    Default,
    /// Writes and edits inside the workspace run without asking.
    AcceptEdits,
    /// Read-only: writes and shell are denied, not asked.
    Plan,
    /// Everything runs except what the preset or a rule denies.
    Bypass,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::AcceptEdits => "accept-edits",
            Mode::Plan => "plan",
            Mode::Bypass => "bypass",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "default" => Mode::Default,
            "accept-edits" => Mode::AcceptEdits,
            "plan" => Mode::Plan,
            "bypass" => Mode::Bypass,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Allow,
    Ask,
    Deny,
}

/// One `tool(pattern)` rule, e.g. `shell(cargo test*)` or `read_file(*.env)`.
/// A bare `tool` matches every call of that tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub kind: RuleKind,
    pub tool: String,
    pub pattern: Option<String>,
}

impl Rule {
    pub fn parse(kind: RuleKind, text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let (tool, pattern) = match text.split_once('(') {
            Some((tool, rest)) => (tool.trim(), Some(rest.strip_suffix(')')?.to_string())),
            None => (text, None),
        };
        Some(Self {
            kind,
            tool: tool.to_string(),
            pattern,
        })
    }

    /// Paths match on the workspace-relative path or the file name, so
    /// `*.env` catches `config/.env` too. Case-insensitive: on Windows `.ENV`
    /// is the same file, and a deny must not be dodged by casing.
    fn matches(&self, tool: &str, subject: &Subject) -> bool {
        if self.tool != tool {
            return false;
        }
        let Some(pat) = &self.pattern else {
            return true;
        };
        let pat = pat.to_lowercase();
        match subject {
            Subject::Command(c) => glob_match(&pat, &c.to_lowercase(), false),
            Subject::Path(p) => {
                let p = p.to_lowercase();
                let name = p.rsplit('/').next().unwrap_or(&p);
                glob_match(&pat, &p, true) || glob_match(&pat, name, true)
            }
        }
    }

    fn describe(&self) -> String {
        match &self.pattern {
            Some(p) => format!("{}({p})", self.tool),
            None => self.tool.clone(),
        }
    }
}

enum Subject {
    Path(String),
    Command(String),
}

impl Subject {
    fn text(&self) -> &str {
        match self {
            Subject::Path(s) | Subject::Command(s) => s,
        }
    }
}

/// Shell-style glob: `?` one char, `*` any run, `**` any run across `/`.
/// With `path = false`, `*` also crosses `/` (a command is not a path).
///
/// ponytail: plain backtracking, exponential on patterns with many stars;
/// fine for hand-written rules, swap for globset if patterns get large.
pub fn glob_match(pattern: &str, text: &str, path: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_rec(&p, &t, path)
}

fn glob_rec(p: &[char], t: &[char], path: bool) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') => {
            let double = p.get(1) == Some(&'*');
            let rest = if double { &p[2..] } else { &p[1..] };
            // `**/` also matches zero directories.
            if double && rest.first() == Some(&'/') && glob_rec(&rest[1..], t, path) {
                return true;
            }
            for i in 0..=t.len() {
                if glob_rec(rest, &t[i..], path) {
                    return true;
                }
                if i < t.len() && path && !double && t[i] == '/' {
                    return false;
                }
            }
            false
        }
        Some('?') => !t.is_empty() && !(path && t[0] == '/') && glob_rec(&p[1..], &t[1..], path),
        Some(c) => !t.is_empty() && *c == t[0] && glob_rec(&p[1..], &t[1..], path),
    }
}

/// The active policy: one preset, one mode, rules, one workspace root.
#[derive(Debug, Clone)]
pub struct Policy {
    pub preset: &'static Preset,
    pub workspace: PathBuf,
    pub enforcement: Enforcement,
    pub mode: Mode,
    /// Checked in [`Policy::check_tool`]: deny > ask > allow, first match.
    pub rules: Vec<Rule>,
}

impl Policy {
    /// A policy without rules, for hosts that configure it in code.
    pub fn new(preset: &'static Preset, mode: Mode, workspace: impl AsRef<Path>) -> Self {
        Self {
            preset,
            workspace: workspace.as_ref().to_path_buf(),
            // The host has no jail of its own yet; saying partial is the honest
            // report and the reason a preset is never described as a promise.
            enforcement: Enforcement::Partial,
            mode,
            rules: Vec::new(),
        }
    }

    /// The mode a preset implied before modes existed, so `ZERITH_SANDBOX`
    /// alone keeps its old meaning: workspace-write auto-allows writes inside,
    /// danger-full-access never asks.
    pub fn default_mode(preset: &Preset) -> Mode {
        match preset.sandbox {
            Sandbox::ReadOnly => Mode::Default,
            Sandbox::WorkspaceWrite => Mode::AcceptEdits,
            Sandbox::DangerFullAccess => Mode::Bypass,
        }
    }

    /// Builds the policy from the environment, defaulting to the middle preset.
    /// An unknown preset or mode is an error rather than a silent fallback: a
    /// typo must not widen what the agent may do.
    ///
    /// `ZERITH_SANDBOX` picks the preset, `ZERITH_MODE` the mode, and
    /// `ZERITH_ALLOW` / `ZERITH_ASK` / `ZERITH_DENY` hold `;`-separated rules.
    pub fn from_env(workspace: impl AsRef<Path>) -> Result<Self, String> {
        let name = std::env::var("ZERITH_SANDBOX").unwrap_or_else(|_| "workspace-write".into());
        let preset = preset(&name).ok_or_else(|| {
            format!(
                "unknown sandbox preset {name:?}; use one of: {}",
                PRESETS
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let mode = match std::env::var("ZERITH_MODE") {
            Ok(m) => Mode::parse(&m).ok_or_else(|| {
                format!("unknown mode {m:?}; use one of: default, accept-edits, plan, bypass")
            })?,
            Err(_) => Self::default_mode(preset),
        };
        let mut policy = Self::new(preset, mode, workspace);
        for (var, kind) in [
            ("ZERITH_DENY", RuleKind::Deny),
            ("ZERITH_ASK", RuleKind::Ask),
            ("ZERITH_ALLOW", RuleKind::Allow),
        ] {
            if let Ok(list) = std::env::var(var) {
                policy
                    .rules
                    .extend(list.split(';').filter_map(|r| Rule::parse(kind, r)));
            }
        }
        Ok(policy)
    }

    /// Decides one action from preset and mode, without tool rules.
    pub fn check(&self, action: &Action) -> Verdict {
        let bypass = self.mode == Mode::Bypass;
        match action {
            Action::Read(path) => {
                if bypass || self.inside_workspace(path) {
                    Verdict::Allow
                } else {
                    Verdict::Ask(format!("leitura fora do workspace: {}", path.display()))
                }
            }
            Action::Write(path) => {
                if self.preset.sandbox == Sandbox::ReadOnly {
                    return Verdict::Deny("preset read-only: no write is permitted".into());
                }
                if self.mode == Mode::Plan {
                    return Verdict::Deny("modo plan: nenhuma escrita é permitida".into());
                }
                let inside = self.inside_workspace(path);
                if bypass || (inside && self.mode == Mode::AcceptEdits) {
                    Verdict::Allow
                } else if inside {
                    Verdict::Ask(format!("escrever {}", path.display()))
                } else {
                    Verdict::Ask(format!(
                        "{} is outside the workspace {}",
                        path.display(),
                        self.workspace.display()
                    ))
                }
            }
            Action::Shell(cmd) => match self.mode {
                Mode::Plan => Verdict::Deny("modo plan: shell não é permitido".into()),
                Mode::Bypass => Verdict::Allow,
                _ => Verdict::Ask(format!("shell: {cmd}")),
            },
            Action::Network(url) => match self.mode {
                Mode::Bypass => Verdict::Allow,
                _ => Verdict::Ask(format!("network: {url}")),
            },
        }
    }

    /// Decides one tool call: a preset/mode deny is final, then rules
    /// (deny > ask > allow, first match), then the preset/mode verdict.
    pub fn check_tool(&self, tool: &str, action: &Action) -> Verdict {
        let base = self.check(action);
        if matches!(base, Verdict::Deny(_)) {
            return base;
        }
        let subject = match action {
            Action::Read(p) | Action::Write(p) => {
                let shown = p.strip_prefix(&self.workspace).unwrap_or(p);
                Subject::Path(shown.to_string_lossy().replace('\\', "/"))
            }
            Action::Shell(c) | Action::Network(c) => Subject::Command(c.clone()),
        };
        let hit = |kind: RuleKind| {
            self.rules
                .iter()
                .find(|r| r.kind == kind && r.matches(tool, &subject))
        };
        if let Some(r) = hit(RuleKind::Deny) {
            return Verdict::Deny(format!("bloqueado pela regra {}", r.describe()));
        }
        if let Some(r) = hit(RuleKind::Ask) {
            return Verdict::Ask(format!("regra {}: {}", r.describe(), subject.text()));
        }
        if hit(RuleKind::Allow).is_some() {
            return Verdict::Allow;
        }
        base
    }

    fn inside_workspace(&self, path: &Path) -> bool {
        let Ok(base) = self.workspace.canonicalize() else {
            return false;
        };
        // The target, and possibly its parents, may not exist yet. Walk up to
        // the first ancestor that does, resolve *that*, then re-append the
        // missing tail. A missing directory must not turn into "outside".
        let mut tail = Vec::new();
        let mut probe = path;
        let resolved = loop {
            if let Ok(p) = probe.canonicalize() {
                let mut out = p;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                break out;
            }
            match probe.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    match probe.parent() {
                        Some(parent) => probe = parent,
                        None => return false,
                    }
                }
                None => return false,
            }
        };
        resolved.starts_with(&base)
    }
}

/// Replaces anything that looks like a credential with a fixed marker.
///
/// Deliberately blunt: a false positive costs one unreadable log line, a false
/// negative leaks a key into a file the user may share.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let body = token.trim_end();
        let trail = &token[body.len()..];
        out.push_str(&redact_token(body));
        out.push_str(trail);
    }
    out
}

fn redact_token(token: &str) -> String {
    let prefixes = ["sk-", "sk_", "ghp_", "gho_", "xoxb-", "AKIA"];
    if prefixes.iter().any(|p| token.starts_with(p)) && token.len() > 8 {
        return "[redacted]".to_string();
    }
    if token.len() > 40
        && token
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_')
    {
        return "[redacted]".to_string();
    }
    token.to_string()
}

/// Redacts header-shaped text, where the secret follows the name.
pub fn redact_headers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("authorization:")
            || lower.contains("api-key:")
            || lower.contains("x-api-key:")
        {
            out.push_str("[redacted header]\n");
        } else {
            out.push_str(&redact(line));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_replaced_and_ordinary_text_survives() {
        let s = redact("chave sk-f7949b9d5b2e4f1a e texto normal");
        assert_eq!(s, "chave [redacted] e texto normal");
    }

    #[test]
    fn an_authorization_header_never_reaches_a_log() {
        let s = redact_headers("GET /v1\nAuthorization: Bearer abc\nok\n");
        assert!(!s.contains("abc"));
        assert!(s.contains("GET /v1"));
    }

    #[test]
    fn the_default_preset_allows_writes_inside_and_asks_outside() {
        // Not the OS temp dir: it is ACL-restricted on this machine.
        let scratch =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let ws = preset("workspace-write").unwrap();
        let policy = Policy::new(ws, Policy::default_mode(ws), dir.path());
        let inside = dir.path().join("a.txt");
        std::fs::write(&inside, "x").unwrap();
        assert_eq!(policy.check(&Action::Write(inside)), Verdict::Allow);
        assert!(matches!(
            policy.check(&Action::Write(PathBuf::from("C:/Windows/system.ini"))),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn read_only_denies_every_write() {
        let policy = Policy::new(preset("read-only").unwrap(), Mode::Bypass, ".");
        assert!(matches!(
            policy.check(&Action::Write(PathBuf::from("a"))),
            Verdict::Deny(_)
        ));
    }

    fn scratch_dir() -> tempfile::TempDir {
        let scratch =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        tempfile::Builder::new().tempdir_in(scratch).unwrap()
    }

    #[test]
    fn modes_decide_how_often_the_user_is_asked() {
        let dir = scratch_dir();
        let ws = preset("workspace-write").unwrap();
        let file = Action::Write(dir.path().join("a.txt"));
        let read_out = Action::Read(PathBuf::from("C:/Windows/win.ini"));
        let shell = Action::Shell("cargo test".into());

        let p = Policy::new(ws, Mode::Default, dir.path());
        assert!(matches!(p.check(&file), Verdict::Ask(_)));
        assert_eq!(p.check(&Action::Read(dir.path().join("x"))), Verdict::Allow);
        assert!(matches!(p.check(&read_out), Verdict::Ask(_)));

        let p = Policy::new(ws, Mode::AcceptEdits, dir.path());
        assert_eq!(p.check(&file), Verdict::Allow);
        assert!(matches!(p.check(&shell), Verdict::Ask(_)));

        let p = Policy::new(ws, Mode::Plan, dir.path());
        assert!(matches!(p.check(&file), Verdict::Deny(_)));
        assert!(matches!(p.check(&shell), Verdict::Deny(_)));

        let p = Policy::new(ws, Mode::Bypass, dir.path());
        assert_eq!(p.check(&shell), Verdict::Allow);
        assert_eq!(p.check(&read_out), Verdict::Allow);
    }

    #[test]
    fn rules_resolve_deny_before_ask_before_allow() {
        let dir = scratch_dir();
        let mut p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Default,
            dir.path(),
        );
        p.rules = vec![
            Rule::parse(RuleKind::Allow, "shell(cargo *)").unwrap(),
            Rule::parse(RuleKind::Deny, "shell(cargo publish*)").unwrap(),
            Rule::parse(RuleKind::Ask, "read_file(*.env)").unwrap(),
            Rule::parse(RuleKind::Allow, "read_file").unwrap(),
        ];
        let sh = |c: &str| p.check_tool("shell", &Action::Shell(c.into()));
        assert_eq!(sh("cargo test --all"), Verdict::Allow);
        assert!(matches!(sh("cargo publish"), Verdict::Deny(_)));
        assert!(matches!(sh("rm -rf /"), Verdict::Ask(_)));
        let env = Action::Read(dir.path().join("config").join(".ENV"));
        assert!(matches!(p.check_tool("read_file", &env), Verdict::Ask(_)));
        let other = Action::Read(dir.path().join("src").join("main.rs"));
        assert_eq!(p.check_tool("read_file", &other), Verdict::Allow);
    }

    #[test]
    fn an_allow_rule_does_not_lift_a_preset_deny() {
        let mut p = Policy::new(preset("read-only").unwrap(), Mode::Default, ".");
        p.rules = vec![Rule::parse(RuleKind::Allow, "write_file").unwrap()];
        assert!(matches!(
            p.check_tool("write_file", &Action::Write(PathBuf::from("a"))),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn glob_handles_stars_and_double_stars() {
        assert!(glob_match("*.rs", "main.rs", true));
        assert!(!glob_match("*.rs", "src/main.rs", true));
        assert!(glob_match("**/*.rs", "src/a/main.rs", true));
        assert!(glob_match("**/*.rs", "main.rs", true));
        assert!(glob_match("src/?.rs", "src/a.rs", true));
        assert!(glob_match("cargo test*", "cargo test -p x/y", false));
        assert!(!glob_match("cargo test*", "cargo build", false));
    }

    #[test]
    fn an_unknown_preset_is_an_error_not_a_wider_default() {
        std::env::set_var("ZERITH_SANDBOX", "no-such-preset");
        let err = Policy::from_env(".").unwrap_err();
        std::env::remove_var("ZERITH_SANDBOX");
        assert!(err.contains("unknown sandbox preset"));
    }
}
