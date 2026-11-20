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
    // Read-only and stateless: safe to run side by side.
    fn concurrency_safe(&self, _: &Value) -> bool {
        matches!(
            self.spec.name.as_str(),
            "read_file" | "list_dir" | "glob" | "grep" | "memory_recall"
        )
    }
    fn uses_memory(&self) -> bool {
        matches!(self.spec.name.as_str(), "memory_recall" | "memory_save")
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
            "Procura em arquivos por regex (ex.: `vlan 13[0-9]`, `^interface`); texto que não é regex válida é buscado literalmente. Diferencia maiúsculas só com \"-i\": false.",
            json!({"type": "object", "properties": {
                "query": {"type": "string"},
                "path": {"type": "string", "description": "diretório ou arquivo (padrão: workspace)"},
                "glob": {"type": "string", "description": "filtra arquivos, ex.: *.rs"},
                "-i": {"type": "boolean", "description": "ignora maiúsculas (padrão true)"},
                "output_mode": {"type": "string", "enum": ["content", "files", "count"], "description": "content (padrão): linhas; files: só caminhos; count: ocorrências por arquivo"},
                "head_limit": {"type": "integer", "description": "máximo de resultados (padrão e teto 500)"}
            }, "required": ["query"]}),
            "linhas \"caminho:N: texto\", ou caminhos, ou \"caminho:contagem\"",
            Permission::Read,
            30,
            dir_action,
            grep,
        ),
        tool(
            "shell",
            "Executa um comando no shell dentro do workspace: bash (Git Bash no Windows, quando instalado; senão cmd), sh nos demais. Programa que não termina sozinho (janela gráfica, servidor, `python -m http.server`) precisa de run_in_background: em primeiro plano ele é encerrado no timeout e a janela fecha junto.",
            json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout_secs": {"type": "integer", "description": "padrão 120, máximo 600"},
                "run_in_background": {"type": "boolean", "description": "devolve um id na hora; leia com shell_output, encerre com shell_kill (até 4 jobs)"}
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
        tool(
            "memory_save",
            "Guarda na memória durável um fato, preferência ou instrução que o usuário disse ou confirmou e que vale para conversas futuras (ex.: editor favorito, idioma, estilo de resposta, nome do projeto, \"sempre rode os testes\"). Use por conta própria, sem pedir, assim que o usuário revelar algo assim. Uma frase curta e autossuficiente, na terceira pessoa (\"O usuário prefere respostas curtas\"). Não salve progresso de tarefa, resultados temporários nem segredos (senhas, chaves, tokens).",
            json!({"type": "object", "properties": {
                "text": {"type": "string", "description": "o fato, em uma frase autossuficiente"},
                "kind": {"type": "string", "enum": ["instruction", "preference", "fact"], "description": "instruction: regra a seguir sempre; preference: gosto do usuário; fact: informação (padrão)"}
            }, "required": ["text"]}),
            "confirmação do que foi salvo, ou que já estava na memória",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_save,
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

/// Console text as Windows really writes it: UTF-8 when it is, the OEM code
/// page otherwise (`dir`, `ping`, `taskkill` write CP850 on a pt-BR machine —
/// read as UTF-8, every accent became a U+FFFD).
pub fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            #[cfg(windows)]
            {
                oem(bytes)
            }
            #[cfg(not(windows))]
            {
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}

/// `MultiByteToWideChar(GetOEMCP())` straight from kernel32: one call, no
/// crate, and the page is the machine's own.
#[cfg(windows)]
fn oem(bytes: &[u8]) -> String {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetOEMCP() -> u32;
        fn MultiByteToWideChar(
            cp: u32,
            flags: u32,
            s: *const u8,
            len: i32,
            out: *mut u16,
            cap: i32,
        ) -> i32;
    }
    let lossy = || String::from_utf8_lossy(bytes).into_owned();
    if bytes.is_empty() || bytes.len() > i32::MAX as usize {
        return lossy();
    }
    // SAFETY: both calls only read `bytes` and write into `wide`, whose length
    // the first call reported.
    unsafe {
        let cp = GetOEMCP();
        let n = MultiByteToWideChar(
            cp,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return lossy();
        }
        let mut wide = vec![0u16; n as usize];
        if MultiByteToWideChar(
            cp,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            n,
        ) <= 0
        {
            return lossy();
        }
        std::ffi::OsString::from_wide(&wide)
            .to_string_lossy()
            .into_owned()
    }
}

fn read_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let raw = std::fs::read(&path)?;
    // Legacy pt-BR files are CP1252/Latin-1: a lossy read still reads, an
    // Input/output error does not.
    let lossy = std::str::from_utf8(&raw).is_err();
    let text = decode(&raw);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    exec.host.note_read(&path);
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().unwrap_or(2000) as usize;
    let total = text.lines().count();
    if lossy {
        eprintln!(
            "zerith: {} não está em UTF-8, lido com a página de código do sistema",
            path.display()
        );
    }
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

fn write_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    exec.host.check_editable(&path)?;
    exec.host.before_write(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    // The model knows what it just wrote; a follow-up edit needs no re-read.
    exec.host.note_read(&path);
    Ok(format!(
        "{} bytes escritos em {}",
        content.len(),
        path.display()
    ))
}

fn edit_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    exec.host.check_editable(&path)?;
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
    let raw = std::fs::read(&path)?;
    let text = String::from_utf8(raw).map_err(|_| {
        ToolError::Failed(format!(
            "{} não está em UTF-8 (arquivo antigo em CP1252/Latin-1): converta antes de editar",
            path.display()
        ))
    })?;
    // A Windows file uses CRLF while the model, which sees lines through
    // `.lines()`, sends LF. The text as it came is tried first (the file keeps
    // its own style); the CRLF retry is what made a multi-line edit of this
    // very repo fail.
    let crlf = |t: &str| t.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = match (text.matches(old).count(), text.contains("\r\n")) {
        (0, true) => (crlf(old), crlf(new)),
        _ => (old.to_string(), new.to_string()),
    };
    let (old, new) = (old.as_str(), new.as_str());
    let n = text.matches(old).count();
    if n > 0 {
        exec.host.before_write(&path)?;
    }
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
            exec.host.note_read(&path);
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
        // Directory order is whatever the file system hands back (it differed
        // between Windows and the CI runner): sorted, the model gets the same
        // answer twice.
        .sort_by_file_name()
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
    let insensitive = args["-i"].as_bool().unwrap_or(true);
    let raw = str_arg(args, "query")?;
    // A regex (`vlan 13[0-9]`, `^interface`); text that is not a valid
    // pattern (`foo(`) is searched literally, as before.
    let re = regex::RegexBuilder::new(raw)
        .case_insensitive(insensitive)
        .size_limit(1 << 20)
        .build()
        .or_else(|_| {
            regex::RegexBuilder::new(&regex::escape(raw))
                .case_insensitive(insensitive)
                .build()
        })
        .map_err(|e| ToolError::BadArguments(e.to_string()))?;
    let mode = args["output_mode"].as_str().unwrap_or("content");
    if !matches!(mode, "content" | "files" | "count") {
        return Err(ToolError::BadArguments(
            "output_mode precisa ser content, files ou count".into(),
        ));
    }
    let cap = args["head_limit"]
        .as_u64()
        .map_or(MAX_MATCHES, |n| (n as usize).clamp(1, MAX_MATCHES));
    let filter = args["glob"].as_str();
    let base = ctx.resolve(args["path"].as_str().unwrap_or("."));
    let mut out = Vec::new();
    let mut truncated = false;
    'files: for item in walk(&base, exec) {
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
        let shown = if base.is_file() {
            name.to_string()
        } else {
            rel.clone()
        };
        let mut hits = 0;
        for (i, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            hits += 1;
            if mode == "content" {
                out.push(format!("{shown}:{}: {}", i + 1, line.trim()));
                if out.len() >= cap {
                    truncated = true;
                    break 'files;
                }
            } else if mode == "files" {
                break;
            }
        }
        match mode {
            "files" if hits > 0 => out.push(shown),
            "count" if hits > 0 => out.push(format!("{shown}:{hits}")),
            _ => {}
        }
        if mode != "content" && out.len() >= cap {
            truncated = true;
            break;
        }
    }
    if out.is_empty() {
        return Ok("nenhuma ocorrência".into());
    }
    if truncated {
        out.push(format!("[parado em {cap} resultados]"));
    }
    Ok(out.join("\n"))
}

/// The platform shell: `cmd /C` on Windows, `sh -c` elsewhere.
///
/// The folder of programs that ship with the app (`bundled-tools`) is put at
/// the end of `PATH` for this child only, so a skill that says
/// `agent-desktop snapshot ...` works without the person installing anything
/// or the user's own `PATH` changing. A program of the same name already on
/// `PATH` wins.
pub fn shell_command(cmd: &str) -> Command {
    let bash = zerith_core::shellparse::windows_bash();
    let mut c = if let Some(bash) = bash {
        // Git Bash: `$HOME`, `&&`, `grep`, quotes as a developer writes them.
        let mut c = Command::new(bash);
        c.arg("-c").arg(cmd);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        // Through `.arg`, Rust escapes the quotes as `\"`, which cmd does not
        // understand: `git commit -m "msg"` and every path with a space broke.
        // `raw_arg` hands cmd the line the person would have typed.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("/S /C \"{cmd}\""));
        }
        #[cfg(not(windows))]
        {
            c.arg("/C").arg(cmd);
        }
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    if let Some(dir) = zerith_core::Paths::resolve().bundled_tools() {
        c.env("PATH", path_with(std::env::var_os("PATH"), &dir));
    }
    c
}

/// `existing` with `dir` appended (the platform separator), unless present.
fn path_with(existing: Option<std::ffi::OsString>, dir: &std::path::Path) -> std::ffi::OsString {
    let mut parts: Vec<std::path::PathBuf> = existing
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if !parts.iter().any(|p| p == dir) {
        parts.push(dir.to_path_buf());
    }
    std::env::join_paths(parts).unwrap_or_default()
}

fn shell(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let cmd = str_arg(args, "command")?;
    // Before either branch: a background job mutates the workspace too.
    exec.host.before_shell(cmd, ctx.workspace);
    if args["run_in_background"].as_bool().unwrap_or(false) {
        return crate::jobs::start(args, ctx.workspace, ctx.policy, exec.host);
    }
    let secs = args["timeout_secs"].as_u64().unwrap_or(120).clamp(1, 600);
    let deadline = exec
        .deadline
        .min(std::time::Instant::now() + Duration::from_secs(secs));
    let jail = zerith_security::jail::Jail::for_policy(ctx.policy);
    // Wrap BEFORE configuring stdio: on macOS `sandbox_exec` creates a fresh
    // `Command`, so stdio set earlier would be dropped.
    let mut command = shell_command(cmd);
    command.current_dir(ctx.workspace);
    let mut command = jail.wrap(command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The jail attaches the process to a Job Object on Windows (kill-on-close,
    // process and memory caps) and wraps it in `sandbox-exec` on macOS. It
    // fails closed: no jail, no process. What it does and does not confine is
    // in `zerith policy` and `docs/sandbox.md`.
    let mut jailed = jail.spawn_wrapped(command)?;
    let child = &mut jailed.child;
    // Pipes are drained on threads: a child that fills a pipe buffer would
    // otherwise block forever while we wait for it to exit.
    let drain = |r: Option<Box<dyn Read + Send>>| {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fill, finished) = (buf.clone(), done.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            if let Some(mut r) = r {
                while let Ok(n) = r.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut v = fill.lock().unwrap_or_else(|e| e.into_inner());
                    // ponytail: 8 MB per stream is a log's worth; a runaway
                    // producer is cut here instead of in RAM.
                    if v.len() < 8 * 1024 * 1024 {
                        v.extend_from_slice(&chunk[..n]);
                    }
                }
            }
            finished.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        (buf, done)
    };
    let (out, out_done) = drain(child.stdout.take().map(|s| Box::new(s) as _));
    let (err, err_done) = drain(child.stderr.take().map(|s| Box::new(s) as _));
    let text_of = |b: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
        let v = b.lock().unwrap_or_else(|e| e.into_inner());
        decode(&v)
    };
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
            jailed.kill_tree();
            // The pipe may still hold what the command printed before it was
            // stopped; a timeout that says nothing is not diagnosable.
            std::thread::sleep(Duration::from_millis(150));
            return Err(match e {
                ToolError::Timeout(d) => ToolError::Failed(format!(
                    "tempo esgotado após {}s: o comando e o que ele iniciou foram encerrados\n--- stdout (parcial)\n{}\n--- stderr (parcial)\n{}",
                    d.as_secs(),
                    text_of(&out),
                    text_of(&err)
                )),
                other => other,
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A detached grandchild (`start /b`, a dev server) keeps the pipes open
    // after the shell exits and `join()` used to wait for it forever. Give the
    // readers a short grace, then stop the tree: a background process belongs
    // to the jobs tool, not to this call.
    let grace = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < grace {
        use std::sync::atomic::Ordering::SeqCst;
        if out_done.load(SeqCst) && err_done.load(SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    jailed.kill_tree();
    let stdout = text_of(&out);
    let stderr = text_of(&err);
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

fn memory_save(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let text = str_arg(args, "text")?.trim();
    if text.is_empty() {
        return Err(ToolError::BadArguments("text está vazio".into()));
    }
    let kind = match args["kind"].as_str().unwrap_or("fact") {
        k @ ("instruction" | "preference" | "fact") => zerith_memory::FactKind::parse(k),
        other => {
            return Err(ToolError::BadArguments(format!(
                "kind precisa ser instruction, preference ou fact, não {other}"
            )))
        }
    };
    let (Some(mem), Some(scope)) = (ctx.memory, ctx.scope) else {
        return Err(ToolError::Denied("memória desligada ou pausada".into()));
    };
    let session = exec.host.session();
    // The same switch automatic extraction obeys: paused, or a private session.
    if !mem
        .is_extraction_allowed(session.as_deref().unwrap_or(""))
        .unwrap_or(false)
    {
        return Err(ToolError::Denied(
            "a memória está pausada para esta sessão".into(),
        ));
    }
    let known = mem
        .recall(scope, text, 20)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if known
        .iter()
        .any(|h| h.fact.superseded_by.is_none() && h.fact.text.trim().eq_ignore_ascii_case(text))
    {
        return Ok(format!("já estava na memória: {text}"));
    }
    mem.remember(
        scope,
        kind,
        text,
        zerith_memory::Confidence::Confirmed,
        session.as_deref(),
    )
    .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(format!("salvo na memória ({}): {text}", kind.as_str()))
}

#[cfg(test)]
mod path_tests {
    use super::path_with;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_bundled_folder_goes_last_so_the_users_own_program_wins() {
        let user = std::env::join_paths(["/usr/bin", "/opt/x"]).unwrap();
        let got = path_with(Some(user), Path::new("/app/bundled-tools"));
        let parts: Vec<PathBuf> = std::env::split_paths(&got).collect();
        assert_eq!(parts.last().unwrap(), Path::new("/app/bundled-tools"));
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn it_is_not_added_twice_and_works_without_a_path() {
        let once = path_with(None, Path::new("/b"));
        let twice = path_with(Some(once.clone()), Path::new("/b"));
        assert_eq!(once, twice);
        assert!(!path_with(Some(OsString::new()), Path::new("/b")).is_empty());
    }
}
