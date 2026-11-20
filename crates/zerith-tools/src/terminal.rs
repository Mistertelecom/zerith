//! Interactive terminal sessions: a program that keeps a prompt open (ssh,
//! telnet, a REPL, a router CLI) driven one input at a time.
//!
//! `shell` runs a command to completion with stdin closed, so anything that
//! asks a question (a password, `[y/n]`, a router's `>` prompt) hangs until
//! the timeout. Here the program runs under a pseudo-terminal (ConPTY on
//! Windows, `script` elsewhere), a thread buffers its output, and each call
//! sends one input and returns what came back since the last read.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

const MAX_OPEN: usize = 6;
// ponytail: a forgotten session dies on the next terminal call after 30 min
// idle; a reaper thread if sessions must die exactly on time.
const IDLE_REAP: Duration = Duration::from_secs(30 * 60);
const BUF_CAP: usize = 16 * 1024 * 1024;

struct Term {
    owner: Option<String>,
    command: String,
    input: Box<dyn Write + Send>,
    /// Output not yet handed to the model.
    buf: Arc<Mutex<Vec<u8>>>,
    proc: Proc,
    used: Instant,
    /// Vault values typed here: scrubbed from every read.
    secrets: Vec<String>,
}

impl Drop for Term {
    fn drop(&mut self) {
        self.proc.kill();
    }
}

fn table() -> &'static Mutex<HashMap<String, Term>> {
    static T: OnceLock<Mutex<HashMap<String, Term>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Term>> {
    table().lock().unwrap_or_else(|e| e.into_inner())
}

/// The program a terminal session runs, for the approval rules (a session
/// on `ssh` is remote access; every input to it is approved on its own).
pub fn terminal_program(id: &str) -> Option<String> {
    lock().get(id).map(|t| t.command.clone())
}

fn open_list(t: &HashMap<String, Term>) -> String {
    let v: Vec<String> = t
        .iter()
        .map(|(id, s)| format!("{id} ({})", s.command))
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
    lock().retain(|_, t| t.used.elapsed() < IDLE_REAP && t.proc.alive());
}

fn open(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let command = str_arg(args, "command")?;
    let re = expect(args)?;
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
    let (proc, input, mut out) = spawn(command, ctx.workspace)
        .map_err(|e| ToolError::Failed(format!("não abriu `{command}`: {e}")))?;
    let buf = Arc::new(Mutex::new(Vec::new()));
    let fill = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = out.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let mut v = fill.lock().unwrap_or_else(|e| e.into_inner());
            if v.len() < BUF_CAP {
                v.extend_from_slice(&chunk[..n]);
            }
        }
    });
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = format!("term-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let term = Term {
        owner: exec.host.session(),
        command: command.to_string(),
        input,
        buf,
        proc,
        used: Instant::now(),
        secrets: Vec::new(),
    };
    let deadline = exec.deadline.min(Instant::now() + timeout(args, 20, 120));
    let text = collect(&term, re.as_ref(), deadline, exec);
    let alive = term.proc.alive();
    if alive {
        lock().insert(id.clone(), term);
    }
    Ok(format!(
        "{id} ({command}){}\n{text}",
        if alive {
            ""
        } else {
            " — o programa já encerrou"
        }
    ))
}

fn send(args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let id = str_arg(args, "id")?;
    let re = expect(args)?;
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
    let mut data = String::new();
    if let Some(v) = &secret {
        data.push_str(v);
        t.secrets.push(v.clone());
    } else if let Some(i) = args["input"].as_str() {
        data.push_str(i);
    }
    let typed = secret.is_some() || args["input"].is_string();
    if typed && args["enter"].as_bool().unwrap_or(true) {
        data.push('\r');
    }
    if !data.is_empty() {
        if let Err(e) = t
            .input
            .write_all(data.as_bytes())
            .and_then(|_| t.input.flush())
        {
            return Err(ToolError::Failed(format!("terminal {id} fechado: {e}")));
        }
    }
    let deadline = exec.deadline.min(Instant::now() + timeout(args, 15, 600));
    let text = collect(&t, re.as_ref(), deadline, exec);
    t.used = Instant::now();
    let alive = t.proc.alive();
    if alive {
        lock().insert(id.to_string(), t);
    }
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
    match lock().remove(id) {
        Some(_) => Ok(format!("{id} fechado")),
        None => Err(ToolError::Failed(format!("terminal {id} não existe"))),
    }
}

/// Waits for the program to settle, then hands over (and drops) everything
/// unread. Settled: the `expect` pattern showed up, or the output went quiet
/// on what looks like a prompt, or quiet for 2 s.
fn collect(t: &Term, expect: Option<&Regex>, deadline: Instant, exec: &Exec<'_>) -> String {
    let len = || t.buf.lock().map(|b| b.len()).unwrap_or(0);
    let (mut last, mut changed) = (len(), Instant::now());
    let note = loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = len();
        if now != last {
            (last, changed) = (now, Instant::now());
        }
        let quiet = changed.elapsed();
        let text = clean(&t.buf.lock().map(|b| b.clone()).unwrap_or_default());
        let done = match expect {
            Some(re) => re.is_match(&text),
            None => {
                now > 0
                    && (quiet >= Duration::from_secs(2)
                        || (quiet >= Duration::from_millis(400) && looks_like_prompt(&text)))
            }
        };
        if done {
            break None;
        }
        if !t.proc.alive() {
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
    let raw = std::mem::take(&mut *t.buf.lock().unwrap_or_else(|e| e.into_inner()));
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

#[cfg(windows)]
struct Proc {
    hpc: isize,
    process: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: the handles are plain kernel handles, used from one thread at a time
// (a session is taken out of the table while a call works on it).
#[cfg(windows)]
unsafe impl Send for Proc {}

#[cfg(windows)]
impl Proc {
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
fn spawn(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
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
            Proc {
                hpc,
                process: pi.hProcess,
            },
            Box::new(File::from_raw_handle(in_w as _)),
            Box::new(File::from_raw_handle(out_r as _)),
        ))
    }
}

#[cfg(not(windows))]
struct Proc(std::process::Child);

#[cfg(not(windows))]
impl Proc {
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
fn spawn(command: &str, cwd: &std::path::Path) -> std::io::Result<Spawned> {
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
    Ok((Proc(child), Box::new(input), Box::new(output)))
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
