//! Background shell: `shell{run_in_background:true}` starts a job and returns
//! its id; `shell_output` reads what it printed since the last look;
//! `shell_kill` stops it. Jobs belong to the registry's [`crate::Host`] and
//! die with it, so a closed session leaves no stray process.
//!
//! ponytail: output is kept in memory (capped at 1 MB, oldest dropped); a job
//! that prints gigabytes loses its head, not the process. Spill to a file if
//! that ever matters.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};

use crate::builtin::{kill_tree, shell_command};
use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

/// Jobs alive at once, per registry.
pub const MAX_JOBS: usize = 4;
/// Bytes of output kept per job.
const MAX_OUTPUT: usize = 1 << 20;

static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Output {
    text: String,
    /// Bytes of `text` already handed to the model.
    seen: usize,
    truncated: bool,
}

struct Job {
    child: Child,
    out: Arc<Mutex<Output>>,
    readers: Vec<JoinHandle<()>>,
    command: String,
    exit: Option<String>,
}

#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<Vec<(String, Job)>>,
}

fn poisoned() -> ToolError {
    ToolError::Failed("registro de jobs corrompido".into())
}

impl Jobs {
    pub fn start(&self, command: &str, cwd: &Path) -> Result<String> {
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        for (_, j) in jobs.iter_mut() {
            refresh(j);
        }
        let alive = jobs.iter().filter(|(_, j)| j.exit.is_none()).count();
        if alive >= MAX_JOBS {
            return Err(ToolError::Failed(format!(
                "já há {MAX_JOBS} jobs em segundo plano; encerre um com shell_kill"
            )));
        }
        // Finished jobs whose output was read can go; keep the table small.
        jobs.retain(|(_, j)| j.exit.is_none() || j.out.lock().is_ok_and(|o| o.seen < o.text.len()));
        let mut cmd = shell_command(command);
        cmd.current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn()?;
        let out = Arc::new(Mutex::new(Output::default()));
        let readers = vec![
            pump(child.stdout.take().map(|s| Box::new(s) as _), &out),
            pump(child.stderr.take().map(|s| Box::new(s) as _), &out),
        ];
        let id = format!("job-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        jobs.push((
            id.clone(),
            Job {
                child,
                out,
                readers,
                command: command.to_string(),
                exit: None,
            },
        ));
        Ok(id)
    }

    /// New output since the last call, plus the state.
    pub fn read(&self, id: &str) -> Result<String> {
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let j = find(&mut jobs, id)?;
        refresh(j);
        let mut o = j.out.lock().map_err(|_| poisoned())?;
        let mut cut = o.seen.min(o.text.len());
        while !o.text.is_char_boundary(cut) {
            cut += 1;
        }
        let fresh = o.text[cut..].to_string();
        o.seen = o.text.len();
        let mut text = match &j.exit {
            Some(code) => format!("{id}: terminou ({code})\n"),
            None => format!("{id}: em execução\n"),
        };
        if fresh.is_empty() {
            text.push_str("[sem saída nova]\n");
        } else {
            text.push_str(&fresh);
            if !fresh.ends_with('\n') {
                text.push('\n');
            }
        }
        if o.truncated {
            text.push_str("[saída antiga descartada: limite de 1 MB]\n");
        }
        Ok(text)
    }

    pub fn kill(&self, id: &str) -> Result<String> {
        let mut jobs = self.jobs.lock().map_err(|_| poisoned())?;
        let j = find(&mut jobs, id)?;
        if j.exit.is_some() {
            return Ok(format!("{id} já tinha terminado"));
        }
        kill_tree(&mut j.child);
        for r in j.readers.drain(..) {
            let _ = r.join();
        }
        j.exit = Some("encerrado".into());
        Ok(format!("{id} encerrado: {}", j.command))
    }
}

fn find<'a>(jobs: &'a mut [(String, Job)], id: &str) -> Result<&'a mut Job> {
    jobs.iter_mut()
        .find(|(i, _)| i == id)
        .map(|(_, j)| j)
        .ok_or_else(|| ToolError::Failed(format!("job desconhecido: {id}")))
}

impl Drop for Jobs {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.jobs.lock() {
            for (_, j) in jobs.iter_mut() {
                if j.exit.is_none() {
                    kill_tree(&mut j.child);
                }
            }
        }
    }
}

/// Notes the exit once; the readers have hit EOF by then and are joined so the
/// output is complete before it is reported.
fn refresh(j: &mut Job) {
    if j.exit.is_some() {
        return;
    }
    if let Ok(Some(status)) = j.child.try_wait() {
        for r in j.readers.drain(..) {
            let _ = r.join();
        }
        j.exit = Some(match status.code() {
            Some(c) => format!("exit {c}"),
            None => "sinal".into(),
        });
    }
}

fn pump(r: Option<Box<dyn Read + Send>>, out: &Arc<Mutex<Output>>) -> JoinHandle<()> {
    let out = out.clone();
    std::thread::spawn(move || {
        let Some(r) = r else { return };
        let mut r = BufReader::new(r);
        let mut line = Vec::new();
        loop {
            line.clear();
            match r.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let s = String::from_utf8_lossy(&line);
            let Ok(mut o) = out.lock() else { return };
            o.text.push_str(&s);
            if o.text.len() > MAX_OUTPUT {
                let mut cut = o.text.len() - MAX_OUTPUT;
                while !o.text.is_char_boundary(cut) {
                    cut += 1;
                }
                o.text.drain(..cut);
                o.seen = o.seen.saturating_sub(cut);
                o.truncated = true;
            }
        }
    })
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make(
            "shell_output",
            "Lê a saída nova de um job iniciado com shell run_in_background e diz se ele terminou.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "estado do job e as linhas novas desde a última leitura",
            Permission::Read,
            10,
            false,
            |a, _, e| e.host.jobs().read(str_arg(a, "id")?),
        ),
        make(
            "shell_kill",
            "Encerra um job em segundo plano e os processos que ele iniciou.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
            "confirmação",
            Permission::Shell,
            10,
            false,
            |a, _, e| e.host.jobs().kill(str_arg(a, "id")?),
        ),
    ]
}

/// Called by `shell` itself for `run_in_background`.
pub(crate) fn start(args: &Value, cwd: &Path, host: &crate::Host) -> Result<String> {
    let id = host.jobs().start(str_arg(args, "command")?, cwd)?;
    Ok(format!(
        "{id} iniciado em segundo plano; use shell_output para ler a saída e shell_kill para encerrar"
    ))
}
