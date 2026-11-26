//! Interactive terminal sessions: a program that keeps a prompt open (ssh,
//! telnet, a REPL, a router CLI) driven one input at a time.
//!
//! `shell` runs a command to completion with stdin closed, so anything that
//! asks a question (a password, `[y/n]`, a router's `>` prompt) hangs until
//! the timeout. Here the program runs under a pseudo-terminal (ConPTY on
//! Windows, `script` elsewhere), a thread buffers its output, and each call
//! sends one input and returns what came back since the last read.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

const MAX_OPEN: usize = 24;
const MAX_AUTO_PAGES: usize = 200;
const LOOP_TAIL_BYTES: usize = 4096;
/// Output past this without a prompt is treated as never-ending (below
/// `BUF_CAP`, so nothing is dropped before the interrupt).
const MAX_CMD_BYTES: usize = 3 * 1024 * 1024;
/// Same block of 1–6 lines this many times in a row, no prompt: a loop.
const REPEAT_STOP: usize = 5;
/// Lines handed back after a continuous command was cut (head + tail).
const CUT_HEAD_LINES: usize = 120;
const CUT_TAIL_LINES: usize = 30;
/// Marks output that went through the pager (the batch then turns paging off).
pub(crate) const PAGED_NOTE: &str = "[paginação automática:";
/// A terminal idle this long is closed (a pause in the conversation; the
/// next call reopens a `net_connect` session, see `Remembered`).
const IDLE_REAP: Duration = Duration::from_secs(30 * 60);
const BUF_CAP: usize = 4 * 1024 * 1024;
const BUF_SHRINK_THRESHOLD: usize = 64 * 1024;

type TermHook = Arc<dyn Fn() + Send + Sync>;

fn change_hook_slot() -> &'static Mutex<Option<TermHook>> {
    static H: OnceLock<Mutex<Option<TermHook>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Registers a callback fired whenever terminal liveness, busy state, or list changes.
pub fn set_terminal_change_hook(cb: TermHook) {
    if let Ok(mut guard) = change_hook_slot().lock() {
        *guard = Some(cb);
    }
}

fn notify_changed() {
    let cb = change_hook_slot().lock().ok().and_then(|g| g.clone());
    if let Some(f) = cb {
        f();
    }
}

fn ensure_reaper() {
    static REAPER: OnceLock<()> = OnceLock::new();
    REAPER.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(60));
            reap();
        });
    });
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub id: String,
    pub command: String,
    pub alive: bool,
    pub busy: bool,
    pub vendor: Option<String>,
    pub device: Option<String>,
    pub host: Option<String>,
    pub role: Option<String>,
    pub client: Option<String>,
    pub vpn: Option<String>,
    pub transport: Option<String>,
    pub idle_secs: u64,
}

#[derive(Clone)]
struct TermMeta {
    id: String,
    command: String,
    vendor: Option<String>,
    device: Option<String>,
    host: Option<String>,
    role: Option<String>,
    client: Option<String>,
    vpn: Option<String>,
    transport: Option<String>,
    /// The session that opened it (see [`terminal_open_for`]).
    owner: Option<String>,
    buf: Arc<Mutex<VecDeque<u8>>>,
    input: Arc<Mutex<Box<dyn Write + Send>>>,
    last_used: Arc<Mutex<Instant>>,
    alive: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

pub(crate) struct Term {
    meta: TermMeta,
    /// Output not yet handed to the model.
    buf: Arc<Mutex<VecDeque<u8>>>,
    input: Arc<Mutex<Box<dyn Write + Send>>>,
    proc: Proc,
    owner: Option<String>,
    /// Vault values typed here: scrubbed from every read.
    secrets: Vec<String>,
}

impl Drop for Term {
    fn drop(&mut self) {
        self.meta.alive.store(false, Ordering::SeqCst);
        self.meta.busy.store(false, Ordering::SeqCst);
        self.proc.kill();
        if let Ok(mut m) = meta_table().lock() {
            m.remove(&self.meta.id);
        }
    }
}

fn table() -> &'static Mutex<HashMap<String, Term>> {
    static T: OnceLock<Mutex<HashMap<String, Term>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

fn meta_table() -> &'static Mutex<HashMap<String, TermMeta>> {
    static M: OnceLock<Mutex<HashMap<String, TermMeta>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Term>> {
    table().lock().unwrap_or_else(|e| e.into_inner())
}

/// What is kept of a terminal after it closes on its own (ssh dropped, idle
/// reap, app restart of the session's turn): who opened it, on what, and the
/// `net_connect` arguments that reopen it. Ids never repeat within a process,
/// so the id alone names it. Forgotten on `terminal_close` and when the
/// conversation is deleted. Idea from Hermes Agent's `process_registry.py`
/// (MIT), which keeps a session's processes findable after they exit.
// ponytail: memory only, so an app restart forgets it (the model then gets
// the "chame net_connect de novo" error); a file per session if that matters.
#[derive(Clone)]
struct Remembered {
    owner: Option<String>,
    command: String,
    vendor: Option<String>,
    device: Option<String>,
    role: Option<String>,
    /// `net_connect` arguments; `None` for `terminal_open`, never reopened.
    reopen: Option<Value>,
    /// The live session's clock, still readable after it closed.
    last_used: Arc<Mutex<Instant>>,
}

/// A dropped session is reopened only if it was used this recently: past
/// that the model is told to connect again (a router is not logged into
/// hours later behind the user's back).
const REOPEN_WINDOW: Duration = Duration::from_secs(2 * 3600);

fn remembered() -> std::sync::MutexGuard<'static, HashMap<String, Remembered>> {
    static R: OnceLock<Mutex<HashMap<String, Remembered>>> = OnceLock::new();
    R.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Records the `net_connect` arguments that reopen terminal `id`.
pub(crate) fn remember_reopen(id: &str, args: Value) {
    if let Some(r) = remembered().get_mut(id) {
        r.reopen = Some(args);
    }
}

/// The `net_connect` arguments for a dead terminal `id` of this conversation,
/// when it is gone and was opened by `net_connect`.
pub(crate) fn reopen_args(id: &str, session: Option<&str>) -> Option<Value> {
    // A session whose program just died is still in the table until reaped.
    reap();
    if lock().contains_key(id) {
        return None;
    }
    let r = remembered().get(id).cloned()?;
    let recent = r
        .last_used
        .lock()
        .is_ok_and(|t| t.elapsed() < REOPEN_WINDOW);
    (recent && r.owner.as_deref() == session)
        .then_some(r.reopen)
        .flatten()
}

/// Gives the terminal `from` the id `to` (a reopened session keeps the id
/// the model knows).
pub(crate) fn rename_terminal(from: &str, to: &str) {
    let mut t = lock();
    let Some(mut term) = t.remove(from) else {
        return;
    };
    term.meta.id = to.to_string();
    if let Ok(mut m) = meta_table().lock() {
        m.remove(from);
        m.insert(to.to_string(), term.meta.clone());
    }
    let mut r = remembered();
    if let Some(entry) = r.remove(from) {
        r.insert(to.to_string(), entry);
    }
    t.insert(to.to_string(), term);
    drop((t, r));
    notify_changed();
}

/// The open terminals, and what the model should do about a dead `id`.
pub(crate) fn missing_terminal(id: &str) -> String {
    format!(
        "terminal {id} não existe mais (abertos: {}); a sessão caiu ou foi fechada: chame \
         net_connect de novo para abrir outra e use o id novo",
        open_list(&lock())
    )
}

/// The program a terminal session runs, for the approval rules (a session
/// on `ssh` is remote access; every input to it is approved on its own).
/// A terminal that closed on its own still answers: the next call may
/// reopen it, and must be judged as reaching that same device.
pub fn terminal_program(id: &str) -> Option<String> {
    meta_table()
        .lock()
        .ok()?
        .get(id)
        .map(|t| t.command.clone())
        .or_else(|| remembered().get(id).map(|r| r.command.clone()))
}

/// `(vendor, device, role)` metadata for an active terminal session, or for
/// one that closed on its own (see [`terminal_program`]).
pub fn terminal_meta(id: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    meta_table()
        .lock()
        .ok()?
        .get(id)
        .map(|t| (t.vendor.clone(), t.device.clone(), t.role.clone()))
        .or_else(|| {
            remembered()
                .get(id)
                .map(|r| (r.vendor.clone(), r.device.clone(), r.role.clone()))
        })
}

/// Closes every terminal opened by conversation `session` and forgets them
/// (the conversation was deleted).
pub fn close_session_terminals(session: &str) {
    let ids: Vec<String> = remembered()
        .iter()
        .filter(|(_, r)| r.owner.as_deref() == Some(session))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let _ = close_terminal(&id);
    }
}

/// Lists all open terminal sessions with live liveness/busy/idle status.
pub fn list_active_terminals() -> Vec<TerminalSnapshot> {
    reap();
    let entries: Vec<TermMeta> = meta_table()
        .lock()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    let mut out: Vec<TerminalSnapshot> = entries
        .into_iter()
        .map(|m| {
            let idle_secs = m
                .last_used
                .lock()
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            TerminalSnapshot {
                id: m.id,
                command: m.command,
                alive: m.alive.load(Ordering::SeqCst),
                busy: m.busy.load(Ordering::SeqCst),
                vendor: m.vendor,
                device: m.device,
                host: m.host,
                role: m.role,
                client: m.client,
                vpn: m.vpn,
                transport: m.transport,
                idle_secs,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Whether `session` has a live terminal open (in use or idle): the turn
/// keeps the network tools offered while it does.
pub fn terminal_open_for(session: &str) -> bool {
    meta_table().lock().is_ok_and(|m| {
        m.values()
            .any(|t| t.owner.as_deref() == Some(session) && t.alive.load(Ordering::SeqCst))
    })
}

/// Reads the trailing output buffer of `id` without consuming it.
pub fn terminal_tail(id: &str, max_bytes: usize) -> std::result::Result<String, String> {
    let buf = meta_table()
        .lock()
        .ok()
        .and_then(|m| m.get(id).map(|t| t.buf.clone()))
        .ok_or_else(|| format!("terminal {id} não encontrado"))?;
    let bytes: Vec<u8> = buf
        .lock()
        .map(|b| {
            let cap = max_bytes.clamp(64, 65536);
            let start = b.len().saturating_sub(cap);
            b.range(start..).copied().collect()
        })
        .unwrap_or_default();
    Ok(clean(&bytes))
}

/// Sends a single manual command line directly from the Desktop Active Terminals
/// drawer to an open session `id`.
pub fn send_terminal_input(id: &str, input: &str) -> std::result::Result<(), String> {
    let meta = meta_table()
        .lock()
        .ok()
        .and_then(|m| m.get(id).cloned())
        .ok_or_else(|| format!("terminal {id} não encontrado"))?;
    if !meta.alive.load(Ordering::SeqCst) {
        return Err(format!("terminal {id} já encerrou"));
    }
    let mut payload = input.to_string();
    if !payload.ends_with('\r') && !payload.ends_with('\n') {
        payload.push('\r');
    }
    {
        let mut w = meta
            .input
            .lock()
            .map_err(|e| format!("falha ao travar stdin: {e}"))?;
        w.write_all(payload.as_bytes())
            .and_then(|_| w.flush())
            .map_err(|e| format!("falha ao enviar comando para {id}: {e}"))?;
    }
    if let Ok(mut lu) = meta.last_used.lock() {
        *lu = Instant::now();
    }
    notify_changed();
    Ok(())
}

/// Sends `Ctrl+C` (`0x03`) to `id`, even if a command is currently running.
pub fn interrupt_terminal(id: &str) -> std::result::Result<(), String> {
    let input = meta_table()
        .lock()
        .ok()
        .and_then(|m| m.get(id).map(|t| t.input.clone()))
        .ok_or_else(|| format!("terminal {id} não encontrado"))?;
    let mut w = input
        .lock()
        .map_err(|e| format!("falha ao travar stdin: {e}"))?;
    w.write_all(b"\x03")
        .and_then(|_| w.flush())
        .map_err(|e| format!("falha ao enviar Ctrl+C para {id}: {e}"))?;
    notify_changed();
    Ok(())
}

/// Closes terminal session `id` and terminates its child process.
pub fn close_terminal(id: &str) -> std::result::Result<(), String> {
    // Closed on purpose: never reopened behind the user's back.
    let forgot = remembered().remove(id).is_some();
    let removed = lock().remove(id);
    if let Some(term) = removed {
        drop(term);
        notify_changed();
        return Ok(());
    }
    // If currently in flight inside send/batch, mark dead so it drops on return.
    if let Ok(m) = meta_table().lock() {
        if let Some(meta) = m.get(id) {
            meta.alive.store(false, Ordering::SeqCst);
            if let Ok(mut w) = meta.input.lock() {
                let _ = w.write_all(b"\x03\nexit\n");
                let _ = w.flush();
            }
            drop(m);
            notify_changed();
            return Ok(());
        }
    }
    if forgot {
        return Ok(());
    }
    Err(format!("terminal {id} não existe"))
}

/// Closes all open interactive PTY/ConPTY sessions (used on app exit).
pub fn close_all_terminals() {
    let drained: Vec<Term> = {
        let mut t = lock();
        t.drain().map(|(_, term)| term).collect()
    };
    let had_drained = !drained.is_empty();
    drop(drained);

    let in_flight: Vec<TermMeta> = meta_table()
        .lock()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    let had_in_flight = !in_flight.is_empty();
    for meta in in_flight {
        meta.alive.store(false, Ordering::SeqCst);
        if let Ok(mut w) = meta.input.lock() {
            let _ = w.write_all(b"\x03\nexit\n");
            let _ = w.flush();
        }
    }

    if had_drained || had_in_flight {
        notify_changed();
    }
}

/// Closes open terminals belonging to a different client or different VPN
/// before switching an exclusive WireGuard tunnel.
pub fn close_other_vpn_terminals(active_vpn: &str) -> Vec<String> {
    let ids_to_close: Vec<String> = meta_table()
        .lock()
        .map(|m| {
            m.values()
                .filter_map(|meta| {
                    let other_vpn = meta
                        .vpn
                        .as_deref()
                        .is_some_and(|v| !v.eq_ignore_ascii_case(active_vpn));
                    if other_vpn {
                        Some(meta.id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut closed = Vec::new();
    for id in ids_to_close {
        if close_terminal(&id).is_ok() {
            closed.push(id);
        }
    }
    closed
}

fn open_list(t: &HashMap<String, Term>) -> String {
    let v: Vec<String> = t
        .iter()
        .map(|(id, s)| format!("{id} ({})", s.meta.command))
        .collect();
    if v.is_empty() {
        "nenhum".into()
    } else {
        v.join(", ")
    }
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make_with(
            "terminal_open",
            "Abre um programa interativo num terminal que fica aberto entre chamadas: ssh, telnet, \
             plink, um REPL (python, node), um CLI que pergunta coisas. Devolve um id e o que o \
             programa mostrou até parar num prompt. Use terminal_send para cada comando seguinte. \
             Prefira `shell` para comandos que terminam sozinhos.",
            json!({"type": "object", "properties": {
                "command": {"type": "string", "description": "o programa e argumentos, ex.: ssh admin@10.0.0.1"},
                "expect": {"type": "string", "description": "regex: espera até isso aparecer (ex.: \"[Pp]assword:\")"},
                "auto_page": {"type": "boolean", "description": "envia espaço automaticamente em prompts de paginação (--More--), padrão true"},
                "timeout_secs": {"type": "integer", "description": "padrão 20, máximo 120"}
            }, "required": ["command"]}),
            "id do terminal e a saída inicial",
            Permission::Shell,
            130,
            false,
            |args, _| Ok(Some(Action::Shell(str_arg(args, "command")?.to_string()))),
            open,
        ),
        make_with(
            "terminal_send",
            "Digita no terminal `id` e devolve a saída nova até o próximo prompt. `input` é o \
             texto (Enter no fim, a menos que enter=false); sem input e sem secret só lê o que \
             chegou. `secret`: nome de uma entrada do cofre do sistema (o usuário grava com \
             `zerith secret set <nome>`); o valor é digitado sem você ver e some da saída. \
             Paginação (---- More ----): input \" \" com enter=false, ou desligue antes \
             (`screen-length 0 temporary`, `terminal length 0`). Ctrl+C: input \"\\u0003\" com enter=false.",
            json!({"type": "object", "properties": {
                "id": {"type": "string"},
                "input": {"type": "string"},
                "secret": {"type": "string", "description": "nome no cofre; nunca o valor"},
                "enter": {"type": "boolean", "description": "padrão true"},
                "auto_page": {"type": "boolean", "description": "envia espaço automaticamente em prompts de paginação (--More--), padrão true"},
                "expect": {"type": "string", "description": "regex do que esperar"},
                "timeout_secs": {"type": "integer", "description": "padrão 15, máximo 600"}
            }, "required": ["id"]}),
            "a saída nova do terminal",
            Permission::Shell,
            610,
            false,
            send_action,
            send,
        ),
        make_with(
            "terminal_close",
            "Fecha o terminal `id` e encerra o programa.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "confirmação",
            Permission::Shell,
            10,
            false,
            |_, _| Ok(None),
            close,
        ),
    ]
}

/// What the approval shows: the text typed into the terminal (raw when
/// non-empty so shell policy rules see the real command). Reading only (no
/// input) needs no approval.
fn send_action(args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
    let what = match (opt_str(args, "secret"), args["input"].as_str()) {
        (Some(n), _) => format!("[digita o segredo `{n}` do cofre]"),
        (None, Some(i)) if !i.trim().is_empty() => i.to_string(),
        _ => return Ok(None),
    };
    Ok(Some(Action::Shell(what)))
}

fn timeout(args: &Value, default: u64, max: u64) -> Duration {
    Duration::from_secs(
        args["timeout_secs"]
            .as_u64()
            .unwrap_or(default)
            .clamp(1, max),
    )
}

fn expect(args: &Value) -> Result<Option<Regex>> {
    opt_str(args, "expect")
        .map(|p| Regex::new(p).map_err(|e| ToolError::BadArguments(format!("expect: {e}"))))
        .transpose()
}

fn reap() {
    let mut dropped = Vec::new();
    {
        let mut t = lock();
        let dead: Vec<String> = t
            .iter()
            .filter_map(|(id, term)| {
                let expired = term
                    .meta
                    .last_used
                    .lock()
                    .map(|lu| lu.elapsed() >= IDLE_REAP)
                    .unwrap_or(false);
                (expired || !term.proc.alive() || !term.meta.alive.load(Ordering::SeqCst))
                    .then(|| id.clone())
            })
            .collect();
        for id in dead {
            if let Some(term) = t.remove(&id) {
                dropped.push(term);
            }
        }
    }
    let removed = !dropped.is_empty();
    drop(dropped);
    if removed {
        notify_changed();
    }
    // Past the reopen window a closed session is only an error message.
    let live: Vec<String> = lock().keys().cloned().collect();
    remembered().retain(|id, r| {
        live.contains(id)
            || r.last_used
                .lock()
                .is_ok_and(|t| t.elapsed() < REOPEN_WINDOW)
    });
}

pub(crate) struct OpenSpec<'a> {
    pub command: &'a str,
    pub cwd: &'a std::path::Path,
    pub expect: Option<&'a Regex>,
    pub timeout_dur: Duration,
    pub auto_page: bool,
    pub secrets: Vec<String>,
    pub vendor: Option<String>,
    pub device: Option<String>,
    pub host: Option<String>,
    pub role: Option<String>,
    pub client: Option<String>,
    pub vpn: Option<String>,
    pub transport: Option<String>,
}

pub(crate) fn open_with_meta(
    spec: OpenSpec<'_>,
    exec: &Exec<'_>,
) -> Result<(String, String, bool)> {
    ensure_reaper();
    reap();
    {
        let t = lock();
        if t.len() >= MAX_OPEN {
            return Err(ToolError::Failed(format!(
                "já há {MAX_OPEN} terminais abertos; feche um com terminal_close ({})",
                open_list(&t)
            )));
        }
    }
    let (proc, input_w, mut out) = spawn(spec.command, spec.cwd)
        .map_err(|e| ToolError::Failed(format!("não abriu `{}`: {e}", spec.command)))?;
    let buf = Arc::new(Mutex::new(VecDeque::new()));
    let fill = buf.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_reader = alive.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = out.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let mut v = fill.lock().unwrap_or_else(|e| e.into_inner());
            v.extend(&chunk[..n]);
            let overflow = v.len().saturating_sub(BUF_CAP);
            if overflow > 0 {
                v.drain(..overflow);
            }
        }
        alive_reader.store(false, Ordering::SeqCst);
        notify_changed();
    });
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = format!("term-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let input = Arc::new(Mutex::new(input_w));
    let last_used = Arc::new(Mutex::new(Instant::now()));
    let busy = Arc::new(AtomicBool::new(true));
    let meta = TermMeta {
        id: id.clone(),
        command: spec.command.to_string(),
        vendor: spec.vendor,
        device: spec.device,
        host: spec.host,
        role: spec.role,
        client: spec.client,
        vpn: spec.vpn,
        transport: spec.transport,
        owner: exec.host.session(),
        buf: buf.clone(),
        input: input.clone(),
        last_used,
        alive,
        busy,
    };
    if let Ok(mut m) = meta_table().lock() {
        m.insert(id.clone(), meta.clone());
    }
    remembered().insert(
        id.clone(),
        Remembered {
            owner: exec.host.session(),
            command: meta.command.clone(),
            vendor: meta.vendor.clone(),
            device: meta.device.clone(),
            role: meta.role.clone(),
            reopen: None,
            last_used: meta.last_used.clone(),
        },
    );
    notify_changed();
    let term = Term {
        meta: meta.clone(),
        owner: exec.host.session(),
        input,
        buf,
        proc,
        secrets: spec.secrets,
    };
    let deadline = exec.deadline.min(Instant::now() + spec.timeout_dur);
    let (text, note) = collect(
        "terminal_open",
        &term,
        spec.expect,
        deadline,
        spec.auto_page,
        None,
        false,
        exec,
    );
    let text = with_note(text, note);
    term.meta.busy.store(false, Ordering::SeqCst);
    if let Ok(mut u) = term.meta.last_used.lock() {
        *u = Instant::now();
    }
    let still_alive = term.proc.alive() && term.meta.alive.load(Ordering::SeqCst);
    if still_alive {
        lock().insert(id.clone(), term);
    }
    notify_changed();
    Ok((id, text, still_alive))
}

fn open(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    if ctx.policy.preset.sandbox == zerith_security::Sandbox::ReadOnly {
        return Err(ToolError::Denied(
            "terminal bloqueado no sandbox somente-leitura".into(),
        ));
    }
    let command = str_arg(args, "command")?;
    exec.host.before_shell(command, ctx.workspace);
    let re = expect(args)?;
    let auto_page = args["auto_page"].as_bool().unwrap_or(true);
    let (id, text, alive) = open_with_meta(
        OpenSpec {
            command,
            cwd: ctx.workspace,
            expect: re.as_ref(),
            timeout_dur: timeout(args, 20, 120),
            auto_page,
            secrets: Vec::new(),
            vendor: None,
            device: None,
            host: None,
            role: None,
            client: None,
            vpn: None,
            transport: None,
        },
        exec,
    )?;
    Ok(format!(
        "{id} ({command}){}\n{text}",
        if alive {
            ""
        } else {
            " — o programa já encerrou"
        }
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn send_session_command(
    tool_name: &str,
    id: &str,
    input_str: Option<&str>,
    secret_val: Option<String>,
    enter: bool,
    re: Option<&Regex>,
    timeout_dur: Duration,
    auto_page: bool,
    exec: &Exec<'_>,
) -> Result<(String, bool)> {
    send_session_command_with(
        tool_name,
        id,
        input_str,
        secret_val,
        enter,
        re,
        timeout_dur,
        auto_page,
        false,
        exec,
    )
}

/// `send_session_command`, plus `batch`: at the deadline the running command
/// gets `q`/Ctrl+C so the session is back at a prompt (the batch moves on to
/// its next command instead of typing into a monitor); an end marker is
/// typed after it (`sync_end`), so late output stays with its command; the
/// echo and the closing prompt are left out (the batch header has the
/// command).
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_session_command_with(
    tool_name: &str,
    id: &str,
    input_str: Option<&str>,
    secret_val: Option<String>,
    enter: bool,
    re: Option<&Regex>,
    timeout_dur: Duration,
    auto_page: bool,
    batch: bool,
    exec: &Exec<'_>,
) -> Result<(String, bool)> {
    let started = Instant::now();
    let typed_cmd = if secret_val.is_none() {
        input_str
    } else {
        None
    };
    let mut t = {
        let mut table = lock();
        let Some(t) = table.remove(id) else {
            drop(table);
            return Err(ToolError::Failed(missing_terminal(id)));
        };
        if t.owner.is_some() && t.owner != exec.host.session() {
            let err = format!("terminal {id} é de outra conversa");
            table.insert(id.to_string(), t);
            return Err(ToolError::Failed(err));
        }
        t
    };
    t.meta.busy.store(true, Ordering::SeqCst);
    notify_changed();
    let mut data = String::new();
    if let Some(v) = secret_val {
        data.push_str(&v);
        t.secrets.push(v);
    } else if let Some(i) = input_str {
        data.push_str(i);
    }
    let typed = !data.is_empty() || input_str.is_some();
    if typed && enter {
        data.push('\r');
    }
    if !data.is_empty() {
        let write_res = t
            .input
            .lock()
            .map_err(|e| ToolError::Failed(format!("terminal {id} travado: {e}")))
            .and_then(|mut w| {
                w.write_all(data.as_bytes())
                    .and_then(|_| w.flush())
                    .map_err(|e| ToolError::Failed(format!("terminal {id} fechado: {e}")))
            });
        write_res?;
    }
    let deadline = exec.deadline.min(Instant::now() + timeout_dur);
    let (mut text, mut note) = collect(
        tool_name, &t, re, deadline, auto_page, typed_cmd, batch, exec,
    );
    if batch {
        let prompt = t.meta.vendor.as_deref().and_then(vendor_prompt);
        text = without_echo(&without_prompt_tail(&text, prompt), typed_cmd);
        // The program asks something (`Password:`, `[y/N]?`): no marker.
        let asking = text
            .lines()
            .last()
            .is_some_and(|l| l.trim_end().ends_with([':', '?']));
        if let Some((late, synced)) = sync_end(&t, note.is_some(), asking, deadline, exec) {
            let late = without_prompt_tail(&late, prompt);
            if !late.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&late);
            }
            match synced {
                Synced::Clean => {}
                Synced::AfterStop => {
                    note.get_or_insert_with(|| Stop::Timeout(started).note(0));
                }
                Synced::Lost => {
                    note = Some(match note {
                        Some(n) => format!("{n}\n{UNSYNCED_NOTE}"),
                        None => UNSYNCED_NOTE.into(),
                    })
                }
            }
        }
    }
    let text = with_note(text, note);
    t.meta.busy.store(false, Ordering::SeqCst);
    if let Ok(mut u) = t.meta.last_used.lock() {
        *u = Instant::now();
    }
    let alive = t.proc.alive() && t.meta.alive.load(Ordering::SeqCst);
    if alive {
        lock().insert(id.to_string(), t);
    }
    notify_changed();
    Ok((text, alive))
}

fn send(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    let re = expect(args)?;
    let reopened = crate::network::reopen_if_dropped(id, ctx, exec)?.unwrap_or_default();
    let auto_page = args["auto_page"].as_bool().unwrap_or(true);
    let enter = args["enter"].as_bool().unwrap_or(true);
    // The vault read happens before the session is taken: an unknown name
    // must not leave the session out of the table.
    let secret = opt_str(args, "secret")
        .map(|n| {
            zerith_core::secrets::get_secret(n).map_err(|e| {
                ToolError::Failed(format!(
                    "segredo `{n}` não está no cofre ({e}); peça ao usuário: zerith secret set {n}"
                ))
            })
        })
        .transpose()?;
    // A device session (net_connect) knows its vendor: commands that never
    // end get their stop parameter, or are refused, before being typed.
    let vendor = terminal_meta(id)
        .and_then(|(v, _, _)| v)
        .and_then(|v| crate::network::Vendor::parse(&v));
    let (input, adjusted) = match (args["input"].as_str(), vendor) {
        (Some(i), Some(v)) if enter && secret.is_none() && !i.trim().is_empty() => {
            match crate::network::prepare_network_command(v, i) {
                Ok((c, n)) => (Some(c), n),
                Err(why) => return Ok(format!("{id}\n[não enviado pelo Zerith: {why}]")),
            }
        }
        (i, _) => (i.map(str::to_string), None),
    };
    let (text, alive) = send_session_command(
        "terminal_send",
        id,
        input.as_deref(),
        secret,
        enter,
        re.as_ref(),
        timeout(args, 15, 600),
        auto_page,
        exec,
    )?;
    Ok(format!(
        "{reopened}{id}{}\n{}{text}",
        if alive {
            ""
        } else {
            " — o programa encerrou; sessão fechada"
        },
        adjusted.map(|n| format!("{n}\n")).unwrap_or_default()
    ))
}

fn close(args: &Value, _: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    close_terminal(id)
        .map(|()| format!("{id} fechado"))
        .map_err(ToolError::Failed)
}

/// Waits for the program to settle, then hands over (and drops) everything
/// unread. Settled: the `expect` pattern showed up, or the output went quiet
/// on what looks like a prompt, or quiet for 2 s.
///
/// Output that never settles (monitor footer, a block repeating, too many
/// bytes, too many pages) is stopped with `q`/Ctrl+C and condensed. `cmd`
/// is what was typed: a monitor footer on a command with `count=`,
/// `duration=` or `once` is left to finish. `interrupt_on_timeout` (batch):
/// at the deadline the command is stopped too, so the next one finds a prompt.
#[allow(clippy::too_many_arguments)]
fn collect(
    tool: &str,
    t: &Term,
    expect: Option<&Regex>,
    deadline: Instant,
    auto_page: bool,
    cmd: Option<&str>,
    interrupt_on_timeout: bool,
    exec: &Exec<'_>,
) -> (String, Option<String>) {
    let len = || t.buf.lock().map(|b| b.len()).unwrap_or(0);
    let (mut last, mut changed) = (len(), Instant::now());
    let (mut progress_at, mut last_emit) = (0usize, Instant::now());
    let (mut pages, mut page_at) = (0usize, 0usize);
    let started = Instant::now();
    let watch_footer = !cmd.is_some_and(has_stop_param);
    let mut footer_at: Option<Instant> = None;
    // A prompt only ends the read once the device echoed what was typed:
    // RouterOS redraws the prompt in the middle of its own echo
    // (`/ip fir[admin@MK] > /ip firewall filter print`), and stopping there
    // handed each command the output of the one before, while the next
    // command got typed into the pager. No echo seen: a prompt needs 600 ms
    // of quiet. Idea from Hermes Agent's `terminal_tool.py` (MIT), which
    // waits on a known prompt instead of a fixed sleep.
    let echo = cmd.map(echo_tail).filter(|e| !e.is_empty());
    let mut echo_seen = echo.is_none();
    let strict = t.meta.vendor.as_deref().and_then(vendor_prompt);
    let mut analyzed = false;
    let mut pager_like = false;
    let mut strict_hit: Option<bool> = None;
    let mut has_newline = false;
    let mut prompt_like = false;
    let mut expect_hit = false;
    let mut monitor: Option<&'static [u8]> = None;
    let mut repeating_hit = false;
    let note = loop {
        std::thread::sleep(Duration::from_millis(12));
        let now = len();
        let buf_changed = now != last || !analyzed;
        if now != last {
            (last, changed) = (now, Instant::now());
        }
        if now > progress_at && last_emit.elapsed() >= Duration::from_millis(250) {
            if now.saturating_sub(progress_at) > LOOP_TAIL_BYTES {
                progress_at = now - LOOP_TAIL_BYTES;
            }
            let chunk_bytes: Vec<u8> = t
                .buf
                .lock()
                .map(|b| {
                    let start = progress_at.min(b.len());
                    let end = b.len().min(start + 1024);
                    b.range(start..end).copied().collect()
                })
                .unwrap_or_default();
            if !chunk_bytes.is_empty() {
                progress_at = (progress_at + chunk_bytes.len()).min(now);
                let mut text = clean(&chunk_bytes);
                for s in &t.secrets {
                    text = text.replace(s.as_str(), "[segredo]");
                }
                if !text.is_empty() {
                    exec.host.sink().push(crate::HostEvent::ToolProgress {
                        tool: tool.to_string(),
                        chunk: text,
                    });
                }
            }
            last_emit = Instant::now();
        }
        let quiet = changed.elapsed();
        if buf_changed {
            let tail_bytes: Vec<u8> = t
                .buf
                .lock()
                .map(|b| {
                    let start = b.len().saturating_sub(LOOP_TAIL_BYTES);
                    b.range(start..).copied().collect()
                })
                .unwrap_or_default();
            let tail_clean = clean(&tail_bytes);
            pager_like = looks_like_pager(&tail_clean);
            if auto_page && pages < MAX_AUTO_PAGES && now > page_at && pager_like {
                if let Ok(mut w) = t.input.lock() {
                    if w.write_all(b" ").and_then(|_| w.flush()).is_ok() {
                        pages += 1;
                        page_at = now;
                        changed = Instant::now();
                        analyzed = true;
                        continue;
                    }
                }
            }
            if !echo_seen {
                let head: Vec<u8> = t
                    .buf
                    .lock()
                    .map(|b| b.range(..b.len().min(ECHO_SCAN_BYTES)).copied().collect())
                    .unwrap_or_default();
                echo_seen = echo.as_deref().is_some_and(|e| clean(&head).contains(e));
            }
            strict_hit = strict.map(|re| {
                !tail_bytes.ends_with(b"\n")
                    && tail_clean.lines().last().is_some_and(|l| re.is_match(l))
            });
            has_newline = tail_clean.contains('\n');
            prompt_like = looks_like_prompt(&tail_clean);
            expect_hit = expect.is_some_and(|re| re.is_match(&tail_clean));
            monitor = monitor_key(&tail_clean);
            repeating_hit = !prompt_like && repeating_tail(&tail_clean).is_some();
            analyzed = true;
        }
        let pager_waiting = auto_page && pages < MAX_AUTO_PAGES && pager_like;
        // The vendor's own prompt alone on the last line, after the echo:
        // the command is over, no need to wait for quiet (30 ms only so a
        // line split by the network is not taken for one). The redrawn
        // prompt inside an echo (`/ip fir[admin@MK] >`) is not alone.
        let at_vendor_prompt =
            echo_seen && strict_hit == Some(true) && quiet >= Duration::from_millis(30);
        // Anything else that ends like a prompt waits a little for quiet;
        // longer before the echo, or when it is not the vendor's shape.
        let prompt_settle = match (echo_seen && strict_hit != Some(false), has_newline) {
            (false, _) => Duration::from_millis(600),
            (true, true) => Duration::from_millis(80),
            (true, false) => Duration::from_millis(120),
        };
        // A monitor footer keeps output flowing, so "quiet for 2 s" never
        // comes and the footer is not a prompt.
        let done = !pager_waiting
            && match expect {
                Some(_) => expect_hit,
                None => {
                    now > 0
                        && (at_vendor_prompt
                            || quiet >= Duration::from_secs(2) && monitor.is_none()
                            || (quiet >= prompt_settle && prompt_like))
                }
            };
        if done {
            break None;
        }
        // Never-ending output: stop the command instead of waiting for a
        // prompt that won't come (RouterOS `/ip route check` filled 12k lines
        // in 86 s). Footer: the first frame is drawn, give it 2 s to settle.
        if monitor.is_some() && watch_footer {
            let seen = *footer_at.get_or_insert_with(Instant::now);
            if seen.elapsed() >= Duration::from_secs(2) {
                break Some(Err(stop_continuous(t, monitor, Stop::Monitor)));
            }
        }
        if repeating_hit {
            break Some(Err(stop_continuous(t, monitor, Stop::Monitor)));
        }
        if now >= MAX_CMD_BYTES {
            break Some(Err(stop_continuous(t, monitor, Stop::Bytes)));
        }
        if auto_page && pages >= MAX_AUTO_PAGES && pager_like {
            break Some(Err(stop_continuous(t, Some(b"q"), Stop::Pages)));
        }
        if !t.proc.alive() || !t.meta.alive.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
            break Some(Ok("[o programa encerrou]".to_string()));
        }
        if exec.cancel.is_cancelled() {
            break Some(Ok("[cancelado; a sessão continua aberta]".into()));
        }
        if Instant::now() >= deadline {
            if interrupt_on_timeout && now > 0 {
                break Some(Err(stop_continuous(t, monitor, Stop::Timeout(started))));
            }
            break Some(Ok(match expect {
                Some(re) => format!(
                    "[tempo esgotado esperando /{}/; a sessão continua aberta]",
                    re.as_str()
                ),
                None if now == 0 => "[sem saída ainda; a sessão continua aberta — chame \
                                     terminal_send sem input para ler de novo]"
                    .into(),
                None => {
                    "[ainda chegando saída; chame terminal_send sem input para ler o resto]".into()
                }
            }));
        }
    };
    let raw: Vec<u8> = {
        let mut guard = t.buf.lock().unwrap_or_else(|e| e.into_inner());
        let drained: Vec<u8> = guard.drain(..).collect();
        if guard.capacity() > BUF_SHRINK_THRESHOLD {
            guard.shrink_to(BUF_SHRINK_THRESHOLD);
        }
        drained
    };
    let mut text = clean(&raw);
    for s in &t.secrets {
        text = text.replace(s.as_str(), "[segredo]");
    }
    let note = note.map(|n| match n {
        Ok(n) => n,
        Err(stop) => {
            let (short, runs) = condense_continuous(&text);
            text = short;
            stop.note(runs)
        }
    });
    let paged = (pages > 0).then(|| format!("{PAGED_NOTE} {pages} páginas]"));
    let note = match (paged, note) {
        (Some(p), Some(n)) => Some(format!("{p}\n{n}")),
        (p, n) => p.or(n),
    };
    (text, note)
}

/// Output with the reader's note (`[tempo esgotado…]`) on its own last line.
fn with_note(text: String, note: Option<String>) -> String {
    match note {
        Some(n) if text.is_empty() => n,
        Some(n) => format!("{text}\n{n}"),
        None => text,
    }
}

/// Batch command whose end marker never came back (the batch stops there).
pub(crate) const UNSYNCED_NOTE: &str =
    "[o equipamento não respondeu ao marcador de fim nem a q/Ctrl+C: a \
                             saída deste comando pode continuar no próximo]";

/// How the batch's end marker went (see [`sync_end`]).
enum Synced {
    /// Came back on the first try.
    Clean,
    /// Came back only after `q`/Ctrl+C.
    AfterStop,
    /// Never came back.
    Lost,
}

/// End marker for one batch command: a cheap read-only command and the
/// line it prints, which nothing else prints (the echo has the quotes or
/// `echo`). RouterOS `:put` writes its argument to the console and changes
/// no configuration (the `read` policy allows every console command that
/// does not change it). Cisco/Huawei/Junos/FortiOS exec modes have no echo
/// command: `None`, the prompt heuristic stays.
fn end_marker(vendor: &str, n: u64) -> Option<(String, String)> {
    let mark = format!("__ZB_END_{n}__");
    let cmd = match vendor {
        "mikrotik_router_os" => format!(":put \"{mark}\""),
        "linux" => format!("echo {mark}"),
        _ => return None,
    };
    Some((cmd, mark))
}

/// After a batch command: types an end marker and keeps reading until its
/// line comes back, so whatever the command still prints (a slow
/// `connection print` that answered after a 2 s silence or after the
/// deadline) stays with it and the next command starts on a clean session.
/// Waits until the command's `deadline` (3 s when it was already stopped),
/// then `q` + Ctrl+C, a new marker and 5 s more. Vendors without a marker:
/// after a stop only, waits up to 3 s for the prompt. Nothing is typed when
/// the output ends on a question (`Password:`, `[y/N]?`): the marker would
/// be the answer. Returns the late output (markers out) and how it went;
/// `None` when there was nothing to wait for.
// Idea from netmiko's `send_command` (MIT), which reads until a known
// pattern instead of trusting the prompt, and from pexpect's sentinel.
fn sync_end(
    t: &Term,
    stopped: bool,
    asking: bool,
    deadline: Instant,
    exec: &Exec<'_>,
) -> Option<(String, Synced)> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let vendor = t.meta.vendor.as_deref()?;
    let alive = || t.proc.alive() && t.meta.alive.load(Ordering::SeqCst);
    if asking || exec.cancel.is_cancelled() || !alive() {
        return None;
    }
    let send = |k: &[u8]| {
        if let Ok(mut w) = t.input.lock() {
            let _ = w.write_all(k).and_then(|_| w.flush());
        }
    };
    let seen = || {
        let raw: Vec<u8> = t
            .buf
            .lock()
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default();
        clean(&raw)
    };
    let take = |upto: Option<usize>| {
        let raw: Vec<u8> = t
            .buf
            .lock()
            .map(|mut b| b.drain(..).collect())
            .unwrap_or_default();
        let mut s = clean(&raw);
        for x in &t.secrets {
            s = s.replace(x.as_str(), "[segredo]");
        }
        s.lines()
            .take(upto.unwrap_or(usize::MAX))
            .filter(|l| !l.contains("__ZB_END_"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = vendor_prompt(vendor);
    if end_marker(vendor, 0).is_none() {
        if !stopped {
            return None;
        }
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && alive() {
            std::thread::sleep(Duration::from_millis(50));
            let s = seen();
            let last = s.lines().last().unwrap_or("");
            if prompt.map_or(looks_like_prompt(&s), |re| re.is_match(last)) {
                break;
            }
        }
        return Some((take(None), Synced::Clean));
    }
    let first_wait = if stopped {
        Instant::now() + Duration::from_secs(3)
    } else {
        deadline.max(Instant::now() + Duration::from_secs(2))
    };
    for round in 0..2 {
        if round == 1 {
            if !alive() {
                break;
            }
            send(b"q");
            std::thread::sleep(Duration::from_millis(100));
            send(b"\x03");
        }
        let (cmd, mark) = end_marker(vendor, NEXT.fetch_add(1, Ordering::Relaxed))?;
        send(format!("{cmd}\r").as_bytes());
        let until = if round == 0 {
            first_wait
        } else {
            Instant::now() + Duration::from_secs(5)
        };
        let (mut size, mut changed) = (0usize, Instant::now());
        while Instant::now() < until && alive() && !exec.cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(12));
            let s = seen();
            if s.len() != size {
                (size, changed) = (s.len(), Instant::now());
            }
            // Only the last marker typed: an earlier one may come back
            // before what `q`/Ctrl+C left on the screen.
            let lines: Vec<&str> = s.lines().collect();
            let Some(at) = lines.iter().rposition(|l| l.trim() == mark) else {
                // Echoed, answered and back at the prompt without the
                // marker line (`:put` refused): it will not come.
                let echoed = s.lines().any(|l| l.contains(&mark));
                let last = s.lines().last().unwrap_or("");
                let at_prompt = prompt.map_or(looks_like_prompt(&s), |re| re.is_match(last));
                if echoed && at_prompt && changed.elapsed() >= Duration::from_secs(1) {
                    return Some((take(None), Synced::Clean));
                }
                continue;
            };
            // The marker's own prompt follows; it is not the next command's.
            let settle = Instant::now() + Duration::from_millis(500);
            while Instant::now() < settle && seen().lines().count() <= at + 1 {
                std::thread::sleep(Duration::from_millis(12));
            }
            let how = if round == 0 {
                Synced::Clean
            } else {
                Synced::AfterStop
            };
            return Some((take(Some(at)), how));
        }
    }
    let how = if alive() { Synced::Lost } else { Synced::Clean };
    Some((take(None), how))
}

/// Batch output without the device's echo of `cmd` at the top: the plain
/// echo, or the one RouterOS redraws with the prompt in the middle and cut
/// at the terminal width (`/ip firewall connection [joao@MK] > /ip firewall
/// connection print count-only where d6"`). An echo line starts with the
/// command, maybe after a prompt, sharing the command's first 10 characters (all of
/// a shorter one); only lines at the top, at most 3.
fn without_echo(text: &str, cmd: Option<&str>) -> String {
    let Some(cmd) = cmd.map(str::trim).filter(|c| !c.is_empty()) else {
        return text.to_string();
    };
    let need = cmd.chars().count().min(10);
    let shares = |l: &str| {
        let n = l
            .chars()
            .zip(cmd.chars())
            .take_while(|(a, b)| a == b)
            .count();
        n >= need
    };
    let is_echo = |l: &str| {
        let l = l.trim();
        // `[admin@MK] > /ip ...`, `R1#show run`: prompt then the command.
        shares(l)
            || l.match_indices(['>', '#', '$', '%', ']'])
                .any(|(i, _)| shares(l[i + 1..].trim_start()))
    };
    // RouterOS reads the `q` that stopped the previous command as a typed
    // line and answers `no such item (4)` before the next echo (seen on a
    // CCR1036); it is the stop's leftover, not this command's output.
    let leftover = |l: &str| l.trim().starts_with("no such item (");
    let lines: Vec<&str> = text.lines().collect();
    let mut skip = 0;
    while skip < lines.len().min(4)
        && (lines[skip].trim().is_empty() || is_echo(lines[skip]) || leftover(lines[skip]))
    {
        skip += 1;
    }
    lines[skip..].join("\n")
}

/// Batch output without bare prompt lines (the one closing the command,
/// those Ctrl+C left) and blank edges.
fn without_prompt_tail(text: &str, prompt: Option<&Regex>) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !prompt.is_some_and(|re| re.is_match(l.trim())))
        .collect();
    lines.join("\n").trim_matches('\n').trim_end().to_string()
}

/// Why `collect` cut a command short.
enum Stop {
    Monitor,
    Bytes,
    Pages,
    Timeout(Instant),
}

impl Stop {
    fn note(&self, runs: usize) -> String {
        match self {
            Self::Monitor => format!(
                "[saída contínua interrompida pelo Zerith após {runs} repetições: o comando é um \
                 monitor; use count=/duration=/once ou o equivalente]"
            ),
            Self::Bytes => format!(
                "[saída contínua interrompida pelo Zerith após {} MB sem voltar o prompt; \
                 filtre (where/include/| no-more) ou limite com count=/duration=/once]",
                MAX_CMD_BYTES / (1024 * 1024)
            ),
            Self::Pages => format!(
                "[paginação interrompida pelo Zerith após {MAX_AUTO_PAGES} páginas; desligue a \
                 paginação (terminal length 0 / screen-length 0 temporary) ou filtre a saída]"
            ),
            Self::Timeout(t) => format!(
                "[tempo esgotado após {}s: o Zerith interrompeu o comando (q/Ctrl+C) e seguiu; \
                 aumente timeout_secs se ele é só lento]",
                t.elapsed().as_secs()
            ),
        }
    }
}

/// Stops a running command: `key` (RouterOS monitor footer → `q`), else
/// Ctrl+C; then waits briefly for the prompt, with Ctrl+C as a second try.
// Idea from Hermes (MIT), which sets PAGER=cat because a PTY makes pagers
// wait for `q`; network CLIs have no env, so the key is sent instead.
fn stop_continuous(t: &Term, key: Option<&'static [u8]>, why: Stop) -> Stop {
    let send = |k: &[u8]| {
        if let Ok(mut w) = t.input.lock() {
            let _ = w.write_all(k).and_then(|_| w.flush());
        }
    };
    let prompt_back = |wait: Duration| {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
            let tail: Vec<u8> = t
                .buf
                .lock()
                .map(|b| b.range(b.len().saturating_sub(512)..).copied().collect())
                .unwrap_or_default();
            if looks_like_prompt(&clean(&tail)) {
                return true;
            }
        }
        false
    };
    send(key.unwrap_or(b"\x03"));
    if !prompt_back(Duration::from_secs(3)) && key != Some(b"\x03") {
        send(b"\x03");
        prompt_back(Duration::from_secs(2));
    }
    why
}

/// Detects CLI pagination prompts at the tail of terminal output.
pub(crate) fn looks_like_pager(text: &str) -> bool {
    let tail = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    if tail.is_empty() {
        return false;
    }
    tail.contains("--more--")
        || tail.contains("-- more --")
        || tail.contains("---- more ----")
        || tail.contains("--- more ---")
        || tail.contains("---(more")
        || tail.contains("press any key")
        // RouterOS pager on a long `print`: `-- [Q quit|D dump|down]`.
        || (tail.contains("[q quit") && !is_monitor_line(&tail))
}

/// Footer of a command that never ends on its own. RouterOS monitors
/// (`/ip route check`, `/ping` without count, `/tool torch`,
/// `monitor-traffic`, `/interface ethernet monitor`) draw
/// `-- [Q quit|D dump|C-z pause]`; `print follow` draws `-- Ctrl-C to quit`.
fn is_monitor_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    (l.contains("[q quit") && l.contains("c-z pause")) || l.contains("ctrl-c to quit")
}

/// The key that stops the monitor whose footer is in `text`, if any.
fn monitor_key(text: &str) -> Option<&'static [u8]> {
    let line = text.lines().rev().find(|l| is_monitor_line(l))?;
    Some(if line.to_ascii_lowercase().contains("q quit") {
        b"q"
    } else {
        b"\x03"
    })
}

/// `count=`, `duration=`, `once`: the command says when it stops, so a
/// monitor footer on screen is not a reason to cut it.
fn has_stop_param(cmd: &str) -> bool {
    let l = cmd.to_ascii_lowercase();
    l.contains("count=")
        || l.contains("duration=")
        || l.split_whitespace().any(|w| w == "once" || w == "once=yes")
}

/// Size of a block of 1–6 lines repeated `REPEAT_STOP` times in a row at
/// the end of `text` (the last line, maybe half-written, left out). Blocks
/// with under 6 visible characters (`!` separators) don't count.
fn repeating_tail(text: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let lines = &lines[..lines.len().saturating_sub(1)];
    (1..=6).find(|&k| {
        // One line alone repeats legitimately more often (`Request timed out.`).
        let need = k * if k == 1 { 4 * REPEAT_STOP } else { REPEAT_STOP };
        if lines.len() < need {
            return false;
        }
        let tail = &lines[lines.len() - need..];
        let visible: usize = tail[..k].iter().map(|l| l.trim().len()).sum();
        visible >= 6 && tail.chunks(k).all(|c| c == &tail[..k])
    })
}

/// Collapses each run of the same 1–6 line block (3+ copies in a row) to one
/// copy, drops monitor footers, and keeps head + tail when still long.
/// Returns the text and the longest run collapsed.
fn condense_continuous(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.lines().filter(|l| !is_monitor_line(l)).collect();
    let (mut out, mut longest, mut i) = (Vec::new(), 1usize, 0usize);
    while i < lines.len() {
        let best = (1..=6)
            .filter(|&k| i + 2 * k <= lines.len())
            .map(|k| {
                let block = &lines[i..i + k];
                let runs = lines[i..]
                    .chunks_exact(k)
                    .take_while(|c| *c == block)
                    .count();
                (runs, k)
            })
            .filter(|&(runs, _)| runs >= 3)
            .max_by_key(|&(runs, k)| (runs * k, std::cmp::Reverse(k)));
        match best {
            Some((runs, k)) => {
                out.extend_from_slice(&lines[i..i + k]);
                longest = longest.max(runs);
                i += runs * k;
            }
            None => {
                out.push(lines[i]);
                i += 1;
            }
        }
    }
    if out.len() > CUT_HEAD_LINES + CUT_TAIL_LINES {
        let cut = out.len() - CUT_HEAD_LINES - CUT_TAIL_LINES;
        let tail = out.split_off(out.len() - CUT_TAIL_LINES);
        out.truncate(CUT_HEAD_LINES);
        out.push("");
        let mut s = out.join("\n");
        s.push_str(&format!("[… {cut} linhas omitidas …]\n"));
        s.push_str(&tail.join("\n"));
        return (s, longest);
    }
    (out.join("\n"), longest)
}

/// How far into a command's output its echo is looked for.
const ECHO_SCAN_BYTES: usize = 4096;

/// The part of a typed command its echo must show: the last 24 characters
/// (a long line may wrap or scroll at the start, never at the end).
fn echo_tail(cmd: &str) -> String {
    let c: Vec<char> = cmd.trim().chars().collect();
    c[c.len().saturating_sub(24)..].iter().collect()
}

/// A line that is exactly the vendor's prompt (after `clean`, trailing
/// space gone): RouterOS `[admin@MK] >` / `[admin@MK] /ip/route>`, Cisco
/// `R1#` / `R1(config-if)#`, Huawei `<HW>` / `[~HW-Gi0/0/1]`, Junos
/// `user@mx>`, FortiOS `FGT60E #`, Linux `user@host:~$`. `None`: no
/// known shape, the read waits for quiet.
fn vendor_prompt(vendor: &str) -> Option<&'static Regex> {
    static RES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            // `<SAFE>` in safe mode.
            (
                "mikrotik_router_os",
                r"^\[[^\[\]\s]+@[^\[\]]+\] ?(/\S*)?(>|<SAFE>)$",
            ),
            ("cisco_ios", r"^[\w.\-/:]+(\([\w.\-]+\))?[#>]$"),
            ("cisco_nxos", r"^[\w.\-/:]+(\([\w.\-]+\))?[#>]$"),
            ("cisco_xr", r"^(RP/\S+:)?[\w.\-/]+(\([\w.\-]+\))?[#>]$"),
            ("huawei_vrp", r"^(<[\w.\-~]+>|\[~?\*?[\w.\-/:~]+\])$"),
            (
                "juniper_junos",
                r"^(\{\w+(:\d+)?\}\s*)?[\w.\-]+@[\w.\-]+[>#%]$",
            ),
            ("fortinet_forti_os", r"^[\w.\-]+( \([\w.\-]+\))? [#$]$"),
            ("linux", r"^[\w.\-]+@[\w.\-]+(:\S*)?\s?[$#]$"),
        ]
        .into_iter()
        .map(|(v, p)| (v, Regex::new(p).expect("static regex")))
        .collect()
    });
    res.iter().find(|(v, _)| *v == vendor).map(|(_, re)| re)
}

/// The last line ends like a prompt: `Router#`, `<Huawei>`, `[admin@MikroTik] >`,
/// `$`, `Password:`, `(yes/no)?`. A pager or monitor footer is not one.
fn looks_like_prompt(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| {
            l.trim_end().ends_with(['>', '#', '$', '%', ']', ':', '?'])
                && !is_monitor_line(l)
                && !looks_like_pager(l)
        })
}

/// Terminal output as text: escape sequences (colors, cursor moves, titles)
/// and carriage returns out, trailing padding trimmed.
// ponytail: a repaint that moves the cursor up shows as repeated lines; a
// real screen model (vt100 crate) if a full-screen program ever matters.
fn clean(raw: &[u8]) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let re = ANSI.get_or_init(|| {
        Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78DEHMNOZc]")
            .expect("static regex")
    });
    let s = String::from_utf8_lossy(raw);
    let s = re.replace_all(&s, "");
    s.split('\n')
        .map(|l| {
            l.chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

type Spawned = (Proc, Box<dyn Write + Send>, Box<dyn Read + Send>);

enum Proc {
    Os(OsProc),
    Socket {
        alive: Arc<AtomicBool>,
        stream: std::net::TcpStream,
    },
}

impl Proc {
    fn alive(&self) -> bool {
        match self {
            Self::Os(p) => p.alive(),
            Self::Socket { alive, .. } => alive.load(Ordering::SeqCst),
        }
    }

    fn kill(&mut self) {
        match self {
            Self::Os(p) => p.kill(),
            Self::Socket { alive, stream } => {
                alive.store(false, Ordering::SeqCst);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Stateful Telnet RFC 854 IAC parser that buffers incomplete IAC or
/// subnegotiation (`IAC SB ... IAC SE`) byte sequences across TCP chunk reads.
#[derive(Default)]
pub(crate) struct TelnetIacParser {
    pending: Vec<u8>,
}

impl TelnetIacParser {
    pub(crate) fn process(&mut self, chunk: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let buf: std::borrow::Cow<'_, [u8]> = if self.pending.is_empty() {
            std::borrow::Cow::Borrowed(chunk)
        } else {
            let mut combined = std::mem::take(&mut self.pending);
            combined.extend_from_slice(chunk);
            std::borrow::Cow::Owned(combined)
        };
        let raw = buf.as_ref();
        let mut clean_out = Vec::with_capacity(raw.len());
        let mut replies = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] != 0xFF {
                clean_out.push(raw[i]);
                i += 1;
                continue;
            }
            if i + 1 >= raw.len() {
                self.pending.extend_from_slice(&raw[i..]);
                break;
            }
            let cmd = raw[i + 1];
            match cmd {
                0xFF => {
                    clean_out.push(0xFF);
                    i += 2;
                }
                // IAC DO <opt>
                0xFD => {
                    if i + 2 >= raw.len() {
                        self.pending.extend_from_slice(&raw[i..]);
                        break;
                    }
                    let opt = raw[i + 2];
                    if opt == 0x03 {
                        // SUPPRESS-GO-AHEAD -> IAC WILL SGA
                        replies.extend_from_slice(&[0xFF, 0xFB, 0x03]);
                    } else if opt == 0x1F {
                        // NAWS -> IAC WILL NAWS + subnegotiation 250x60
                        replies.extend_from_slice(&[
                            0xFF, 0xFB, 0x1F, 0xFF, 0xFA, 0x1F, 0x00, 250, 0x00, 60, 0xFF, 0xF0,
                        ]);
                    } else {
                        // IAC WONT <opt>
                        replies.extend_from_slice(&[0xFF, 0xFC, opt]);
                    }
                    i += 3;
                }
                // IAC WILL <opt>
                0xFB => {
                    if i + 2 >= raw.len() {
                        self.pending.extend_from_slice(&raw[i..]);
                        break;
                    }
                    let opt = raw[i + 2];
                    if opt == 0x01 || opt == 0x03 {
                        // ECHO or SGA -> IAC DO <opt>
                        replies.extend_from_slice(&[0xFF, 0xFD, opt]);
                    } else {
                        // IAC DONT <opt>
                        replies.extend_from_slice(&[0xFF, 0xFE, opt]);
                    }
                    i += 3;
                }
                // IAC WONT / IAC DONT
                0xFC | 0xFE => {
                    if i + 2 >= raw.len() {
                        self.pending.extend_from_slice(&raw[i..]);
                        break;
                    }
                    i += 3;
                }
                // IAC SB ... IAC SE
                0xFA => {
                    let mut j = i + 2;
                    let mut closed = false;
                    while j + 1 < raw.len() {
                        if raw[j] == 0xFF {
                            if raw[j + 1] == 0xF0 {
                                j += 2;
                                closed = true;
                                break;
                            } else if raw[j + 1] == 0xFF {
                                j += 2;
                                continue;
                            }
                        }
                        j += 1;
                    }
                    if !closed {
                        if raw.len() - i <= 4096 {
                            self.pending.extend_from_slice(&raw[i..]);
                        }
                        break;
                    }
                    i = j;
                }
                _ => {
                    i += 2;
                }
            }
        }
        (clean_out, replies)
    }
}

struct TelnetReader {
    stream: std::net::TcpStream,
    alive: Arc<AtomicBool>,
    iac: TelnetIacParser,
    pending_clean: Vec<u8>,
}

impl Read for TelnetReader {
    fn read(&mut self, out_buf: &mut [u8]) -> std::io::Result<usize> {
        if out_buf.is_empty() {
            return Ok(0);
        }
        if !self.pending_clean.is_empty() {
            let copy_len = self.pending_clean.len().min(out_buf.len());
            out_buf[..copy_len].copy_from_slice(&self.pending_clean[..copy_len]);
            self.pending_clean.drain(..copy_len);
            return Ok(copy_len);
        }
        let mut raw = [0u8; 4096];
        loop {
            let n = match self.stream.read(&mut raw) {
                Ok(0) => {
                    self.alive.store(false, Ordering::SeqCst);
                    return Ok(0);
                }
                Ok(n) => n,
                Err(e) => {
                    self.alive.store(false, Ordering::SeqCst);
                    return Err(e);
                }
            };
            let (clean_bytes, reply) = self.iac.process(&raw[..n]);
            if !reply.is_empty() {
                let _ = self.stream.write_all(&reply);
                let _ = self.stream.flush();
            }
            if !clean_bytes.is_empty() {
                let copy_len = clean_bytes.len().min(out_buf.len());
                out_buf[..copy_len].copy_from_slice(&clean_bytes[..copy_len]);
                if copy_len < clean_bytes.len() {
                    self.pending_clean
                        .extend_from_slice(&clean_bytes[copy_len..]);
                }
                return Ok(copy_len);
            }
        }
    }
}

struct TelnetWriter {
    stream: std::net::TcpStream,
}

impl Write for TelnetWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut norm = Vec::with_capacity(buf.len() + 2);
        for (idx, &b) in buf.iter().enumerate() {
            if b == b'\r' && buf.get(idx + 1).copied() != Some(b'\n') {
                norm.extend_from_slice(b"\r\n");
            } else {
                norm.push(b);
            }
        }
        self.stream.write_all(&norm)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn spawn_telnet(target: &str) -> std::io::Result<Spawned> {
    use std::net::{TcpStream, ToSocketAddrs};
    let trimmed = target.trim();
    let (host, port) = match trimmed.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !h.contains(':') => {
            let port = p.parse::<u16>().unwrap_or(23);
            (h, port)
        }
        _ => (trimmed, 23u16),
    };
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("DNS não resolveu {host}:{port}")))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    let _ = stream.set_nodelay(true);
    let r_stream = stream.try_clone()?;
    let w_stream = stream.try_clone()?;
    let alive = Arc::new(AtomicBool::new(true));
    Ok((
        Proc::Socket {
            alive: alive.clone(),
            stream,
        },
        Box::new(TelnetWriter { stream: w_stream }),
        Box::new(TelnetReader {
            stream: r_stream,
            alive,
            iac: TelnetIacParser::default(),
            pending_clean: Vec::new(),
        }),
    ))
}

fn spawn(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
    if let Some(target) = command.trim().strip_prefix("telnet://") {
        return spawn_telnet(target);
    }
    spawn_os(command, cwd)
}

#[cfg(windows)]
struct OsProc {
    hpc: isize,
    process: windows_sys::Win32::Foundation::HANDLE,
    /// Kill-on-close: closing the terminal ends everything the program
    /// started (ssh's helpers, a REPL's children), not just the top process.
    job: Option<zerith_security::jail::JobHandle>,
}

// SAFETY: the handles are plain kernel handles, used from one thread at a time
// (a session is taken out of the table while a call works on it).
#[cfg(windows)]
unsafe impl Send for OsProc {}

#[cfg(windows)]
impl OsProc {
    fn alive(&self) -> bool {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        !self.process.is_null() && unsafe { WaitForSingleObject(self.process, 0) } == WAIT_TIMEOUT
    }

    fn kill(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Console::ClosePseudoConsole;
        use windows_sys::Win32::System::Threading::TerminateProcess;
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        unsafe {
            if !self.process.is_null() {
                TerminateProcess(self.process, 1);
                CloseHandle(self.process);
                self.process = std::ptr::null_mut();
            }
            if self.hpc != 0 {
                ClosePseudoConsole(self.hpc);
                self.hpc = 0;
            }
        }
    }
}

/// The program under a ConPTY: it sees a real console (so ssh asks for the
/// password, a REPL shows its prompt) and we see the console's VT output.
#[cfg(windows)]
fn spawn_os(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
        ResumeThread, TerminateProcess, UpdateProcThreadAttribute, CREATE_SUSPENDED,
        CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
        STARTUPINFOEXW,
    };
    let last = std::io::Error::last_os_error;
    unsafe {
        let (mut in_r, mut in_w, mut out_r, mut out_w) =
            (null_mut(), null_mut(), null_mut(), null_mut());
        if CreatePipe(&mut in_r, &mut in_w, null(), 0) == 0 {
            return Err(last());
        }
        if CreatePipe(&mut out_r, &mut out_w, null(), 0) == 0 {
            let e = last();
            CloseHandle(in_r);
            CloseHandle(in_w);
            return Err(e);
        }
        // Wide enough that a router's long config lines do not wrap.
        let mut hpc = 0isize;
        let hr = CreatePseudoConsole(COORD { X: 250, Y: 60 }, in_r, out_w, 0, &mut hpc);
        // The console holds its own copies of these two ends.
        CloseHandle(in_r);
        CloseHandle(out_w);
        let fail = |e: std::io::Error| {
            CloseHandle(in_w);
            CloseHandle(out_r);
            e
        };
        if hr != 0 {
            return Err(fail(std::io::Error::other(format!(
                "CreatePseudoConsole falhou (0x{hr:08x})"
            ))));
        }
        let mut size = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0
            || UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const core::ffi::c_void,
                std::mem::size_of::<isize>(),
                null_mut(),
                null(),
            ) == 0
        {
            let e = last();
            ClosePseudoConsole(hpc);
            return Err(fail(e));
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        // Without this a child of a process whose stdio is redirected (the CLI
        // under a pipe) writes to those pipes instead of the console.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let mut line: Vec<u16> = std::ffi::OsStr::new(command)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let dir: Vec<u16> = cwd.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = CreateProcessW(
            null(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            null(),
            dir.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(list);
        if ok == 0 {
            let e = last();
            ClosePseudoConsole(hpc);
            return Err(fail(e));
        }
        // Suspended until it sits in the job, so nothing it starts escapes.
        let job = match zerith_security::jail::contain(pi.hProcess) {
            Ok(j) => j,
            Err(e) => {
                TerminateProcess(pi.hProcess, 1);
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
                ClosePseudoConsole(hpc);
                return Err(fail(e));
            }
        };
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        Ok((
            Proc::Os(OsProc {
                hpc,
                process: pi.hProcess,
                job: Some(job),
            }),
            Box::new(File::from_raw_handle(in_w as _)),
            Box::new(File::from_raw_handle(out_r as _)),
        ))
    }
}

#[cfg(not(windows))]
struct OsProc(Mutex<std::process::Child>);

#[cfg(not(windows))]
impl OsProc {
    fn alive(&self) -> bool {
        let mut child = self.0.lock().unwrap_or_else(|e| e.into_inner());
        matches!(child.try_wait(), Ok(None))
    }

    fn kill(&mut self) {
        let mut child = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        #[cfg(unix)]
        {
            let pgid = child.id();
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &format!("-{pgid}")])
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(not(windows))]
impl Drop for OsProc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `script` gives the program a pseudo-terminal without a pty crate.
#[cfg(not(windows))]
fn spawn_os(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let pty_cmd = format!("stty cols 250 rows 60 2>/dev/null; {command}");
    let mut c = Command::new("script");
    if cfg!(target_os = "macos") {
        c.args(["-q", "/dev/null", "sh", "-c", &pty_cmd]);
    } else {
        c.args(["-qfec", &pty_cmd, "/dev/null"]);
    }
    #[cfg(unix)]
    c.process_group(0);
    let mut child = c
        .env("COLUMNS", "250")
        .env("LINES", "60")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("sem stdin"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("sem stdout"))?;
    Ok((
        Proc::Os(OsProc(Mutex::new(child))),
        Box::new(input),
        Box::new(output),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_and_padding_are_dropped() {
        let raw = b"\x1b[?25l\x1b[2J\x1b[H\x1b]0;title\x07<Huawei>   \r\n\x1b[32mok\x1b[0m\r\n";
        assert_eq!(clean(raw), "<Huawei>\nok");
        assert!(looks_like_prompt("banner\n[admin@MikroTik] > "));
        assert!(looks_like_prompt("Password: "));
        assert!(!looks_like_prompt("carregando 50%... feito"));
        assert!(looks_like_pager("interface Eth0/0/1\n  ---- More ----"));
        assert!(looks_like_pager("show bgp\n--More--"));
        assert!(looks_like_pager("---(more 42%)---"));
        assert!(!looks_like_pager("Router#"));
    }

    #[test]
    fn vendor_prompts_are_told_from_an_echo_with_the_prompt_inside() {
        let ok = |v: &str, l: &str| vendor_prompt(v).unwrap().is_match(l);
        let mk = "mikrotik_router_os";
        assert!(ok(mk, "[admin@MK] >"));
        assert!(ok(mk, "[admin@Borda SP-01] /ip/route>"));
        assert!(ok(mk, "[admin@MK] <SAFE>"));
        assert!(!ok(mk, "/ip fir[admin@MK] >"));
        assert!(!ok(mk, "[admin@MK] > /ip firewall filter print"));
        assert!(ok("cisco_ios", "R1(config-if)#"));
        assert!(!ok("cisco_ios", "R1#show run"));
        assert!(ok("huawei_vrp", "<HW-CORE>"));
        assert!(ok("huawei_vrp", "[~HW-CORE-GigabitEthernet0/0/1]"));
        assert!(ok("juniper_junos", "netops@mx480>"));
        assert!(ok("linux", "root@pve:~#"));
        assert!(vendor_prompt("generic").is_none());
        assert_eq!(
            echo_tail("/ip firewall filter print terse"),
            "ewall filter print terse"
        );
    }

    /// Echo lines taken from a CCR1036 (RouterOS 7.24.2) batch.
    #[test]
    fn a_broken_echo_is_dropped_from_the_top_of_a_batch_section() {
        let cut = |text: &str, cmd: &str| without_echo(text, Some(cmd));
        let c = r#"/ip firewall connection print count-only where dst-address~"179.108.156.6""#;
        assert_eq!(
            cut(
                &format!("{}[joao72991@Masterlink] > /ip firewall connection print count-only where dst-a\n1", &c[..60]),
                c
            ),
            "1"
        );
        let m = "/ip firewall mangle print terse without-paging";
        assert_eq!(
            cut(
                "/ip firewall mangle prin[joao72991@Masterlink] > /ip firewall mangle print terse without-pagingwithout-paging\n0 chain=prerouting",
                m
            ),
            "0 chain=prerouting"
        );
        // The previous command's stop left `no such item (4)` on top (real
        // CCR1036 output).
        assert_eq!(
            cut(
                "no such item (4)\n\n[joao72991@Masterlink] >/ip firewall connection print count-only where dst-address~\"179.108.156.6[joao72991@Masterlink] > /ip firewall connection print count-only where dst-address~\"179.108.156.6\"\n1",
                c
            ),
            "1"
        );
        // Output that only looks like the command stays.
        assert_eq!(
            cut("ok\n/ip firewall", "/ip firewall nat print"),
            "ok\n/ip firewall"
        );
        assert_eq!(cut("uptime: 1d", "/system resource print"), "uptime: 1d");
        assert_eq!(
            without_prompt_tail("1\n[joao@MK] >\n", vendor_prompt("mikrotik_router_os")),
            "1"
        );
    }

    #[test]
    fn monitor_footer_pager_and_repeats_are_told_apart() {
        let frame = "      status: ok\n   interface: sfp-sfpplus1\n     nexthop: 10.0.0.1\n";
        let footer = "-- [Q quit|D dump|C-z pause]";
        // RouterOS monitor: not a pager, not a prompt, stopped with `q`.
        assert!(!looks_like_pager(footer));
        assert!(!looks_like_prompt(&format!("{frame}{footer}")));
        assert_eq!(monitor_key(&format!("{frame}{footer}")), Some(&b"q"[..]));
        assert_eq!(monitor_key("x\n-- Ctrl-C to quit."), Some(&b"\x03"[..]));
        // RouterOS pager on a long print still pages.
        assert!(looks_like_pager("0 ether1\n-- [Q quit|D dump|down]"));
        assert!(has_stop_param("/ping 8.8.8.8 count=4"));
        assert!(has_stop_param("/interface monitor-traffic ether1 once"));
        assert!(!has_stop_param("/ip route check 1.1.1.1"));
        // Five copies of the 3-line frame, last line still being written.
        let looped = format!("{}      sta", frame.repeat(5));
        assert_eq!(repeating_tail(&looped), Some(3));
        assert_eq!(repeating_tail(&frame.repeat(4)), None);
        assert_eq!(repeating_tail("!\n!\n!\n!\n!\n!\n!\n"), None);
        let (short, runs) = condense_continuous(&format!(
            "[admin@MK] > /ip route check 1.1.1.1\n{}",
            format!("{frame}{footer}\n").repeat(400)
        ));
        assert_eq!(runs, 400);
        assert_eq!(short.matches("status: ok").count(), 1, "{short}");
        assert!(!short.contains("Q quit"), "{short}");
    }

    fn exec_ctx(f: impl FnOnce(&CallCtx<'_>, &Exec<'_>)) {
        let ws = std::env::current_dir().unwrap();
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Bypass,
            &ws,
        );
        let ctx = CallCtx {
            workspace: &ws,
            policy: &policy,
            memory: None,
            scope: None,
        };
        let cancel = zerith_providers::Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(60),
            timeout: Duration::from_secs(60),
            host: &host,
        };
        f(&ctx, &exec);
    }

    /// A program that asks a question and waits: the case `shell` hangs on.
    /// Not on macOS: BSD `script` wants a terminal on stdin, which CI lacks.
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn a_program_that_waits_for_input_gets_it_across_calls() {
        let cmd = if cfg!(windows) {
            "cmd /Q /K prompt $G"
        } else {
            "sh -c 'printf \"nome? \"; read n; echo \"oi $n\"; printf \"> \"; read x'"
        };
        exec_ctx(|ctx, exec| {
            let first = open(&json!({"command": cmd}), ctx, exec).unwrap();
            let id = first.split_whitespace().next().unwrap().to_string();
            assert!(id.starts_with("term-"), "{first}");
            let input = if cfg!(windows) {
                "echo oi zerith"
            } else {
                "zerith"
            };
            let out = send(&json!({"id": id, "input": input}), ctx, exec).unwrap();
            assert!(out.contains("oi zerith"), "{out}");
            assert!(terminal_program(&id).is_some());
            close(&json!({"id": id}), ctx, exec).unwrap();
            assert!(terminal_program(&id).is_none());
        });
    }

    #[test]
    fn send_action_passes_raw_command_and_open_enforces_readonly_and_before_shell() {
        let ws = std::env::current_dir().unwrap();
        let ro = zerith_security::Policy::new(
            zerith_security::preset("read-only").unwrap(),
            zerith_security::Mode::Bypass,
            &ws,
        );
        let ctx = CallCtx {
            workspace: &ws,
            policy: &ro,
            memory: None,
            scope: None,
        };
        assert_eq!(
            send_action(&json!({"id": "term-1", "input": "rm -rf /\n"}), &ctx).unwrap(),
            Some(Action::Shell("rm -rf /\n".into()))
        );
        assert_eq!(
            send_action(&json!({"id": "term-1", "input": "   "}), &ctx).unwrap(),
            None
        );
        let cancel = zerith_providers::Cancel::default();
        let host = crate::Host::default();
        let exec = Exec {
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(5),
            timeout: Duration::from_secs(5),
            host: &host,
        };
        assert!(matches!(
            open(&json!({"command": "sh"}), &ctx, &exec),
            Err(ToolError::Denied(_))
        ));

        let hook_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_flag = hook_called.clone();
        host.on_before_shell(move |_| {
            hook_flag.store(true, Ordering::SeqCst);
        });
        let rw = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Bypass,
            &ws,
        );
        let rw_ctx = CallCtx {
            workspace: &ws,
            policy: &rw,
            memory: None,
            scope: None,
        };
        // Call open with an immediately failing command (or invalid binary) after checking before_shell:
        let _ = open(
            &json!({"command": "nonexistent_cmd_for_hook_check"}),
            &rw_ctx,
            &exec,
        );
        assert!(hook_called.load(Ordering::SeqCst));
    }

    #[test]
    fn telnet_iac_parser_buffers_incomplete_sequences_across_chunks() {
        let mut p = TelnetIacParser::default();

        // Chunk 1 ends right on 0xFF (IAC).
        let (clean1, rep1) = p.process(b"Hello\xff");
        assert_eq!(clean1, b"Hello");
        assert!(rep1.is_empty());

        // Chunk 2 completes IAC DO NAWS (0xFD 0x1F) and starts IAC WILL (0xFF 0xFB).
        let (clean2, rep2) = p.process(b"\xfd\x1f world\xff\xfb");
        assert_eq!(clean2, b" world");
        assert_eq!(
            rep2,
            vec![0xFF, 0xFB, 0x1F, 0xFF, 0xFA, 0x1F, 0x00, 250, 0x00, 60, 0xFF, 0xF0]
        );

        // Chunk 3 completes IAC WILL ECHO (0x01) and starts an unclosed IAC SB (with escaped 0xFF 0xFF inside).
        let (clean3, rep3) = p.process(b"\x01!\xff\xfa\x18\x00xterm\xff\xff");
        assert_eq!(clean3, b"!");
        assert_eq!(rep3, vec![0xFF, 0xFD, 0x01]);

        // Chunk 4 closes the subnegotiation with IAC SE (0xFF 0xF0) followed by prompt text.
        let (clean4, rep4) = p.process(b"-256color\xff\xf0Login:");
        assert_eq!(clean4, b"Login:");
        assert!(rep4.is_empty());
    }

    /// Live check of `zerith chat` asking s/n on a real console. Needs the
    /// built CLI and a model: `ZERITH_LIVE_CHAT=<ZERITH_HOME> cargo test -p
    /// zerith-tools live_chat -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZERITH_LIVE_CHAT=<home with a model> and target/debug/zerith"]
    fn live_chat_asks_on_the_terminal_and_obeys_the_answer() {
        let Ok(home) = std::env::var("ZERITH_LIVE_CHAT") else {
            return;
        };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ws = root.join("target/chat-live");
        std::fs::create_dir_all(&ws).unwrap();
        let _ = std::fs::remove_file(ws.join("ok.txt"));
        std::env::set_current_dir(&ws).unwrap();
        std::env::set_var("ZERITH_HOME", &home);
        std::env::set_var("ZERITH_MODE", "default");
        let exe = root
            .join("target/debug/zerith")
            .canonicalize()
            .unwrap_or(root.join("target/debug/zerith.exe"));
        let cmd = format!(
            "\"{}\" chat --model openrouter:anthropic/claude-haiku-4.5 \"Use write_file para criar ok.txt cujo conteudo e exatamente a palavra aprovado. Nao pergunte nada. Depois responda feito.\"",
            exe.display()
        );
        exec_ctx(|ctx, exec| {
            let out = open(
                &json!({"command": cmd, "expect": "Permitir\\?", "timeout_secs": 90}),
                ctx,
                exec,
            )
            .unwrap();
            eprintln!("--- open\n{out}");
            assert!(out.contains("[aprovação]"), "{out}");
            let id = out.split_whitespace().next().unwrap().to_string();
            let done = send(
                &json!({"id": id, "input": "s", "expect": "(?i)feito", "timeout_secs": 90}),
                ctx,
                exec,
            )
            .unwrap();
            eprintln!("--- after s\n{done}");
            let _ = close(&json!({"id": id}), ctx, exec);
        });
        assert_eq!(
            std::fs::read_to_string(ws.join("ok.txt")).unwrap().trim(),
            "aprovado"
        );
    }
}
