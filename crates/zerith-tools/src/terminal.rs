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
const MAX_AUTO_PAGES: usize = 64;
const LOOP_TAIL_BYTES: usize = 4096;
const IDLE_REAP: Duration = Duration::from_secs(20 * 60);
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

/// The program a terminal session runs, for the approval rules (a session
/// on `ssh` is remote access; every input to it is approved on its own).
pub fn terminal_program(id: &str) -> Option<String> {
    meta_table().lock().ok()?.get(id).map(|t| t.command.clone())
}

/// `(vendor, device, role)` metadata for an active terminal session.
pub fn terminal_meta(id: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    meta_table()
        .lock()
        .ok()?
        .get(id)
        .map(|t| (t.vendor.clone(), t.device.clone(), t.role.clone()))
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
                idle_secs,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
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
    Err(format!("terminal {id} não existe"))
}

/// Closes all open interactive PTY/ConPTY sessions (used on app exit).
pub fn close_all_terminals() {
    let drained: Vec<Term> = {
        let mut t = lock();
        t.drain().map(|(_, term)| term).collect()
    };
    let had_any = !drained.is_empty();
    drop(drained);
    if had_any {
        notify_changed();
    }
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

/// What the approval shows: the session's program and the text typed into
/// it. Reading only (no input) needs no approval.
fn send_action(args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
    let id = args["id"].as_str().unwrap_or("");
    let what = match (opt_str(args, "secret"), args["input"].as_str()) {
        (Some(n), _) => format!("[digita o segredo `{n}` do cofre]"),
        (None, Some(i)) => i.escape_debug().to_string(),
        (None, None) => return Ok(None),
    };
    let prog = terminal_program(id).unwrap_or_default();
    Ok(Some(Action::Shell(format!("[{id}: {prog}] {what}"))))
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
                (expired || !term.proc.alive()).then(|| id.clone())
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
        buf: buf.clone(),
        input: input.clone(),
        last_used,
        alive,
        busy,
    };
    if let Ok(mut m) = meta_table().lock() {
        m.insert(id.clone(), meta.clone());
    }
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
    let text = collect(
        "terminal_open",
        &term,
        spec.expect,
        deadline,
        spec.auto_page,
        exec,
    );
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
    let command = str_arg(args, "command")?;
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
    let mut t = {
        let mut table = lock();
        let Some(t) = table.remove(id) else {
            return Err(ToolError::Failed(format!(
                "terminal {id} não existe (abertos: {})",
                open_list(&table)
            )));
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
    let text = collect(tool_name, &t, re, deadline, auto_page, exec);
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

fn send(args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    let re = expect(args)?;
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
    let (text, alive) = send_session_command(
        "terminal_send",
        id,
        args["input"].as_str(),
        secret,
        enter,
        re.as_ref(),
        timeout(args, 15, 600),
        auto_page,
        exec,
    )?;
    Ok(format!(
        "{id}{}\n{text}",
        if alive {
            ""
        } else {
            " — o programa encerrou; sessão fechada"
        }
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
fn collect(
    tool: &str,
    t: &Term,
    expect: Option<&Regex>,
    deadline: Instant,
    auto_page: bool,
    exec: &Exec<'_>,
) -> String {
    let len = || t.buf.lock().map(|b| b.len()).unwrap_or(0);
    let (mut last, mut changed) = (len(), Instant::now());
    let (mut progress_at, mut last_emit) = (0usize, Instant::now());
    let (mut pages, mut page_at) = (0usize, 0usize);
    let note = loop {
        std::thread::sleep(Duration::from_millis(12));
        let now = len();
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
        let tail_bytes: Vec<u8> = t
            .buf
            .lock()
            .map(|b| {
                let start = b.len().saturating_sub(LOOP_TAIL_BYTES);
                b.range(start..).copied().collect()
            })
            .unwrap_or_default();
        let tail_clean = clean(&tail_bytes);
        if auto_page && pages < MAX_AUTO_PAGES && now > page_at && looks_like_pager(&tail_clean) {
            if let Ok(mut w) = t.input.lock() {
                if w.write_all(b" ").and_then(|_| w.flush()).is_ok() {
                    pages += 1;
                    page_at = now;
                    changed = Instant::now();
                    continue;
                }
            }
        }
        let pager_waiting = auto_page && pages < MAX_AUTO_PAGES && looks_like_pager(&tail_clean);
        let prompt_settle = if tail_clean.contains('\n') {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(120)
        };
        let done = !pager_waiting
            && match expect {
                Some(re) => re.is_match(&tail_clean),
                None => {
                    now > 0
                        && (quiet >= Duration::from_secs(2)
                            || (quiet >= prompt_settle && looks_like_prompt(&tail_clean)))
                }
            };
        if done {
            break None;
        }
        if !t.proc.alive() || !t.meta.alive.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
            break Some("[o programa encerrou]".to_string());
        }
        if exec.cancel.is_cancelled() {
            break Some("[cancelado; a sessão continua aberta]".into());
        }
        if Instant::now() >= deadline {
            break Some(match expect {
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
            });
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
    match note {
        Some(n) if text.is_empty() => n,
        Some(n) => format!("{text}\n{n}"),
        None => text,
    }
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
        || tail.contains("-- [q quit")
}

/// The last line ends like a prompt: `Router#`, `<Huawei>`, `[admin@MikroTik] >`,
/// `$`, `Password:`, `(yes/no)?`.
fn looks_like_prompt(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim_end().ends_with(['>', '#', '$', '%', ']', ':', '?']))
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

/// Processes raw Telnet RFC 854 stream bytes, stripping IAC negotiation
/// sequences and generating immediate negotiation replies (including NAWS
/// 250x60 window size so router output never wraps at 80 columns).
pub(crate) fn strip_and_reply_telnet_iac(raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
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
                    break;
                }
                i += 3;
            }
            // IAC SB ... IAC SE
            0xFA => {
                i += 2;
                while i + 1 < raw.len() {
                    if raw[i] == 0xFF && raw[i + 1] == 0xF0 {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                i += 2;
            }
        }
    }
    (clean_out, replies)
}

struct TelnetReader {
    stream: std::net::TcpStream,
    alive: Arc<AtomicBool>,
}

impl Read for TelnetReader {
    fn read(&mut self, out_buf: &mut [u8]) -> std::io::Result<usize> {
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
            let (clean_bytes, reply) = strip_and_reply_telnet_iac(&raw[..n]);
            if !reply.is_empty() {
                let _ = self.stream.write_all(&reply);
                let _ = self.stream.flush();
            }
            if !clean_bytes.is_empty() {
                let copy_len = clean_bytes.len().min(out_buf.len());
                out_buf[..copy_len].copy_from_slice(&clean_bytes[..copy_len]);
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
        UpdateProcThreadAttribute, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
        LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        STARTF_USESTDHANDLES, STARTUPINFOEXW,
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
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
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
        CloseHandle(pi.hThread);
        Ok((
            Proc::Os(OsProc {
                hpc,
                process: pi.hProcess,
            }),
            Box::new(File::from_raw_handle(in_w as _)),
            Box::new(File::from_raw_handle(out_r as _)),
        ))
    }
}

#[cfg(not(windows))]
struct OsProc(std::process::Child);

#[cfg(not(windows))]
impl OsProc {
    fn alive(&self) -> bool {
        // try_wait needs &mut; a zero-signal kill answers "still there?".
        std::process::Command::new("kill")
            .args(["-0", &self.0.id().to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `script` gives the program a pseudo-terminal without a pty crate.
#[cfg(not(windows))]
fn spawn_os(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
    use std::process::{Command, Stdio};
    let mut c = Command::new("script");
    if cfg!(target_os = "macos") {
        c.args(["-q", "/dev/null", "sh", "-c", command]);
    } else {
        c.args(["-qfec", command, "/dev/null"]);
    }
    let mut child = c
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
    Ok((Proc::Os(OsProc(child)), Box::new(input), Box::new(output)))
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
