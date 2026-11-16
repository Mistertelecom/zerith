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
    let rest = match p {
        "~" => "",
        _ => match p.strip_prefix("~/") {
            Some(r) => r,
            None => return p.to_string(),
        },
    };
    match std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|h| h.to_lowercase().replace('\\', "/"))
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

    /// In `dontAsk` anything that would ask is denied.
    fn dont_ask(&self, v: Verdict) -> Verdict {
        match v {
            Verdict::Ask(why) if self.mode() == Mode::DontAsk => {
                Verdict::Deny(format!("modo dontAsk: {why}"))
            }
            other => other,
        }
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
                if bypass || (inside && mode == Mode::AcceptEdits) {
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
                // Read-only commands never prompt, in any mode.
                if shellparse::all_read_only(cmd) {
                    return Verdict::Allow;
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
        self.dont_ask(v)
    }

    /// Rules only, for a tool call that has no [`Action`] (MCP tools, skills,
    /// plugins): `subject` is the text a specifier matches (a skill name, a
    /// URL, ...). `None`: no rule spoke, the caller's default stands.
    pub fn check_rules(&self, tool: &str, subject: Option<&str>) -> Option<Verdict> {
        let s = Subject::Text(subject.unwrap_or("").to_string());
        self.rule_verdict(tool, &s).map(|v| self.dont_ask(v))
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
                    || a.raw.iter().chain(&a.inner).any(|p| r.text_matches(p))
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
        let allowed = match (subject, &shell) {
            (Subject::Path(p), _) => rules(RuleKind::Allow).any(|r| r.path_matches(p)),
            (Subject::Shell(_), Some(a)) => {
                rules(RuleKind::Allow).any(|r| r.pattern.is_none())
                    || (!a.raw.is_empty()
                        && a.raw.iter().zip(&a.inner).all(|(raw, inner)| {
                            shellparse::is_read_only(inner)
                                || rules(RuleKind::Allow)
                                    .any(|r| r.text_matches(raw) || r.text_matches(inner))
                        }))
            }
            (Subject::Text(t), _) => rules(RuleKind::Allow).any(|r| r.text_matches(t)),
            (Subject::Shell(_), None) => false,
        };
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
            "read_file" | "list_dir" | "glob" | "grep" => {
                Some(Action::Read(path(s("path").unwrap_or("."))))
            }
            "write_file" | "edit_file" => Some(Action::Write(path(s("path")?))),
            "shell" => Some(Action::Shell(s("command")?.to_string())),
            _ => None,
        }
    }

    /// A path normalised for rule matching.
    fn path_subject(&self, p: &Path) -> PathSubject {
        let resolved = self.resolve(p);
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
    /// prefix that exists, then the missing tail re-appended. A missing
    /// directory must not turn into "outside".
    fn resolve(&self, path: &Path) -> PathBuf {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };
        let mut clean = PathBuf::new();
        for c in path.components() {
            match c {
                Component::ParentDir => {
                    clean.pop();
                }
                Component::CurDir => {}
                other => clean.push(other.as_os_str()),
            }
        }
        let mut tail = Vec::new();
        let mut probe = clean.as_path();
        loop {
            if let Ok(mut p) = probe.canonicalize() {
                for part in tail.iter().rev() {
                    p.push(part);
                }
                return p;
            }
            match (probe.file_name(), probe.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name.to_os_string());
                    probe = parent;
                }
                _ => return clean,
            }
        }
    }

    fn inside_workspace(&self, path: &Path) -> bool {
        let Ok(base) = self.workspace.canonicalize() else {
            return false;
        };
        self.resolve(path).starts_with(&base)
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
