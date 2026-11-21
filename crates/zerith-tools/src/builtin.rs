//! The built-in tools. Each is a struct holding its spec and a plain `run`.

use std::io::Read;
use std::path::{Path, PathBuf};
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
            "read_file" | "list_dir" | "glob" | "grep" | "memory_recall" | "session_search"
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
            "multi_edit",
            "Aplica várias substituições atômicas num arquivo (tudo ou nada). Preserva CRLF/LF, Latin-1 e faz fallback único ignorando diferenças menores de indentação/espaços.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "edits": {"type": "array", "items": {"type": "object", "properties": {
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                }, "required": ["old_string", "new_string"]}}
            }, "required": ["path", "edits"]}),
            "resumo atômico das edições aplicadas",
            Permission::Write,
            10,
            write_action,
            multi_edit,
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
                "case_insensitive": {"type": "boolean", "description": "alias de -i"},
                "-A": {"type": "integer", "description": "linhas de contexto após a ocorrência"},
                "after_context": {"type": "integer", "description": "alias de -A"},
                "-B": {"type": "integer", "description": "linhas de contexto antes da ocorrência"},
                "before_context": {"type": "integer", "description": "alias de -B"},
                "-C": {"type": "integer", "description": "linhas de contexto antes e depois"},
                "context": {"type": "integer", "description": "alias de -C"},
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
        tool(
            "session_search",
            "Busca mensagens e decisões em sessões anteriores salvas no histórico.",
            json!({"type": "object", "properties": {
                "query": {"type": "string", "description": "termos a procurar nas mensagens"},
                "limit": {"type": "integer", "description": "máximo de resultados (padrão 10, máx 30)"}
            }, "required": ["query"]}),
            "trechos de conversas anteriores com id e título da sessão",
            Permission::Read,
            5,
            |_, _| Ok(None),
            session_search,
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

#[derive(Clone, Copy)]
enum WhitespaceFallback {
    Applied(usize, usize),
    None,
    Ambiguous(usize),
}

fn find_unique_whitespace_span(buf: &str, old: &str) -> WhitespaceFallback {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let old_lines: Vec<String> = old.lines().map(norm).collect();
    if old_lines.is_empty() || old_lines.iter().all(String::is_empty) {
        return WhitespaceFallback::None;
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut off = 0usize;
    for chunk in buf.split_inclusive('\n') {
        let len = chunk.len();
        let trimmed_end = chunk.trim_end_matches(['\r', '\n']);
        spans.push((off, off + trimmed_end.len()));
        off += len;
    }
    if !buf.ends_with('\n') && off < buf.len() {
        spans.push((off, buf.len()));
    }
    if spans.len() < old_lines.len() {
        return WhitespaceFallback::None;
    }
    let line_norms: Vec<String> = spans.iter().map(|&(s, e)| norm(&buf[s..e])).collect();
    let mut hits = Vec::new();
    for i in 0..=spans.len() - old_lines.len() {
        if line_norms[i..i + old_lines.len()] == old_lines[..] {
            hits.push((spans[i].0, spans[i + old_lines.len() - 1].1));
        }
    }
    match hits.len() {
        0 => WhitespaceFallback::None,
        1 => WhitespaceFallback::Applied(hits[0].0, hits[0].1),
        n => WhitespaceFallback::Ambiguous(n),
    }
}

fn multi_edit(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    exec.host.check_editable(&path)?;
    let edits = args["edits"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| ToolError::BadArguments("edits precisa ser um array não vazio".into()))?;
    let raw = std::fs::read(&path)?;
    let (mut buf, is_latin1) = match String::from_utf8(raw) {
        Ok(s) => (s, false),
        Err(e) => (
            e.into_bytes().into_iter().map(|b| b as char).collect(),
            true,
        ),
    };
    let crlf = |t: &str| t.replace("\r\n", "\n").replace('\n', "\r\n");
    let has_crlf = buf.contains("\r\n");
    let mut applied_count = 0usize;
    for (idx, item) in edits.iter().enumerate() {
        let raw_old = item["old_string"].as_str().ok_or_else(|| {
            ToolError::BadArguments(format!("edits[{idx}].old_string precisa ser string"))
        })?;
        let raw_new = item["new_string"].as_str().ok_or_else(|| {
            ToolError::BadArguments(format!("edits[{idx}].new_string precisa ser string"))
        })?;
        let replace_all = item["replace_all"].as_bool().unwrap_or(false);
        if raw_old.is_empty() {
            return Err(ToolError::BadArguments(format!(
                "edits[{idx}].old_string está vazio"
            )));
        }
        if raw_old == raw_new {
            return Err(ToolError::BadArguments(format!(
                "edits[{idx}]: old_string e new_string são iguais"
            )));
        }
        let (old_s, new_s) = match (buf.matches(raw_old).count(), has_crlf) {
            (0, true) => (crlf(raw_old), crlf(raw_new)),
            _ => (raw_old.to_string(), raw_new.to_string()),
        };
        let n = buf.matches(&old_s).count();
        if n == 1 || (replace_all && n >= 1) {
            buf = if replace_all {
                buf.replace(&old_s, &new_s)
            } else {
                buf.replacen(&old_s, &new_s, 1)
            };
            applied_count += n;
        } else if n > 1 {
            return Err(ToolError::Failed(format!(
                "edits[{idx}] aparece {n} vezes (use replace_all=true ou dê mais contexto)"
            )));
        } else if !replace_all {
            match find_unique_whitespace_span(&buf, raw_old) {
                WhitespaceFallback::Applied(start, end) => {
                    let rep = if has_crlf {
                        crlf(raw_new)
                    } else {
                        raw_new.to_string()
                    };
                    buf.replace_range(start..end, &rep);
                    applied_count += 1;
                }
                WhitespaceFallback::Ambiguous(k) => {
                    return Err(ToolError::Failed(format!(
                        "edits[{idx}]: {k} trechos compatíveis com espaços normalizados (ambíguo)"
                    )));
                }
                WhitespaceFallback::None => {
                    return Err(ToolError::Failed(format!(
                        "edits[{idx}]: trecho não encontrado em {}",
                        path.display()
                    )));
                }
            }
        } else {
            return Err(ToolError::Failed(format!(
                "edits[{idx}]: trecho não encontrado em {}",
                path.display()
            )));
        }
    }
    let out_bytes = if is_latin1 {
        let mut bytes = Vec::with_capacity(buf.len());
        for ch in buf.chars() {
            let b = u8::try_from(ch as u32).map_err(|_| {
                ToolError::Failed("multi_edit: caractere fora de Latin-1 em arquivo legado".into())
            })?;
            bytes.push(b);
        }
        bytes
    } else {
        buf.into_bytes()
    };
    exec.host.before_write(&path)?;
    std::fs::write(&path, out_bytes)?;
    exec.host.note_read(&path);
    Ok(format!(
        "{} edição(ões) atômica(s) ({applied_count} substituição(ões)) em {}",
        edits.len(),
        path.display()
    ))
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
            Some(".git" | "target" | "node_modules" | "dist" | ".next" | "build")
        )
}

#[derive(Clone)]
struct IgnoreRule {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    pattern: String,
}

fn parse_gitignore(content: &str) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, mut pat) = match line.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let dir_only = pat.ends_with('/');
        if dir_only {
            pat = pat.trim_end_matches('/');
        }
        let anchored = pat.starts_with('/');
        if anchored {
            pat = pat.trim_start_matches('/');
        }
        if !pat.is_empty() {
            rules.push(IgnoreRule {
                negated,
                dir_only,
                anchored,
                pattern: pat.to_string(),
            });
        }
    }
    rules
}

fn load_root_gitignore(base: &Path) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    let mut dir = if base.is_dir() {
        base.to_path_buf()
    } else {
        base.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let mut chain = Vec::new();
    loop {
        let gi = dir.join(".gitignore");
        if gi.is_file() {
            if let Ok(text) = std::fs::read_to_string(&gi) {
                chain.push(parse_gitignore(&text));
            }
        }
        if dir.join(".git").exists() {
            break;
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => break,
        }
    }
    for group in chain.into_iter().rev() {
        rules.extend(group);
    }
    rules
}

fn matches_gitignore(rules: &[IgnoreRule], rel: &str, name: &str, is_dir: bool) -> bool {
    let mut ignored = false;
    for r in rules {
        if r.dir_only && !is_dir {
            continue;
        }
        let hit = if r.anchored || r.pattern.contains('/') {
            glob_match(&r.pattern, rel, true)
                || (is_dir && rel.starts_with(&format!("{}/", r.pattern)))
        } else {
            glob_match(&r.pattern, name, true) || glob_match(&r.pattern, rel, true)
        };
        if hit {
            ignored = !r.negated;
        }
    }
    ignored
}

/// Walks files under `base`, yielding (relative path with `/`, entry).
fn walk<'a>(
    base: &'a Path,
    exec: &'a Exec<'_>,
) -> impl Iterator<Item = Result<(String, walkdir::DirEntry)>> + 'a {
    let base_rules = load_root_gitignore(base);
    let nested = std::sync::Mutex::new(Vec::<(PathBuf, Vec<IgnoreRule>)>::new());
    walkdir::WalkDir::new(base)
        // Directory order is whatever the file system hands back (it differed
        // between Windows and the CI runner): sorted, the model gets the same
        // answer twice.
        .sort_by_file_name()
        .into_iter()
        .filter_entry(move |e| {
            if skipped(e) {
                return false;
            }
            if e.depth() == 0 {
                return true;
            }
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            let name = e.file_name().to_string_lossy();
            let is_dir = e.file_type().is_dir();
            if matches_gitignore(&base_rules, &rel, &name, is_dir) {
                return false;
            }
            if let Ok(guard) = nested.lock() {
                for (dir, rules) in guard.iter() {
                    if let Ok(sub_rel) = e.path().strip_prefix(dir) {
                        let sub = sub_rel.to_string_lossy().replace('\\', "/");
                        if matches_gitignore(rules, &sub, &name, is_dir) {
                            return false;
                        }
                    }
                }
            }
            if is_dir {
                let gi = e.path().join(".gitignore");
                if gi.is_file() {
                    if let Ok(text) = std::fs::read_to_string(&gi) {
                        let parsed = parse_gitignore(&text);
                        if !parsed.is_empty() {
                            if let Ok(mut guard) = nested.lock() {
                                guard.push((e.path().to_path_buf(), parsed));
                            }
                        }
                    }
                }
            }
            true
        })
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

fn rg_available() -> bool {
    static RG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RG.get_or_init(|| {
        Command::new("rg")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[allow(clippy::too_many_arguments)]
fn try_rg_grep(
    raw: &str,
    literal: bool,
    insensitive: bool,
    mode: &str,
    filter: Option<&str>,
    before: usize,
    after: usize,
    cap: usize,
    base: &Path,
) -> Option<String> {
    if !rg_available() || base.is_file() {
        return None;
    }
    let mut cmd = Command::new("rg");
    cmd.current_dir(base)
        .arg("--color")
        .arg("never")
        .arg("--sort")
        .arg("path")
        .arg("--no-heading");
    if literal {
        cmd.arg("-F");
    }
    if insensitive {
        cmd.arg("-i");
    }
    if let Some(g) = filter {
        cmd.arg("--glob").arg(g);
    }
    match mode {
        "files" => {
            cmd.arg("-l");
        }
        "count" => {
            cmd.arg("-c");
        }
        _ => {
            cmd.arg("--line-number");
            if before > 0 {
                cmd.arg("-B").arg(before.to_string());
            }
            if after > 0 {
                cmd.arg("-A").arg(after.to_string());
            }
        }
    }
    cmd.arg("--").arg(raw).arg(".");
    let out = cmd.stdin(Stdio::null()).output().ok()?;
    let code = out.status.code().unwrap_or(2);
    if code == 2 {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = Vec::new();
    let mut truncated = false;
    for l in text.lines() {
        if lines.len() >= cap {
            truncated = true;
            break;
        }
        let clean_path = l
            .strip_prefix("./")
            .or_else(|| l.strip_prefix(".\\"))
            .unwrap_or(l);
        if mode == "content" {
            if clean_path == "--" {
                lines.push("--".to_string());
            } else if let Some((file, ln, sep, body)) = parse_rg_content_line(clean_path) {
                lines.push(format!("{file}{sep}{ln}{sep} {body}"));
            } else {
                lines.push(clean_path.replace('\\', "/"));
            }
        } else if let Some((f, c)) = clean_path.rsplit_once(':') {
            lines.push(format!("{}:{c}", f.replace('\\', "/")));
        } else {
            lines.push(clean_path.replace('\\', "/"));
        }
    }
    if lines.is_empty() {
        return Some("nenhuma ocorrência".into());
    }
    if truncated {
        lines.push(format!("[parado em {cap} resultados]"));
    }
    Some(lines.join("\n"))
}

fn parse_rg_content_line(line: &str) -> Option<(String, &str, char, &str)> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        let sep = bytes[i];
        if sep != b':' && sep != b'-' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 && j < bytes.len() && bytes[j] == sep {
            let file = line[..i].replace('\\', "/");
            let ln = &line[i + 1..j];
            let body = line[j + 1..].trim();
            return Some((file, ln, sep as char, body));
        }
    }
    None
}

fn grep(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let insensitive = args["-i"]
        .as_bool()
        .or_else(|| args["case_insensitive"].as_bool())
        .unwrap_or(true);
    let ctx_both = args["-C"]
        .as_u64()
        .or_else(|| args["context"].as_u64())
        .unwrap_or(0) as usize;
    let before = args["-B"]
        .as_u64()
        .or_else(|| args["before_context"].as_u64())
        .map_or(ctx_both, |n| n as usize)
        .min(20);
    let after = args["-A"]
        .as_u64()
        .or_else(|| args["after_context"].as_u64())
        .map_or(ctx_both, |n| n as usize)
        .min(20);
    let raw = str_arg(args, "query")?;
    let mut literal = false;
    // A regex (`vlan 13[0-9]`, `^interface`); text that is not a valid
    // pattern (`foo(`) is searched literally, as before.
    let re = regex::RegexBuilder::new(raw)
        .case_insensitive(insensitive)
        .size_limit(1 << 20)
        .build()
        .or_else(|_| {
            literal = true;
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

    if let Some(fast) = try_rg_grep(
        raw,
        literal,
        insensitive,
        mode,
        filter,
        before,
        after,
        cap,
        &base,
    ) {
        return Ok(fast);
    }

    let mut candidates = Vec::new();
    for item in walk(&base, exec) {
        let (rel, entry) = item?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(f) = filter {
            if !glob_match(f, &rel, true) && !glob_match(f, &name, true) {
                continue;
            }
        }
        let shown = if base.is_file() { name } else { rel };
        candidates.push((shown, entry.into_path()));
    }

    // Parallel file read + regex matching across workers, then deterministic merge.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(2)
        .min(candidates.len().max(1));
    let mut per_file: Vec<(String, Vec<String>, usize)> = Vec::with_capacity(candidates.len());
    if candidates.len() >= 16 && workers > 1 {
        let chunk_sz = candidates.len().div_ceil(workers);
        let re_ref = &re;
        let parts: Vec<Vec<(String, Vec<String>, usize)>> = std::thread::scope(|s| {
            let handles: Vec<_> = candidates
                .chunks(chunk_sz)
                .map(|slice| {
                    s.spawn(move || {
                        let mut local = Vec::new();
                        for (shown, path) in slice {
                            if exec.cancel.is_cancelled() {
                                break;
                            }
                            if let Ok(text) = std::fs::read_to_string(path) {
                                let (lines, hits) =
                                    scan_file(shown, &text, re_ref, mode, before, after);
                                if hits > 0 {
                                    local.push((shown.clone(), lines, hits));
                                }
                            }
                        }
                        local
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for chunk in parts {
            per_file.extend(chunk);
        }
    } else {
        for (shown, path) in &candidates {
            exec.check()?;
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let (lines, hits) = scan_file(shown, &text, &re, mode, before, after);
            if hits > 0 {
                per_file.push((shown.clone(), lines, hits));
            }
        }
    }

    let mut out = Vec::new();
    let mut truncated = false;
    for (shown, lines, hits) in per_file {
        match mode {
            "files" => out.push(shown),
            "count" => out.push(format!("{shown}:{hits}")),
            _ => {
                if (before > 0 || after > 0) && !out.is_empty() {
                    out.push("--".to_string());
                }
                for l in lines {
                    out.push(l);
                    if out.len() >= cap {
                        truncated = true;
                        break;
                    }
                }
            }
        }
        if truncated || (mode != "content" && out.len() >= cap) {
            truncated = mode != "content" || truncated;
            if out.len() >= cap {
                out.truncate(cap);
                truncated = true;
            }
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

fn scan_file(
    shown: &str,
    text: &str,
    re: &regex::Regex,
    mode: &str,
    before: usize,
    after: usize,
) -> (Vec<String>, usize) {
    if before == 0 && after == 0 {
        let mut out = Vec::new();
        let mut hits = 0usize;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits += 1;
                if mode == "content" {
                    out.push(format!("{shown}:{}: {}", i + 1, line.trim()));
                } else if mode == "files" {
                    return (out, 1);
                }
            }
        }
        return (out, hits);
    }

    let all: Vec<&str> = text.lines().collect();
    let mut matched = vec![false; all.len()];
    let mut hits = 0usize;
    for (i, line) in all.iter().enumerate() {
        if re.is_match(line) {
            hits += 1;
            matched[i] = true;
            if mode == "files" {
                return (Vec::new(), 1);
            }
        }
    }
    if hits == 0 || mode != "content" {
        return (Vec::new(), hits);
    }
    let mut emit = vec![false; all.len()];
    for (i, &m) in matched.iter().enumerate() {
        if m {
            let lo = i.saturating_sub(before);
            let hi = (i + after).min(all.len().saturating_sub(1));
            for slot in &mut emit[lo..=hi] {
                *slot = true;
            }
        }
    }
    let mut out = Vec::new();
    let mut prev_emitted = None::<usize>;
    for i in 0..all.len() {
        if !emit[i] {
            continue;
        }
        if prev_emitted.is_some_and(|p| p + 1 < i) {
            out.push("--".to_string());
        }
        let sep = if matched[i] { ':' } else { '-' };
        out.push(format!("{shown}{sep}{}{sep} {}", i + 1, all[i].trim()));
        prev_emitted = Some(i);
    }
    (out, hits)
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
    shell_exec(args, ctx, exec, true)
}

/// Runs a foreground shell command without triggering `before_shell` workspace
/// snapshots (used by `diagnostics` for read-only checkers like `cargo check`).
pub(crate) fn shell_without_snapshot(
    args: &Value,
    ctx: &CallCtx<'_>,
    exec: &Exec<'_>,
) -> Result<String> {
    shell_exec(args, ctx, exec, false)
}

fn shell_exec(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>, snapshot: bool) -> Result<String> {
    let cmd = str_arg(args, "command")?;
    if snapshot {
        // Before either branch: a background job mutates the workspace too.
        exec.host.before_shell(cmd, ctx.workspace);
    }
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
    let drain = |r: Option<Box<dyn Read + Send>>, sink: crate::EventSink| {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (fill, finished) = (buf.clone(), done.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            let mut pending = Vec::new();
            let mut last_emit = std::time::Instant::now();
            if let Some(mut r) = r {
                while let Ok(n) = r.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    {
                        let mut v = fill.lock().unwrap_or_else(|e| e.into_inner());
                        // ponytail: 8 MB per stream is a log's worth; a runaway
                        // producer is cut here instead of in RAM.
                        if v.len() < 8 * 1024 * 1024 {
                            v.extend_from_slice(&chunk[..n]);
                        }
                    }
                    if pending.len() < 1024 {
                        let take = (1024 - pending.len()).min(n);
                        pending.extend_from_slice(&chunk[..take]);
                    }
                    if last_emit.elapsed() >= Duration::from_millis(250) && !pending.is_empty() {
                        sink.push(crate::HostEvent::ToolProgress {
                            tool: "shell".into(),
                            chunk: decode(&pending),
                        });
                        pending.clear();
                        last_emit = std::time::Instant::now();
                    }
                }
            }
            if !pending.is_empty() {
                sink.push(crate::HostEvent::ToolProgress {
                    tool: "shell".into(),
                    chunk: decode(&pending),
                });
            }
            finished.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        (buf, done)
    };
    let sink = exec.host.sink();
    let (out, out_done) = drain(child.stdout.take().map(|s| Box::new(s) as _), sink.clone());
    let (err, err_done) = drain(child.stderr.take().map(|s| Box::new(s) as _), sink);
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

fn session_search(args: &Value, _ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let query = str_arg(args, "query")?.trim();
    if query.is_empty() {
        return Err(ToolError::BadArguments("query está vazia".into()));
    }
    let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 30) as usize;
    exec.host.session_search(query, limit)
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

#[cfg(test)]
mod ondav4_tests {
    use crate::{CallCtx, Registry};
    use serde_json::json;
    use zerith_security::{preset, Mode, Policy};

    fn scratch() -> tempfile::TempDir {
        let p =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&p).unwrap();
        tempfile::Builder::new().tempdir_in(p).unwrap()
    }

    #[test]
    fn gitignore_and_grep_context_and_case_sensitivity_work() {
        let dir = scratch();
        let root = dir.path();
        std::fs::write(
            root.join(".gitignore"),
            "*.ignored\nlogs/\n!important.ignored\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.gitignore"), "*.tmp\n").unwrap();
        std::fs::write(root.join("skip.ignored"), "NEEDLE ignored\n").unwrap();
        std::fs::write(root.join("important.ignored"), "NEEDLE kept\n").unwrap();
        std::fs::write(root.join("logs/debug.txt"), "NEEDLE inside log\n").unwrap();
        std::fs::write(root.join("sub/cache.tmp"), "NEEDLE nested ignored\n").unwrap();
        std::fs::write(
            root.join("sub/code-file.rs"),
            "alpha\nNEEDLE Upper\nomega\nlowercase needle\n",
        )
        .unwrap();

        let pol = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, root);
        let ctx = CallCtx {
            workspace: pol.workspace.as_path(),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let reg = Registry::new();

        let gl = reg.call("glob", &json!({"pattern": "**/*"}), &ctx).unwrap();
        assert!(!gl.contains("skip.ignored"), "{gl}");
        assert!(!gl.contains("logs/debug.txt"), "{gl}");
        assert!(!gl.contains("cache.tmp"), "{gl}");
        assert!(gl.contains("important.ignored"), "{gl}");
        assert!(gl.contains("sub/code-file.rs"), "{gl}");

        let ctx_grep = reg
            .call(
                "grep",
                &json!({
                    "query": "NEEDLE",
                    "case_insensitive": false,
                    "-B": 1,
                    "-A": 1
                }),
                &ctx,
            )
            .unwrap();
        assert!(ctx_grep.contains("sub/code-file.rs-1- alpha"), "{ctx_grep}");
        assert!(
            ctx_grep.contains("sub/code-file.rs:2: NEEDLE Upper"),
            "{ctx_grep}"
        );
        assert!(ctx_grep.contains("sub/code-file.rs-3- omega"), "{ctx_grep}");
        assert!(
            !ctx_grep.contains("lowercase needle"),
            "busca case-sensitive não deveria casar lowercase: {ctx_grep}"
        );
        assert!(!ctx_grep.contains("skip.ignored"), "{ctx_grep}");
    }

    #[test]
    fn multi_edit_atomic_rollback_crlf_whitespace_fallback_and_latin1() {
        let dir = scratch();
        let root = dir.path();
        let pol = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, root);
        let ctx = CallCtx {
            workspace: pol.workspace.as_path(),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let reg = Registry::new();

        // 1. Atomic rollback if a second edit fails.
        let atomic_path = root.join("atomic.txt");
        std::fs::write(&atomic_path, "um\ndois\n").unwrap();
        let err = reg
            .call(
                "multi_edit",
                &json!({
                    "path": "atomic.txt",
                    "edits": [
                        {"old_string": "um", "new_string": "UM"},
                        {"old_string": "nao-existe", "new_string": "X"}
                    ]
                }),
                &ctx,
            )
            .unwrap_err();
        assert!(err.to_string().contains("edits[1]"), "{err}");
        assert_eq!(std::fs::read_to_string(&atomic_path).unwrap(), "um\ndois\n");

        // 2. CRLF preservation + unique normalized whitespace fallback.
        let crlf_path = root.join("crlf.rs");
        std::fs::write(
            &crlf_path,
            "fn a() {\r\n    let x = 1;\r\n    let y = 2;\r\n}\r\n",
        )
        .unwrap();
        reg.call(
            "multi_edit",
            &json!({
                "path": "crlf.rs",
                "edits": [
                    {"old_string": "  let x = 1;\nlet y = 2;", "new_string": "    let x = 10;\n    let y = 20;"}
                ]
            }),
            &ctx,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&crlf_path).unwrap(),
            "fn a() {\r\n    let x = 10;\r\n    let y = 20;\r\n}\r\n"
        );

        // 3. Ambiguous whitespace fallback must refuse instead of corrupting.
        std::fs::write(root.join("ambig.txt"), "  foo\n  bar\n    foo\n  bar\n").unwrap();
        let err_ambig = reg
            .call(
                "multi_edit",
                &json!({
                    "path": "ambig.txt",
                    "edits": [{"old_string": "foo\nbar", "new_string": "baz"}]
                }),
                &ctx,
            )
            .unwrap_err();
        assert!(err_ambig.to_string().contains("ambíguo"), "{err_ambig}");

        // 4. Latin-1 round-trip succeeds via multi_edit.
        let latin_path = root.join("legacy.txt");
        std::fs::write(&latin_path, b"N\xfamero 1\nlinha 2\n").unwrap();
        reg.call(
            "multi_edit",
            &json!({
                "path": "legacy.txt",
                "edits": [{"old_string": "linha 2", "new_string": "linha B"}]
            }),
            &ctx,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&latin_path).unwrap(),
            b"N\xfamero 1\nlinha B\n"
        );
    }
}
