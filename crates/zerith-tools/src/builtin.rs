//! The built-in tools. Each is a struct holding its spec and a plain `run`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::{glob_match, Action};

use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

type RunFn = fn(&Value, &CallCtx<'_>, &Exec<'_>) -> Result<String>;
type ActionFn = fn(&Value, &CallCtx<'_>) -> Result<Option<Action>>;

/// A built-in is data plus two functions; one struct covers them all.
struct Builtin {
    spec: ToolSpec,
    action: ActionFn,
    run: RunFn,
}

impl Tool for Builtin {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        (self.action)(args, ctx)
    }
    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        (self.run)(args, ctx, exec)
    }
}

#[allow(clippy::too_many_arguments)]
fn tool(
    name: &str,
    description: &str,
    parameters: Value,
    returns: &'static str,
    permission: Permission,
    timeout_s: u64,
    action: ActionFn,
    run: RunFn,
) -> Box<dyn crate::Tool> {
    Box::new(Builtin {
        spec: ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters,
            returns,
            permission,
            timeout: Duration::from_secs(timeout_s),
            cancellable: true,
            local: true,
            source: "built-in".into(),
        },
        action,
        run,
    })
}

pub(crate) fn all() -> Vec<Box<dyn crate::Tool>> {
    vec![
        tool(
            "read_file",
            "Lê um arquivo de texto. Devolve linhas numeradas; use offset/limit para arquivos grandes.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "primeira linha (1 = início)"},
                "limit": {"type": "integer", "description": "máximo de linhas (padrão 2000)"}
            }, "required": ["path"]}),
            "linhas no formato \"N\\ttexto\"",
            Permission::Read,
            10,
            read_action,
            read_file,
        ),
        tool(
            "write_file",
            "Cria ou sobrescreve um arquivo com o conteúdo dado.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"}, "content": {"type": "string"}
            }, "required": ["path", "content"]}),
            "o número de bytes escritos",
            Permission::Write,
            10,
            write_action,
            write_file,
        ),
        tool(
            "edit_file",
            "Substitui um trecho exato de um arquivo. old_string precisa ser único, a menos que replace_all seja true.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            }, "required": ["path", "old_string", "new_string"]}),
            "quantas ocorrências foram substituídas",
            Permission::Write,
            10,
            write_action,
            edit_file,
        ),
        tool(
            "list_dir",
            "Lista as entradas de um diretório; diretórios terminam com /.",
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            "um nome por linha; diretórios terminam com /",
            Permission::Read,
            10,
            read_action,
            list_dir,
        ),
        tool(
            "glob",
            "Encontra arquivos por padrão (ex.: **/*.rs) a partir de um diretório.",
            json!({"type": "object", "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "diretório base (padrão: workspace)"}
            }, "required": ["pattern"]}),
            "caminhos relativos, um por linha",
            Permission::Read,
            30,
            dir_action,
            glob,
        ),
        tool(
            "grep",
            "Procura texto (substring, sem diferenciar maiúsculas) em arquivos.",
            json!({"type": "object", "properties": {
                "query": {"type": "string"},
                "path": {"type": "string", "description": "diretório ou arquivo (padrão: workspace)"},
                "glob": {"type": "string", "description": "filtra arquivos, ex.: *.rs"}
            }, "required": ["query"]}),
            "linhas \"caminho:N: texto\"",
            Permission::Read,
            30,
            dir_action,
            grep,
        ),
        tool(
            "shell",
            "Executa um comando no shell do sistema (cmd no Windows, sh nos demais) dentro do workspace.",
            json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout_secs": {"type": "integer", "description": "padrão 120, máximo 600"}
            }, "required": ["command"]}),
            "código de saída, stdout e stderr",
            Permission::Shell,
            600,
            shell_action,
            shell,
        ),
        tool(
            "memory_recall",
            "Consulta fatos duráveis da memória do usuário.",
            json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
            "linhas \"[confirmed|inferred] texto\"",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_recall,
        ),
    ]
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| ToolError::BadArguments(format!("{key} precisa ser uma string")))
}

fn read_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    Ok(Some(Action::Read(ctx.resolve(str_arg(args, "path")?))))
}

fn dir_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    let p = args["path"].as_str().unwrap_or(".");
    Ok(Some(Action::Read(ctx.resolve(p))))
}

fn write_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    Ok(Some(Action::Write(ctx.resolve(str_arg(args, "path")?))))
}

fn shell_action(args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
    Ok(Some(Action::Shell(str_arg(args, "command")?.to_string())))
}

fn read_file(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let text = std::fs::read_to_string(&path)?;
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().unwrap_or(2000) as usize;
    let total = text.lines().count();
    let mut out: String = text
        .lines()
        .enumerate()
        .skip(offset - 1)
        .take(limit)
        .map(|(i, l)| format!("{}\t{l}\n", i + 1))
        .collect();
    let last = (offset - 1 + limit).min(total);
    if last < total {
        out.push_str(&format!(
            "[linhas {offset}-{last} de {total}; use offset para continuar]\n"
        ));
    }
    if total == 0 {
        out.push_str("[arquivo vazio]\n");
    }
    Ok(out)
}

fn write_file(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(format!(
        "{} bytes escritos em {}",
        content.len(),
        path.display()
    ))
}

fn edit_file(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let old = str_arg(args, "old_string")?;
    let new = str_arg(args, "new_string")?;
    let all = args["replace_all"].as_bool().unwrap_or(false);
    if old.is_empty() {
        return Err(ToolError::BadArguments("old_string está vazio".into()));
    }
    if old == new {
        return Err(ToolError::BadArguments(
            "old_string e new_string são iguais".into(),
        ));
    }
    let text = std::fs::read_to_string(&path)?;
    let n = text.matches(old).count();
    match (n, all) {
        (0, _) => Err(ToolError::Failed(format!(
            "old_string não encontrado em {}",
            path.display()
        ))),
        (n, false) if n > 1 => Err(ToolError::Failed(format!(
            "old_string aparece {n} vezes em {}; inclua mais contexto ou use replace_all",
            path.display()
        ))),
        _ => {
            std::fs::write(&path, text.replace(old, new))?;
            Ok(format!(
                "{n} ocorrência(s) substituída(s) em {}",
                path.display()
            ))
        }
    }
}

fn list_dir(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(ctx.resolve(str_arg(args, "path")?))? {
        let entry = entry?;
        let mut n = entry.file_name().to_string_lossy().to_string();
        if entry.file_type()?.is_dir() {
            n.push('/');
        }
        names.push(n);
    }
    names.sort();
    Ok(names.join("\n"))
}

/// Directories nobody wants searched: build output and VCS internals.
fn skipped(e: &walkdir::DirEntry) -> bool {
    e.depth() > 0
        && e.file_type().is_dir()
        && matches!(
            e.file_name().to_str(),
            Some(".git" | "target" | "node_modules")
        )
}

/// Walks files under `base`, yielding (relative path with `/`, entry).
fn walk<'a>(
    base: &'a Path,
    exec: &'a Exec<'_>,
) -> impl Iterator<Item = Result<(String, walkdir::DirEntry)>> + 'a {
    walkdir::WalkDir::new(base)
        .into_iter()
        .filter_entry(|e| !skipped(e))
        .filter_map(move |e| {
            if let Err(err) = exec.check() {
                return Some(Err(err));
            }
            let e = e.ok()?;
            if !e.file_type().is_file() {
                return None;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            Some(Ok((rel, e)))
        })
}

const MAX_MATCHES: usize = 500;

fn glob(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let pattern = str_arg(args, "pattern")?.replace('\\', "/");
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let mut hits = Vec::new();
    for item in walk(&base, exec) {
        let (rel, _) = item?;
        if glob_match(&pattern, &rel, true) {
            hits.push(rel);
            if hits.len() >= MAX_MATCHES {
                hits.push(format!("[parado em {MAX_MATCHES} resultados]"));
                break;
            }
        }
    }
    if hits.is_empty() {
        return Ok("nenhum arquivo encontrado".into());
    }
    Ok(hits.join("\n"))
}

fn grep(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    // ponytail: substring match only; `regex` is not in Cargo.lock. Add it if
    // the model keeps asking for patterns.
    let query = str_arg(args, "query")?.to_lowercase();
    let filter = args["glob"].as_str();
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let mut out = Vec::new();
    for item in walk(&base, exec) {
        let (rel, entry) = item?;
        let name = entry.file_name().to_string_lossy();
        if let Some(f) = filter {
            if !glob_match(f, &rel, true) && !glob_match(f, &name, true) {
                continue;
            }
        }
        // Binary or non-UTF-8 files are skipped, not reported as errors.
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&query) {
                let shown = if base.is_file() {
                    name.to_string()
                } else {
                    rel.clone()
                };
                out.push(format!("{shown}:{}: {}", i + 1, line.trim()));
                if out.len() >= MAX_MATCHES {
                    out.push(format!("[parado em {MAX_MATCHES} resultados]"));
                    return Ok(out.join("\n"));
                }
            }
        }
    }
    if out.is_empty() {
        return Ok("nenhuma ocorrência".into());
    }
    Ok(out.join("\n"))
}

/// The platform shell: `cmd /C` on Windows, `sh -c` elsewhere.
pub fn shell_command(cmd: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn shell(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let cmd = str_arg(args, "command")?;
    let secs = args["timeout_secs"].as_u64().unwrap_or(120).clamp(1, 600);
    let deadline = exec
        .deadline
        .min(std::time::Instant::now() + Duration::from_secs(secs));
    let mut child = shell_command(cmd)
        .current_dir(ctx.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Pipes are drained on threads: a child that fills a pipe buffer would
    // otherwise block forever while we wait for it to exit.
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|s| Box::new(s) as _));
    let err = drain(child.stderr.take().map(|s| Box::new(s) as _));
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        let stop = if exec.cancel.is_cancelled() {
            Some(ToolError::Cancelled)
        } else if std::time::Instant::now() >= deadline {
            Some(ToolError::Timeout(Duration::from_secs(secs)))
        } else {
            None
        };
        if let Some(e) = stop {
            // ponytail: kills the direct child only; grandchildren spawned by
            // `cmd`/`sh` may survive. Use a job object / process group if that
            // ever bites.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "sinal".into());
    let mut text = format!("exit {code}\n");
    if !stdout.is_empty() {
        text.push_str(&format!("--- stdout\n{stdout}"));
    }
    if !stderr.is_empty() {
        text.push_str(&format!("\n--- stderr\n{stderr}"));
    }
    if status.success() {
        Ok(text)
    } else {
        Err(ToolError::Failed(text))
    }
}

fn memory_recall(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let query = str_arg(args, "query")?;
    let (Some(mem), Some(scope)) = (ctx.memory, ctx.scope) else {
        return Err(ToolError::Denied("nenhuma memória configurada".into()));
    };
    let hits = mem
        .recall(scope, query, 8)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if hits.is_empty() {
        return Ok("nenhum fato encontrado".into());
    }
    Ok(hits
        .iter()
        .map(|h| format!("[{}] {}", h.fact.confidence.as_str(), h.fact.text))
        .collect::<Vec<_>>()
        .join("\n"))
}
