//! OS containment for the `shell` tool, and an honest report of what it does.
//!
//! What is enforced, per platform (see `docs/sandbox.md`):
//!
//! | Platform | Mechanism | Verified |
//! | --- | --- | --- |
//! | Windows | Job Object: kill-on-close, process limit, memory limit, UI limits | yes, by tests in this file |
//! | macOS | `sandbox-exec` profile: writes confined to the workspace/temp | **no** (written without a Mac) |
//! | Linux | process group only; Landlock is planned | no |
//!
//! None of these is a full jail: reads are not confined anywhere, the network
//! is not confined anywhere, and on Windows file writes are not confined at
//! all. [`report`] says so, and `Policy::enforcement` stays `Partial`.
//!
//! API for the tools crate: build a [`Jail`] from the policy
//! ([`Jail::for_policy`]), then either
//! - [`Jail::spawn`] (preferred; the only way to get the Windows Job Object,
//!   because a job has to be attached to the process *after* creation), or
//! - [`Jail::wrap`], which returns a `Command` with what can be applied before
//!   spawning (macOS profile, Unix process group). Call it before configuring
//!   stdio.

use crate::{Enforcement, Policy, Sandbox};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Resource limits of one jailed process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most processes alive in the tree at once (a fork bomb stops here).
    pub max_processes: u32,
    /// Most committed memory for the whole tree, in bytes.
    pub memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_processes: 128,
            memory_bytes: 4 << 30,
        }
    }
}

/// What the current platform can and cannot confine, for `zerith policy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JailReport {
    pub platform: &'static str,
    /// Never `Full` today: no platform confines reads and network too.
    pub enforcement: Enforcement,
    pub isolated: Vec<&'static str>,
    pub not_isolated: Vec<&'static str>,
}

/// The real enforcement of `level` on this platform.
pub fn report(level: Sandbox) -> JailReport {
    let mut r = platform_report();
    if level == Sandbox::DangerFullAccess {
        r.isolated.retain(|i| i.contains("kill-on-close"));
        r.not_isolated.insert(
            0,
            "everything else: danger-full-access asks for no confinement",
        );
    }
    r
}

#[cfg(windows)]
fn platform_report() -> JailReport {
    JailReport {
        platform: "windows",
        enforcement: Enforcement::Partial,
        isolated: vec![
            "kill-on-close: the whole process tree dies when the call ends or the host exits",
            "process limit: a fork bomb stops at the limit",
            "memory limit for the whole tree",
            "UI limits: no desktop switch, clipboard, global atoms, system parameters, logoff",
        ],
        not_isolated: vec![
            "file writes (no restricted/low-integrity token yet: a shell can write anywhere the user can)",
            "file reads",
            "network",
            "the user's environment variables and credentials",
        ],
    }
}

#[cfg(target_os = "macos")]
fn platform_report() -> JailReport {
    JailReport {
        platform: "macos (sandbox-exec profile; UNVERIFIED: written without access to a Mac)",
        enforcement: Enforcement::Partial,
        isolated: vec!["file writes outside the workspace and temp directories"],
        not_isolated: vec![
            "file reads",
            "network",
            "process count and memory",
            "sandbox-exec is deprecated by Apple but still shipped",
        ],
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn bwrap_available() -> bool {
    static BWRAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BWRAP.get_or_init(|| {
        if std::env::var_os("ZERITH_BWRAP").as_deref() == Some(std::ffi::OsStr::new("0")) {
            return false;
        }
        if std::env::var_os("CARGO_MANIFEST_DIR").is_some()
            && std::env::var_os("ZERITH_BWRAP").as_deref() != Some(std::ffi::OsStr::new("1"))
        {
            return false;
        }
        use std::process::Stdio;
        Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--", "/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_report() -> JailReport {
    if bwrap_available() {
        JailReport {
            platform: "unix (bubblewrap + process group + secret env scrubbing)",
            enforcement: Enforcement::Partial,
            isolated: vec![
                "own process group, so the whole tree can be signalled",
                "file writes confined to the workspace and /tmp via bubblewrap",
                "inherited API keys and bot tokens stripped from environment",
            ],
            not_isolated: vec![
                "file reads",
                "network (in workspace-write)",
                "process count and memory",
            ],
        }
    } else {
        JailReport {
            platform: "unix (process group + secret env scrubbing; Landlock/bwrap fallback)",
            enforcement: Enforcement::Partial,
            isolated: vec![
                "own process group, so the whole tree can be signalled",
                "inherited API keys and bot tokens stripped from environment",
            ],
            not_isolated: vec![
                "file writes",
                "file reads",
                "network",
                "process count and memory",
            ],
        }
    }
}

#[cfg(not(any(windows, unix)))]
fn platform_report() -> JailReport {
    JailReport {
        platform: "other",
        enforcement: Enforcement::Partial,
        isolated: vec![],
        not_isolated: vec!["everything"],
    }
}

const SECRET_ENV_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "DEEPSEEK_API_KEY",
    "MISTRAL_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "PERPLEXITY_API_KEY",
    "COHERE_API_KEY",
    "XAI_API_KEY",
    "CEREBRAS_API_KEY",
    "SAMBANOVA_API_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITLAB_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "ZERITH_API_KEY",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
];

/// Returns `true` when an environment variable name holds a provider key, secret
/// or bot token that jailed child processes should not inherit.
pub fn is_secret_env_key(key: &str) -> bool {
    let up = key.trim().to_ascii_uppercase();
    SECRET_ENV_VARS.contains(&up.as_str())
        || up.ends_with("_API_KEY")
        || up.ends_with("_SECRET_KEY")
        || up.ends_with("_ACCESS_TOKEN")
        || up.ends_with("_BOT_TOKEN")
        || up.ends_with("_PRIVATE_KEY")
}

pub fn scrub_secret_env(cmd: &mut Command) {
    for &k in SECRET_ENV_VARS {
        cmd.env_remove(k);
    }
    for (k, _) in std::env::vars_os() {
        if k.to_str().is_some_and(is_secret_env_key) {
            cmd.env_remove(&k);
        }
    }
    let explicit_secrets: Vec<std::ffi::OsString> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            if v.is_some() && k.to_str().is_some_and(is_secret_env_key) {
                Some(k.to_os_string())
            } else {
                None
            }
        })
        .collect();
    for k in explicit_secrets {
        cmd.env_remove(k);
    }
}

/// Containment settings for processes started on behalf of one policy.
#[derive(Debug, Clone)]
pub struct Jail {
    pub level: Sandbox,
    pub workspace: PathBuf,
    pub limits: Limits,
}

impl Jail {
    pub fn new(level: Sandbox, workspace: impl AsRef<Path>) -> Self {
        Self {
            level,
            workspace: workspace.as_ref().to_path_buf(),
            limits: Limits::default(),
        }
    }

    pub fn for_policy(policy: &Policy) -> Self {
        Self::new(policy.preset.sandbox, &policy.workspace)
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn report(&self) -> JailReport {
        report(self.level)
    }

    /// Applies what can be applied before the process exists. On Windows that
    /// is nothing (a Job Object is attached after creation: use [`spawn`]).
    ///
    /// By default, environment variables are preserved so consulting/dev tools
    /// (`gh`, `aws`, `ansible`, `netmiko`, SSH agents) keep their credentials;
    /// opt-in scrubbing is only applied in `ReadOnly` mode when
    /// `ZERITH_SCRUB_ENV=1` is set.
    ///
    /// [`spawn`]: Jail::spawn
    pub fn wrap(&self, mut cmd: Command) -> Command {
        if self.level == Sandbox::ReadOnly
            && std::env::var_os("ZERITH_SCRUB_ENV").as_deref() == Some(std::ffi::OsStr::new("1"))
        {
            scrub_secret_env(&mut cmd);
        }
        #[allow(unused_mut)]
        let mut cmd = if cfg!(target_os = "macos") && self.level != Sandbox::DangerFullAccess {
            sandbox_exec(&macos_profile(self.level, &self.workspace), &cmd)
        } else {
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                if self.level != Sandbox::DangerFullAccess && bwrap_available() {
                    bwrap_exec(self.level, &self.workspace, &cmd)
                } else {
                    cmd
                }
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            {
                cmd
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    }

    /// Wraps and spawns. On Windows the child is created suspended, attached
    /// to a Job Object, then resumed, so it can never run outside the job. If
    /// the job cannot be set up the child is killed and an error returned:
    /// failing closed beats running unjailed.
    pub fn spawn(&self, cmd: Command) -> std::io::Result<Jailed> {
        let cmd = self.wrap(cmd);
        self.spawn_wrapped(cmd)
    }

    /// Spawns a command already prepared with [`Jail::wrap`]. Prefer
    /// `wrap` → configure `stdin`/`stdout`/`stderr` → `spawn_wrapped`, because
    /// on macOS `wrap` builds a fresh `sandbox-exec` `Command` that cannot
    /// inherit stdio configured earlier.
    pub fn spawn_wrapped(&self, cmd: Command) -> std::io::Result<Jailed> {
        #[cfg(windows)]
        {
            win::spawn(cmd, self)
        }
        #[cfg(not(windows))]
        {
            let mut cmd = cmd;
            Ok(Jailed {
                child: cmd.spawn()?,
                _guard: (),
            })
        }
    }
}

impl Sandbox {
    /// [`Jail::wrap`] for a preset level.
    pub fn wrap(self, workspace: &Path, cmd: Command) -> Command {
        Jail::new(self, workspace).wrap(cmd)
    }
}

/// A started child plus whatever keeps it contained. Dropping it closes the
/// job: on Windows, any process of the tree still alive is killed.
// The unit guard off Windows only mirrors the Windows layout; it is not a
// non-exhaustive marker, which is what clippy takes it for on Linux.
#[allow(clippy::manual_non_exhaustive)]
pub struct Jailed {
    pub child: Child,
    #[cfg(windows)]
    _guard: win::JobHandle,
    #[cfg(not(windows))]
    _guard: (),
}

impl Jailed {
    /// Kills the child and, on Windows, every descendant.
    pub fn kill_tree(&mut self) {
        #[cfg(windows)]
        self._guard.terminate();
        #[cfg(unix)]
        {
            // The child leads its own group (see `Jail::wrap`): `kill -KILL -pgid`.
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-{}", self.child.id())])
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `sandbox-exec` profile for `level`. Public so it can be inspected and
/// tested on any platform; only applied on macOS.
///
/// Writes are confined to the workspace and the temp directories; reads and
/// the network are left open (a deny-by-default profile breaks too many
/// toolchains to be a default). `read-only` allows no writes except the
/// terminal and `/dev/null`.
pub fn macos_profile(level: Sandbox, workspace: &Path) -> String {
    let q = |p: &str| p.replace('\\', "\\\\").replace('"', "\\\"");
    let mut s = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    s.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/fd/\"))\n");
    if level == Sandbox::WorkspaceWrite {
        let ws = workspace.canonicalize().unwrap_or(workspace.to_path_buf());
        s.push_str(&format!(
            "(allow file-write* (subpath \"{}\") (subpath \"/private/tmp\") (subpath \"/private/var/folders\"))\n",
            q(&ws.to_string_lossy())
        ));
    }
    s
}

/// `sandbox-exec -p <profile> -- program args...`, keeping cwd and env edits.
pub fn sandbox_exec(profile: &str, inner: &Command) -> Command {
    let mut c = Command::new("/usr/bin/sandbox-exec");
    c.arg("-p")
        .arg(profile)
        .arg("--")
        .arg(inner.get_program())
        .args(inner.get_args());
    if let Some(dir) = inner.get_current_dir() {
        c.current_dir(dir);
    }
    for (k, v) in inner.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c
}

/// Builds the bubblewrap (`bwrap`) argument vector for Linux sandboxing.
///
/// - `--ro-bind / /` makes the root filesystem read-only.
/// - `Sandbox::WorkspaceWrite` adds `--bind <workspace> <workspace>`.
/// - `Sandbox::ReadOnly` adds `--unshare-net` and leaves the workspace read-only.
pub fn linux_bwrap_args(
    level: Sandbox,
    workspace: &Path,
    chdir: &Path,
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    let mut out: Vec<OsString> = vec![
        "--unshare-pid".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ];
    match level {
        Sandbox::WorkspaceWrite => {
            out.push("--bind".into());
            out.push(workspace.as_os_str().to_os_string());
            out.push(workspace.as_os_str().to_os_string());
        }
        Sandbox::ReadOnly => {
            out.push("--unshare-net".into());
        }
        Sandbox::DangerFullAccess => {}
    }
    out.push("--chdir".into());
    out.push(chdir.as_os_str().to_os_string());
    out.push("--".into());
    out.push(program.to_os_string());
    out.extend(args.iter().cloned());
    out
}

/// Wraps `inner` in `bwrap` using [`linux_bwrap_args`], preserving cwd and env edits.
pub fn bwrap_exec(level: Sandbox, workspace: &Path, inner: &Command) -> Command {
    let ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let chdir = inner.get_current_dir().unwrap_or(&ws);
    let inner_args: Vec<std::ffi::OsString> = inner.get_args().map(|a| a.to_os_string()).collect();
    let mut c = Command::new("bwrap");
    c.args(linux_bwrap_args(
        level,
        &ws,
        chdir,
        inner.get_program(),
        &inner_args,
    ));
    if let Some(dir) = inner.get_current_dir() {
        c.current_dir(dir);
    }
    for (k, v) in inner.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c
}

#[cfg(windows)]
mod win {
    use super::{Jail, Jailed};
    use crate::Sandbox;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
        JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_UILIMIT_DESKTOP,
        JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
        JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
        JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    /// The app itself is a window (`windows_subsystem = "windows"`): without
    /// this, every shell command popped a console window on top of it.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[link(name = "ntdll")]
    extern "system" {
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    pub struct JobHandle(HANDLE);

    // A job handle is just a kernel handle; moving it across threads is fine.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        pub fn terminate(&self) {
            // SAFETY: the handle is valid until `drop`.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            // With KILL_ON_JOB_CLOSE this kills every process still in the job.
            // SAFETY: closing the handle we own, once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn job(jail: &Jail) -> std::io::Result<JobHandle> {
        // SAFETY: plain FFI; the structs are zeroed POD and sized correctly.
        unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let guard = JobHandle(h);

            let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            ext.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if jail.level != Sandbox::DangerFullAccess {
                ext.BasicLimitInformation.LimitFlags |=
                    JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_JOB_MEMORY;
                ext.BasicLimitInformation.ActiveProcessLimit = jail.limits.max_processes;
                ext.JobMemoryLimit = jail.limits.memory_bytes;
            }
            if SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &ext as *const _ as *const _,
                std::mem::size_of_val(&ext) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }

            if jail.level != Sandbox::DangerFullAccess {
                let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                    UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                        | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                        | JOB_OBJECT_UILIMIT_EXITWINDOWS
                        | JOB_OBJECT_UILIMIT_GLOBALATOMS
                        | JOB_OBJECT_UILIMIT_HANDLES
                        | JOB_OBJECT_UILIMIT_READCLIPBOARD
                        | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                        | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
                };
                if SetInformationJobObject(
                    h,
                    JobObjectBasicUIRestrictions,
                    &ui as *const _ as *const _,
                    std::mem::size_of_val(&ui) as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(guard)
        }
    }

    pub fn spawn(mut cmd: Command, jail: &Jail) -> std::io::Result<Jailed> {
        let guard = job(jail)?;
        cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let mut child = cmd.spawn()?;
        let h = child.as_raw_handle() as HANDLE;
        // SAFETY: both handles are valid; the child is suspended, so it has
        // run no code (and spawned nothing) outside the job.
        let attached = unsafe { AssignProcessToJobObject(guard.0, h) } != 0;
        if !attached {
            let e = std::io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                e.kind(),
                format!("não foi possível conter o processo num Job Object: {e}"),
            ));
        }
        // SAFETY: resumes the process we just created.
        let status = unsafe { NtResumeProcess(h) };
        if status < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other(format!(
                "NtResumeProcess falhou: 0x{status:x}"
            )));
        }
        Ok(Jailed {
            child,
            _guard: guard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_never_claims_full_enforcement() {
        for l in [
            Sandbox::ReadOnly,
            Sandbox::WorkspaceWrite,
            Sandbox::DangerFullAccess,
        ] {
            let r = report(l);
            assert_eq!(r.enforcement, Enforcement::Partial);
            assert!(!r.not_isolated.is_empty());
        }
        // Reads and network are never reported as isolated.
        let r = report(Sandbox::WorkspaceWrite);
        assert!(r.not_isolated.iter().any(|n| n.contains("network")));
    }

    #[test]
    fn macos_profile_confines_writes() {
        let ws = std::env::current_dir().unwrap();
        let p = macos_profile(Sandbox::WorkspaceWrite, &ws);
        assert!(p.starts_with("(version 1)\n(allow default)\n(deny file-write*)"));
        assert!(p.contains("(subpath \""));
        assert!(p.contains("/private/tmp"));
        let ro = macos_profile(Sandbox::ReadOnly, &ws);
        assert!(
            !ro.contains("subpath"),
            "read-only allows no directory: {ro}"
        );
        let evil = macos_profile(Sandbox::WorkspaceWrite, Path::new("/a\"b)(allow"));
        assert!(
            evil.contains("\\\""),
            "quotes in the path are escaped: {evil}"
        );
    }

    #[test]
    fn sandbox_exec_keeps_program_args_cwd_and_env() {
        let mut inner = Command::new("echo");
        inner.arg("hi").current_dir("/x").env("A", "1");
        let c = sandbox_exec("(version 1)", &inner);
        assert_eq!(c.get_program(), "/usr/bin/sandbox-exec");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-p", "(version 1)", "--", "echo", "hi"]);
        assert_eq!(c.get_current_dir(), Some(Path::new("/x")));
        assert!(c.get_envs().any(|(k, v)| k == "A" && v.is_some()));
    }

    #[test]
    fn linux_bwrap_args_differentiates_read_only_and_workspace_write() {
        use std::ffi::{OsStr, OsString};
        let ws = Path::new("/home/dev/project");
        let sub = Path::new("/home/dev/project/sub");
        let prog = OsStr::new("sh");
        let args = vec![OsString::from("-c"), OsString::from("ls")];

        let ro: Vec<String> = linux_bwrap_args(Sandbox::ReadOnly, ws, ws, prog, &args)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(ro.contains(&"--ro-bind".to_string()));
        assert!(ro.contains(&"--unshare-net".to_string()));
        assert!(!ro.contains(&"--bind".to_string()));
        assert_eq!(&ro[ro.len() - 4..], ["--", "sh", "-c", "ls"]);

        let rw: Vec<String> = linux_bwrap_args(Sandbox::WorkspaceWrite, ws, sub, prog, &args)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(rw.contains(&"--ro-bind".to_string()));
        assert!(rw.contains(&"--bind".to_string()));
        assert!(!rw.contains(&"--unshare-net".to_string()));
        let bind_idx = rw.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(rw[bind_idx + 1], "/home/dev/project");
        assert_eq!(rw[bind_idx + 2], "/home/dev/project");
        let chdir_idx = rw.iter().position(|a| a == "--chdir").unwrap();
        assert_eq!(rw[chdir_idx + 1], "/home/dev/project/sub");

        let mut inner = Command::new("sh");
        inner.arg("-c").arg("pwd").current_dir(sub);
        let wrapped = bwrap_exec(Sandbox::WorkspaceWrite, ws, &inner);
        let w_args: Vec<String> = wrapped
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let w_chdir_idx = w_args.iter().position(|a| a == "--chdir").unwrap();
        assert_eq!(w_args[w_chdir_idx + 1], "/home/dev/project/sub");
    }

    #[test]
    fn wrap_preserves_env_by_default_and_scrub_helper_works_on_demand() {
        let mut inner = Command::new("echo");
        inner
            .env("OPENAI_API_KEY", "sk-secret")
            .env("TELEGRAM_BOT_TOKEN", "bot-secret")
            .env("CUSTOM_ACCESS_TOKEN", "tok-secret")
            .env("A", "1");
        let jailed = Jail::new(Sandbox::WorkspaceWrite, ".").wrap(inner);
        assert!(jailed
            .get_envs()
            .any(|(k, v)| k == "OPENAI_API_KEY" && v == Some(std::ffi::OsStr::new("sk-secret"))));
        assert!(jailed
            .get_envs()
            .any(|(k, v)| k == "A" && v == Some(std::ffi::OsStr::new("1"))));

        let mut scrubbed = Command::new("echo");
        scrubbed
            .env("OPENAI_API_KEY", "sk-secret")
            .env("TELEGRAM_BOT_TOKEN", "bot-secret")
            .env("CUSTOM_ACCESS_TOKEN", "tok-secret")
            .env("A", "1");
        scrub_secret_env(&mut scrubbed);
        assert!(scrubbed
            .get_envs()
            .any(|(k, v)| k == "OPENAI_API_KEY" && v.is_none()));
        assert!(scrubbed
            .get_envs()
            .any(|(k, v)| k == "TELEGRAM_BOT_TOKEN" && v.is_none()));
        assert!(scrubbed
            .get_envs()
            .any(|(k, v)| k == "CUSTOM_ACCESS_TOKEN" && v.is_none()));
        assert!(scrubbed
            .get_envs()
            .any(|(k, v)| k == "A" && v == Some(std::ffi::OsStr::new("1"))));
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::process::Stdio;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        // The tests count PING.EXE processes machine-wide: run them one by one.
        static SERIAL: Mutex<()> = Mutex::new(());

        fn ping_count() -> usize {
            let out = Command::new("tasklist")
                .args(["/FI", "IMAGENAME eq PING.EXE", "/NH"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .matches("PING.EXE")
                .count()
        }

        fn cmd(script: &str) -> Command {
            let mut c = Command::new("cmd");
            std::os::windows::process::CommandExt::raw_arg(c.arg("/C"), script);
            c.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c
        }

        fn alive(pid: u32) -> bool {
            let out = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
        }

        #[test]
        fn a_jailed_command_still_runs_and_reports_its_output() {
            let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let jail = Jail::new(Sandbox::WorkspaceWrite, ".");
            let j = jail.spawn(cmd("echo dentro")).unwrap();
            let out = j.child.wait_with_output().unwrap();
            assert!(out.status.success());
            assert!(String::from_utf8_lossy(&out.stdout).contains("dentro"));
        }

        #[test]
        fn dropping_the_job_kills_the_whole_tree() {
            let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let base = ping_count();
            let jail = Jail::new(Sandbox::WorkspaceWrite, ".");
            // cmd -> ping: a grandchild that a plain `Child::kill` would orphan.
            let j = jail.spawn(cmd("ping -n 30 127.0.0.1 >nul")).unwrap();
            std::thread::sleep(Duration::from_millis(700));
            assert_eq!(ping_count(), base + 1, "the grandchild must be running");
            let pid = j.child.id();
            drop(j);
            let t = Instant::now();
            while (alive(pid) || ping_count() > base) && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(!alive(pid));
            assert_eq!(ping_count(), base, "the grandchild died with the job");
        }

        fn burst(jail: &Jail) -> usize {
            let one = "start /b ping -n 8 127.0.0.1 >nul";
            let script = [one; 6].join(" & ");
            let base = ping_count();
            let mut j = jail.spawn(cmd(&script)).unwrap();
            std::thread::sleep(Duration::from_millis(900));
            let n = ping_count() - base;
            j.kill_tree();
            n
        }

        #[test]
        fn the_process_limit_stops_a_fork_bomb() {
            let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let free = Jail::new(Sandbox::WorkspaceWrite, ".").with_limits(Limits {
                max_processes: 100,
                memory_bytes: 1 << 30,
            });
            let tight = Jail::new(Sandbox::WorkspaceWrite, ".").with_limits(Limits {
                max_processes: 3,
                memory_bytes: 1 << 30,
            });
            // Control: without a tight limit all six children start.
            let unlimited = burst(&free);
            assert_eq!(unlimited, 6, "control run");
            // Limit 3 = cmd + at most 2 children.
            let limited = burst(&tight);
            assert!(limited <= 2, "limit 3 let {limited} children live");
        }

        #[test]
        fn kill_tree_ends_the_command() {
            let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            let jail = Jail::new(Sandbox::ReadOnly, ".");
            let mut j = jail.spawn(cmd("ping -n 30 127.0.0.1 >nul")).unwrap();
            let t = Instant::now();
            j.kill_tree();
            assert!(t.elapsed() < Duration::from_secs(5));
            assert!(!alive(j.child.id()));
        }
    }
}
