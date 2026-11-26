//! `execute_code`: the model writes a Python script that calls Zerith's own
//! tools in a loop (the same `show` on 50 routers, filter a huge output) and
//! only what the script prints comes back. One model turn instead of 50.
//!
//! Concept and limits after Hermes Agent's `execute_code` (MIT): a script
//! talks to the host over a loopback socket guarded by a random token, each
//! call it makes goes through the host's full tool path, 5-minute deadline,
//! 64 tool calls, 50 KB of stdout. No code copied.
//!
//! Every call the script makes enters [`Pipeline`] exactly like a model call:
//! middleware and hooks, permission rules, the approval prompt, then output
//! redaction. A script is never a way around the user's rules.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_providers::{Cancel, ToolCall};

use crate::pipeline::Pipeline;
use crate::RunEvent;

/// Tool calls one script may make.
pub const MAX_CALLS: usize = 64;
/// Longest a script may run.
pub const TIMEOUT: Duration = Duration::from_secs(300);
/// Bytes of stdout kept (head and tail).
pub const MAX_STDOUT: usize = 50_000;

/// Tools a script may call. The rest (agent, ask_user, monitor, the script
/// tool itself) need a person or a turn of their own.
const ALLOWED: &[&str] = &[
    "read_file",
    "list_dir",
    "glob",
    "grep",
    "write_file",
    "edit_file",
    "multi_edit",
    "apply_patch",
    "repo_map",
    "diagnostics",
    "lsp",
    "shell",
    "web_fetch",
    "web_search",
    "net_inventory",
    "net_probe",
    "net_path",
    "net_diff",
    "net_connect",
    "net_vpn",
    "net_backup",
    "net_broadcast",
    "net_transfer",
    "net_snmp",
    "net_api",
    "net_zabbix",
    "terminal_open",
    "terminal_send",
    "terminal_batch",
    "terminal_close",
];

/// The Python module the script imports as `zerith`: one function per
/// allowed tool, each a JSON request over the socket.
fn stub(port: u16, token: &str) -> String {
    let mut funcs = String::new();
    for t in ALLOWED {
        funcs.push_str(&format!(
            "def {t}(**kw):\n    \"\"\"Ferramenta `{t}` do Zerith; mesmos argumentos da chamada normal.\"\"\"\n    return _call({t:?}, kw)\n\n"
        ));
    }
    format!(
        r#"# Gerado pelo Zerith para execute_code. Cada função chama a ferramenta
# de mesmo nome pelo caminho normal (regras, aprovação, segredos mascarados).
import json, socket

_ADDR = ("127.0.0.1", {port})
_TOKEN = {token:?}

class ToolError(Exception):
    pass

def _call(tool, args):
    s = socket.create_connection(_ADDR)
    try:
        s.sendall((json.dumps({{"token": _TOKEN, "tool": tool, "args": args}}) + "\n").encode())
        data = b""
        while not data.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
    finally:
        s.close()
    r = json.loads(data.decode() or "{{}}")
    if not r.get("ok"):
        raise ToolError(r.get("output", "erro desconhecido"))
    return r.get("output", "")

def json_parse(text):
    """json.loads tolerante: aceita um objeto JSON no meio de outro texto."""
    try:
        return json.loads(text)
    except ValueError:
        start = min([i for i in (text.find("{{"), text.find("[")) if i >= 0] or [-1])
        if start < 0:
            raise
        return json.loads(text[start:])

{funcs}"#
    )
}

/// What one script run reports.
pub struct Outcome {
    pub ok: bool,
    pub output: String,
}

/// A request from the script, handed to the turn thread with a reply slot.
struct Request {
    tool: String,
    args: Value,
    reply: mpsc::Sender<(bool, String)>,
}

/// A random hex token from the OS RNG (`getrandom` via std's hasher seed is
/// not a CSPRNG, so read the OS source directly).
fn token() -> String {
    let mut b = [0u8; 16];
    #[cfg(unix)]
    {
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut b);
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "advapi32")]
        extern "system" {
            #[link_name = "SystemFunction036"]
            fn rtl_gen_random(buf: *mut u8, len: u32) -> u8;
        }
        // SAFETY: `b` is a valid, writable 16-byte buffer for the whole call.
        unsafe {
            rtl_gen_random(b.as_mut_ptr(), b.len() as u32);
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn system_python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

/// Prefers a project-local `.venv` interpreter when present, falling back to
/// the system `python` / `python3`.
fn resolve_python(workspace: &Path) -> PathBuf {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[&[".venv", "Scripts", "python.exe"]]
    } else {
        &[&[".venv", "bin", "python"], &[".venv", "bin", "python3"]]
    };
    for parts in candidates {
        let mut p = workspace.to_path_buf();
        for seg in *parts {
            p.push(seg);
        }
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(system_python())
}

/// Head and tail of `s` within [`MAX_STDOUT`] bytes, on char boundaries.
fn clip(s: &str) -> String {
    if s.len() <= MAX_STDOUT {
        return s.to_string();
    }
    let mut head = MAX_STDOUT * 2 / 5;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - (MAX_STDOUT - head);
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[... {} bytes omitidos: imprima menos ou filtre no script ...]\n{}",
        &s[..head],
        tail - head,
        &s[tail..]
    )
}

/// Runs `code` with the `zerith` module on the path and serves its tool
/// calls through `pipe` on this thread (approvals and middleware need it).
pub(crate) fn run(
    pipe: &Pipeline<'_>,
    parent_call_id: &str,
    workspace: &Path,
    code: &str,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
) -> Outcome {
    let fail = |msg: String| Outcome {
        ok: false,
        output: msg,
    };
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => return fail(format!("erro: não abriu o socket local: {e}")),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let tok = token();
    let dir = match tempfile_dir() {
        Ok(d) => d,
        Err(e) => return fail(format!("erro: pasta temporária: {e}")),
    };
    let module = dir.join("zerith.py");
    let script = dir.join("script.py");
    let boot = dir.join("boot.py");
    // -I (isolated: no user site, no PYTHON* env, no cwd on sys.path) also
    // drops the script's own folder, so the loader puts only the module's
    // folder first and runs the script as `__main__`.
    // The tool functions come preloaded (a forgotten `from zerith import`
    // cost a whole retry in the live test), and stdout is UTF-8 even where
    // the console code page is cp1252 (an accent or emoji crashed `print`).
    let loader = "import runpy, sys, os\nsys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))\nsys.stdout.reconfigure(encoding='utf-8', errors='replace')\nsys.stderr.reconfigure(encoding='utf-8', errors='replace')\nimport zerith\nrunpy.run_path(os.path.join(sys.path[0], 'script.py'), init_globals={k: v for k, v in vars(zerith).items() if not k.startswith('_')}, run_name='__main__')\n";
    if let Err(e) = std::fs::write(&module, stub(port, &tok))
        .and_then(|_| std::fs::write(&script, code))
        .and_then(|_| std::fs::write(&boot, loader))
    {
        return fail(format!("erro: não gravou o script: {e}"));
    }

    let py = resolve_python(workspace);
    let jail = zerith_security::jail::Jail::for_policy(pipe.policy());
    let mut cmd = Command::new(&py);
    cmd.arg("-I").arg(&boot).current_dir(workspace);
    let mut cmd = jail.wrap(cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match jail.spawn_wrapped(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return fail(format!(
                "erro: não iniciou o Python ({}): {e}",
                py.display()
            ));
        }
    };

    let (tx, rx) = mpsc::channel::<Request>();
    listener.set_nonblocking(true).ok();
    let calls = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + TIMEOUT;
    let out_pipe = child.child.stdout.take();
    let err_pipe = child.child.stderr.take();

    let (status, stdout, stderr, served) = std::thread::scope(|s| {
        let out_h = s.spawn(move || read_all(out_pipe));
        let err_h = s.spawn(move || read_all(err_pipe));
        let tok = tok.as_str();
        let stop = &stop;
        let accept = s.spawn(move || accept_loop(&listener, tok, tx, deadline, stop));
        let mut served = 0usize;
        let status = loop {
            if cancel.is_cancelled() || Instant::now() >= deadline {
                child.kill_tree();
                break Err(if cancel.is_cancelled() {
                    "cancelado".to_string()
                } else {
                    format!("tempo esgotado ({}s)", TIMEOUT.as_secs())
                });
            }
            if let Ok(first) = rx.recv_timeout(Duration::from_millis(50)) {
                let mut queued = vec![first];
                while queued.len() < MAX_CALLS {
                    match rx.try_recv() {
                        Ok(req) => queued.push(req),
                        Err(_) => break,
                    }
                }
                let mut valid: Vec<(ToolCall, mpsc::Sender<(bool, String)>)> = Vec::new();
                for req in queued {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if n > MAX_CALLS {
                        let _ = req.reply.send((
                            false,
                            format!("limite de {MAX_CALLS} chamadas por script atingido"),
                        ));
                    } else if !ALLOWED.contains(&req.tool.as_str()) {
                        let _ = req.reply.send((
                            false,
                            format!("`{}` não pode ser chamada de dentro de um script", req.tool),
                        ));
                    } else {
                        served += 1;
                        valid.push((
                            ToolCall {
                                id: format!("exec-{n}"),
                                name: req.tool,
                                arguments: req.args.to_string(),
                            },
                            req.reply,
                        ));
                    }
                }
                serve_batch(pipe, parent_call_id, valid, on_event);
            }
            if let Ok(Some(st)) = child.child.try_wait() {
                break Ok(st);
            }
        };
        // Drop `rx` before joining `accept` so any in-flight connection handler
        // waiting on `rrx.recv()` unblocks cleanly instead of stalling shutdown.
        drop(rx);
        stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", port));
        let _ = accept.join();
        let stdout = out_h.join().unwrap_or_default();
        let stderr = err_h.join().unwrap_or_default();
        (status, stdout, stderr, served)
    });
    let _ = std::fs::remove_dir_all(&dir);

    let mut text = clip(&stdout);
    let tail = |s: &str| {
        let s = s.trim();
        let start = s.len().saturating_sub(4000);
        let mut i = start;
        while !s.is_char_boundary(i) {
            i += 1;
        }
        s[i..].to_string()
    };
    let ok = match &status {
        Ok(st) if st.success() => true,
        Ok(st) => {
            text.push_str(&format!(
                "\n[script terminou com erro ({})]\n{}",
                st.code().map_or("sinal".into(), |c| format!("exit {c}")),
                tail(&stderr)
            ));
            false
        }
        Err(why) => {
            text.push_str(&format!(
                "\n[script interrompido: {why}]\n{}",
                tail(&stderr)
            ));
            false
        }
    };
    text.push_str(&format!("\n[{served} chamada(s) de ferramenta]"));
    Outcome {
        ok,
        output: text.trim_start().to_string(),
    }
}

/// Dispatches one or more queued script tool calls through [`Pipeline`],
/// running consecutive `concurrency_safe` / disjoint-target calls concurrently
/// via [`Pipeline::run_group`] while keeping `prepare`, `finish`, and
/// `on_event` safely on the turn thread.
fn serve_batch(
    pipe: &Pipeline<'_>,
    parent_call_id: &str,
    batch: Vec<(ToolCall, mpsc::Sender<(bool, String)>)>,
    on_event: &mut dyn FnMut(RunEvent),
) {
    if batch.is_empty() {
        return;
    }
    let parent_id = (!parent_call_id.is_empty()).then(|| parent_call_id.to_string());
    let mut offset = 0usize;
    while offset < batch.len() {
        let calls_slice: Vec<ToolCall> = batch[offset..].iter().map(|(c, _)| c.clone()).collect();
        let chunk_len = pipe.plan_batch(&calls_slice).max(1).min(calls_slice.len());
        let chunk_ids: Vec<String> = calls_slice[..chunk_len]
            .iter()
            .map(|c| c.id.clone())
            .collect();

        for call in &calls_slice[..chunk_len] {
            on_event(RunEvent::ToolProgress {
                tool_call_id: parent_id.clone(),
                tool: "execute_code".into(),
                chunk: format!("→ {}\n", call.name),
            });
        }

        let mut forward = |mut ev: RunEvent| {
            if let RunEvent::ToolProgress {
                ref mut tool_call_id,
                ..
            } = ev
            {
                if parent_id.is_some()
                    && (tool_call_id.is_none()
                        || tool_call_id
                            .as_deref()
                            .is_some_and(|id| chunk_ids.iter().any(|c| c == id)))
                {
                    *tool_call_id = parent_id.clone();
                }
            }
            on_event(ev);
        };

        let mut prepared = Vec::with_capacity(chunk_len);
        for call in &calls_slice[..chunk_len] {
            prepared.push(pipe.prepare(call, &mut forward));
        }
        let ran = pipe.run_group(&prepared, chunk_len.min(16), &mut forward);
        for (idx, p) in prepared.into_iter().enumerate() {
            let ran_one = ran.get(idx).and_then(|r| r.clone());
            let (outcome, _) = pipe.finish(p, ran_one);
            // The script parses the page or the search JSON: it gets the bare text.
            // ponytail: what the script prints back reaches the model unmarked; mark
            // execute_code's output when a script read the web, if that shows up.
            let output = crate::pipeline::strip_external(&outcome.output).to_string();
            let _ = batch[offset + idx].1.send((outcome.ok, output));
        }
        offset += chunk_len;
    }
}

fn handle_connection(mut stream: TcpStream, token: &str, tx: &mpsc::Sender<Request>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut line = String::new();
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        // The wake-up connection from `run`, or junk.
        return;
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    // Constant-time token check: another local process must not drive
    // the session's tools.
    let given = req["token"].as_str().unwrap_or("").as_bytes();
    let ok_token = given.len() == token.len()
        && given
            .iter()
            .zip(token.as_bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !ok_token {
        let _ = stream.write_all("{\"ok\":false,\"output\":\"token inválido\"}\n".as_bytes());
        return;
    }
    let (rtx, rrx) = mpsc::channel();
    let sent = tx.send(Request {
        tool: req["tool"].as_str().unwrap_or("").to_string(),
        args: req.get("args").cloned().unwrap_or(json!({})),
        reply: rtx,
    });
    if sent.is_err() {
        return;
    }
    // The turn thread may wait on the person (approval): no read timeout.
    let (ok, output) = rrx.recv().unwrap_or((false, "cancelado".into()));
    let body = json!({"ok": ok, "output": output}).to_string();
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.write_all(b"\n");
}

fn accept_loop(
    listener: &TcpListener,
    token: &str,
    tx: mpsc::Sender<Request>,
    deadline: Instant,
    stop: &AtomicBool,
) {
    std::thread::scope(|s| {
        while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
            let stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => break,
            };
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let tx = tx.clone();
            s.spawn(move || handle_connection(stream, token, &tx));
        }
    });
}

fn read_all(r: Option<impl Read>) -> String {
    let mut buf = Vec::new();
    if let Some(mut r) = r {
        let _ = r.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A private temp dir for the script and its module.
fn tempfile_dir() -> std::io::Result<std::path::PathBuf> {
    let d = std::env::temp_dir().join(format!("zerith-exec-{}", token()));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Another local process that finds the port cannot drive the tools:
    /// without the token the request never reaches the turn.
    #[test]
    fn a_request_without_the_token_is_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                accept_loop(
                    &listener,
                    "certo",
                    tx,
                    Instant::now() + Duration::from_secs(5),
                    &stop,
                )
            });
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.write_all(
                b"{\"token\":\"errado\",\"tool\":\"shell\",\"args\":{\"command\":\"x\"}}\n",
            )
            .unwrap();
            let mut reply = String::new();
            BufReader::new(c).read_line(&mut reply).unwrap();
            assert!(reply.contains("token inv"), "{reply}");
            assert!(rx.try_recv().is_err(), "nothing reached the turn");
            stop.store(true, Ordering::SeqCst);
        });
    }

    #[test]
    fn long_output_keeps_head_and_tail() {
        let s = format!("INICIO{}FIM", "x".repeat(MAX_STDOUT * 2));
        let c = clip(&s);
        assert!(c.starts_with("INICIO") && c.ends_with("FIM"));
        assert!(c.len() < MAX_STDOUT + 200);
        assert_eq!(token().len(), 32);
        assert_ne!(token(), token());
    }

    #[test]
    fn allowed_tools_include_wave_d_and_code_tools_with_64_call_cap() {
        assert_eq!(MAX_CALLS, 64);
        let expected = [
            "net_connect",
            "net_vpn",
            "net_backup",
            "net_broadcast",
            "net_transfer",
            "net_snmp",
            "net_api",
            "net_zabbix",
            "multi_edit",
            "apply_patch",
            "repo_map",
            "diagnostics",
            "lsp",
        ];
        let generated = stub(12345, "tok");
        for tool in expected {
            assert!(ALLOWED.contains(&tool), "missing {tool} in ALLOWED");
            assert!(
                generated.contains(&format!("def {tool}(**kw):")),
                "missing stub for {tool}"
            );
        }
        assert!(!ALLOWED.contains(&"agent"));
        assert!(!ALLOWED.contains(&"execute_code"));
    }

    #[test]
    fn resolve_python_prefers_workspace_venv_and_falls_back_to_system() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_python(dir.path()), PathBuf::from(system_python()));

        let venv_py = if cfg!(windows) {
            dir.path().join(".venv").join("Scripts").join("python.exe")
        } else {
            dir.path().join(".venv").join("bin").join("python")
        };
        std::fs::create_dir_all(venv_py.parent().unwrap()).unwrap();
        std::fs::write(&venv_py, b"").unwrap();
        assert_eq!(resolve_python(dir.path()), venv_py);
    }

    #[test]
    fn accept_loop_handles_concurrent_requests_without_blocking() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                accept_loop(
                    &listener,
                    "certo",
                    tx,
                    Instant::now() + Duration::from_secs(5),
                    &stop,
                )
            });

            let mut c1 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let mut c2 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c1.write_all(
                b"{\"token\":\"certo\",\"tool\":\"net_snmp\",\"args\":{\"host\":\"r1\"}}\n",
            )
            .unwrap();
            c2.write_all(
                b"{\"token\":\"certo\",\"tool\":\"net_snmp\",\"args\":{\"host\":\"r2\"}}\n",
            )
            .unwrap();

            // Both requests must arrive before we answer either one.
            let r1 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let r2 = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let _ = r1
                .reply
                .send((true, format!("ok:{}", r1.args["host"].as_str().unwrap())));
            let _ = r2
                .reply
                .send((true, format!("ok:{}", r2.args["host"].as_str().unwrap())));

            let mut s1 = String::new();
            let mut s2 = String::new();
            BufReader::new(c1).read_line(&mut s1).unwrap();
            BufReader::new(c2).read_line(&mut s2).unwrap();
            assert!(s1.contains("ok:r1"), "{s1}");
            assert!(s2.contains("ok:r2"), "{s2}");

            stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", port));
        });
    }
}
