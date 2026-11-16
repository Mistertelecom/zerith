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

/// The active policy: one preset, one workspace root.
#[derive(Debug, Clone)]
pub struct Policy {
    pub preset: &'static Preset,
    pub workspace: PathBuf,
    pub enforcement: Enforcement,
}

impl Policy {
    /// Builds the policy from the environment, defaulting to the middle preset.
    /// An unknown preset name is an error rather than a silent fallback: a
    /// typo must not widen what the agent may do.
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
        Ok(Self {
            preset,
            workspace: workspace.as_ref().to_path_buf(),
            // The host has no jail of its own yet; saying partial is the honest
            // report and the reason a preset is never described as a promise.
            enforcement: Enforcement::Partial,
        })
    }

    /// Decides one action. Paths are resolved before comparison, so a path
    /// that climbs out with dot-dot cannot look like it belongs to the
    /// workspace.
    pub fn check(&self, action: &Action) -> Verdict {
        match action {
            Action::Read(_) => Verdict::Allow,
            Action::Write(path) => match self.preset.sandbox {
                Sandbox::ReadOnly => {
                    Verdict::Deny("preset read-only: no write is permitted".into())
                }
                Sandbox::WorkspaceWrite => {
                    if self.inside_workspace(path) {
                        Verdict::Allow
                    } else {
                        Verdict::Ask(format!(
                            "{} is outside the workspace {}",
                            path.display(),
                            self.workspace.display()
                        ))
                    }
                }
                Sandbox::DangerFullAccess => Verdict::Allow,
            },
            Action::Shell(cmd) => match self.preset.approval {
                Approval::Ask => Verdict::Ask(format!("shell: {cmd}")),
                Approval::Never => Verdict::Allow,
            },
            Action::Network(url) => match self.preset.approval {
                Approval::Ask => Verdict::Ask(format!("network: {url}")),
                Approval::Never => Verdict::Allow,
            },
        }
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
        let policy = Policy {
            preset: preset("workspace-write").unwrap(),
            workspace: dir.path().to_path_buf(),
            enforcement: Enforcement::Partial,
        };
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
        let policy = Policy {
            preset: preset("read-only").unwrap(),
            workspace: PathBuf::from("."),
            enforcement: Enforcement::Partial,
        };
        assert!(matches!(
            policy.check(&Action::Write(PathBuf::from("a"))),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn an_unknown_preset_is_an_error_not_a_wider_default() {
        std::env::set_var("ZERITH_SANDBOX", "no-such-preset");
        let err = Policy::from_env(".").unwrap_err();
        std::env::remove_var("ZERITH_SANDBOX");
        assert!(err.contains("unknown sandbox preset"));
    }
}
