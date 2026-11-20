//! The local billion-context proxy the chat goes through by default
//! (<https://github.com/ranxianglei/billion-context>, MIT + attribution).
//!
//! `ensure` is cheap and idempotent: hosts call it at start and before every
//! turn. It reuses a healthy proxy another Zerith process already runs
//! (`<root>/bili/origin`), else starts one tied to this process
//! (`BILI_PARENT_PID`: it exits when we do). While nothing answers, requests
//! go direct; nothing here can fail a turn.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use zerith_providers::bili as route;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Turned off in settings.
    Off,
    /// `bili` is not on PATH (`npm install -g billion-context`).
    Missing,
    Starting,
    Running(String),
    Failed(String),
}

struct State {
    status: Status,
    child: Option<Child>,
}

static STATE: Mutex<State> = Mutex::new(State {
    status: Status::Off,
    child: None,
});

/// How long a fresh proxy gets to answer its health check (first start
/// migrates its store and loads the model registry).
const BOOT: Duration = Duration::from_secs(25);

pub fn status() -> Status {
    let st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    match (&st.status, route::origin()) {
        // The provider layer forgets a proxy that stopped answering.
        (Status::Running(_), None) => Status::Failed("o proxy parou de responder".into()),
        (s, _) => s.clone(),
    }
}

/// Brings the proxy up (or down, when `enabled` is false). With `wait`, returns
/// once it answers or failed; without, the start runs on its own thread.
pub fn ensure(root: &Path, enabled: bool, wait: bool) {
    if !enabled {
        stop();
        set(Status::Off);
        return;
    }
    if route::origin().is_some_and(|o| route::listening(&o)) {
        return;
    }
    {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if st.status == Status::Starting {
            return;
        }
        st.status = Status::Starting;
    }
    let root = root.to_path_buf();
    let job = move || {
        let s = start(&root);
        if let Status::Running(o) = &s {
            route::set_origin(Some(o.clone()));
        }
        set(s);
    };
    if wait {
        job();
    } else {
        std::thread::spawn(job);
    }
}

/// Uses a proxy another Zerith process already runs (the window's), never
/// starts one: a start costs ~15 s on Windows, a one-shot CLI call would pay
/// it every time and the proxy would die with it.
pub fn attach(root: &Path, enabled: bool) {
    let found = enabled
        .then(|| std::fs::read_to_string(root.join("bili").join("origin")).ok())
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|o| healthy(o));
    set(match &found {
        Some(o) => Status::Running(o.clone()),
        None if enabled => Status::Failed("nenhum proxy aberto".into()),
        None => Status::Off,
    });
    route::set_origin(found);
}

/// Stops the proxy this process started (one it only reused keeps running).
pub fn stop() {
    route::set_origin(None);
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut c) = st.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn set(s: Status) {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).status = s;
}

fn start(root: &Path) -> Status {
    let dir = root.join("bili");
    let shared = dir.join("origin");
    if let Some(o) = std::fs::read_to_string(&shared)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|o| healthy(o))
    {
        return Status::Running(o);
    }
    let Some(mut cmd) = command() else {
        return Status::Missing;
    };
    let port = match std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()) {
        Ok(a) => a.port(),
        Err(e) => return Status::Failed(e.to_string()),
    };
    let _ = std::fs::create_dir_all(&dir);
    cmd.args(["start", "--host", "127.0.0.1", "--port", &port.to_string()])
        .arg("--no-auto-update")
        .env("BILI_PARENT_PID", std::process::id().to_string())
        .env("BILI_SESSIONS_DIR", dir.join("sessions"))
        .env("ACP_LOG_FILE", dir.join("bili.log"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Status::Failed(format!("não consegui iniciar o bili: {e}")),
    };
    let origin = format!("http://127.0.0.1:{port}");
    let until = Instant::now() + BOOT;
    while Instant::now() < until {
        if healthy(&origin) {
            let _ = std::fs::write(&shared, &origin);
            STATE.lock().unwrap_or_else(|e| e.into_inner()).child = Some(child);
            return Status::Running(origin);
        }
        if let Ok(Some(code)) = child.try_wait() {
            return Status::Failed(format!(
                "o bili saiu ao iniciar ({code}); veja {}",
                dir.join("bili.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    Status::Failed("o bili não respondeu a tempo".into())
}

/// `bili` itself, or on Windows `node <npm>/node_modules/billion-context/dist/index.js`
/// beside the npm shim: the `.cmd` would need a console of its own.
fn command() -> Option<Command> {
    let bin = zerith_mcp::which("bili")?;
    let js = bin
        .parent()
        .map(|d| d.join("node_modules/billion-context/dist/index.js"))
        .filter(|p| p.is_file());
    match (cfg!(windows), js, zerith_mcp::which("node")) {
        (true, Some(js), Some(node)) => {
            let mut c = Command::new(node);
            c.arg(js);
            Some(c)
        }
        _ => Some(Command::new(bin)),
    }
}

fn healthy(origin: &str) -> bool {
    route::listening(origin)
        && ureq::get(&format!("{origin}/__bili/health"))
            .timeout(Duration::from_secs(2))
            .call()
            .ok()
            .and_then(|r| r.into_string().ok())
            .is_some_and(|b| b.contains("\"ok\":true"))
}

/// `npm install -g billion-context`, for the "Instalar" button.
pub fn install() -> Result<(), String> {
    let npm = zerith_mcp::which("npm").ok_or("npm não encontrado: instale o Node.js 22+")?;
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(npm);
        c
    } else {
        Command::new(npm)
    };
    cmd.args(["install", "-g", "billion-context"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().take(4).collect::<Vec<_>>().join(" | "))
    }
}

/// Where the proxy writes its log, for the settings screen.
pub fn log_path(root: &Path) -> PathBuf {
    root.join("bili").join("bili.log")
}
