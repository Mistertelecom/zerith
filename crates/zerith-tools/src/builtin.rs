//! The built-in tools. Each is a struct holding its spec and a plain `run`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::Action;

use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

mod edit;
mod search;

use edit::{edit_file, multi_edit};
use search::{glob, grep, list_dir, repo_map};

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
            "read_file"
                | "list_dir"
                | "glob"
                | "grep"
                | "repo_map"
                | "memory_recall"
                | "session_search"
        )
    }
    fn uses_memory(&self) -> bool {
        matches!(
            self.spec.name.as_str(),
            "memory_recall" | "memory_save" | "memory_update" | "memory_forget"
        )
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
            "Lê um arquivo de texto. Devolve linhas numeradas; use offset/limit para arquivos grandes. Também lê o texto de PDFs (datasheet, manual), página por página: aí offset/limit contam páginas.",
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
            "view_image",
            "Mostra para você uma imagem do disco (png, jpeg, gif ou webp, até 20 MB): print de tela, foto de equipamento, diagrama de topologia, gráfico. A imagem chega anexada logo depois do resultado.",
            json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "caminho da imagem"}
            }, "required": ["path"]}),
            "confirmação; a imagem vem anexada na mensagem seguinte",
            Permission::Read,
            10,
            read_action,
            view_image,
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
            "repo_map",
            "Mapeia a estrutura e assinaturas principais do repositório (funções, structs, traits, classes, interfaces e blocos de roteadores/switches em .cfg/.conf/.rsc) respeitando .gitignore e um orçamento de caracteres.",
            json!({"type": "object", "properties": {
                "path": {"type": "string", "description": "diretório ou arquivo base (padrão: workspace)"},
                "glob": {"type": "string", "description": "filtro opcional de arquivos (ex.: *.rs, src/**/*.ts, *.cfg)"},
                "max_chars": {"type": "integer", "description": "orçamento máximo de caracteres (padrão 6000, faixa 500..24000)"},
                "max_symbols_per_file": {"type": "integer", "description": "máximo de símbolos por arquivo (padrão 25)"}
            }}),
            "árvore compacta de arquivos com linhas e assinaturas L<N>: <assinatura>",
            Permission::Read,
            30,
            dir_action,
            repo_map,
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
            "Consulta ou lista fatos duráveis da memória do usuário (retorna [#id], tipo, confiança e texto). Se query for vazio ou \"*\", lista os fatos ativos.",
            json!({"type": "object", "properties": {"query": {"type": "string", "description": "termos para buscar ou \"*\" para listar todos os fatos recentes"}}}),
            "linhas \"[#id] [confirmed|inferred] (kind) texto\"",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_recall,
        ),
        tool(
            "memory_save",
            "Guarda na memória durável um fato, preferência, entidade (cliente, servidor, IP, ASN, pessoa), decisão, projeto ou instrução que o usuário disse ou confirmou e que vale para conversas futuras. Use por conta própria, sem pedir, assim que o usuário revelar algo assim. Uma frase curta e autossuficiente, na terceira pessoa (\"O usuário prefere respostas curtas\"). Não salve progresso de tarefa, resultados temporários nem segredos (senhas, chaves, tokens).",
            json!({"type": "object", "properties": {
                "text": {"type": "string", "description": "o fato, em uma frase autossuficiente"},
                "kind": {"type": "string", "enum": ["instruction", "preference", "fact", "entity", "decision", "project"], "description": "instruction: regra; preference: gosto; entity: pessoa/cliente/equipamento/IP; decision: decisão; project: projeto atual; fact: informação geral (padrão)"}
            }, "required": ["text"]}),
            "confirmação do que foi salvo com seu [#id], ou que já estava na memória",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_save,
        ),
        tool(
            "memory_update",
            "Atualiza o texto de um fato existente na memória pelo seu id numérico (obtido via memory_recall).",
            json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "id numérico do fato na memória"},
                "text": {"type": "string", "description": "novo texto atualizado do fato"}
            }, "required": ["id", "text"]}),
            "confirmação da atualização do fato",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_update,
        ),
        tool(
            "memory_forget",
            "Remove permanentemente um fato obsoleto ou incorreto da memória pelo seu id numérico (obtido via memory_recall).",
            json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "id numérico do fato a remover"}
            }, "required": ["id"]}),
            "confirmação da remoção do fato",
            Permission::Read,
            5,
            |_, _| Ok(None),
            memory_forget,
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

/// Marker that starts a `view_image` result: the turn loop attaches the image
/// named after it to the next request.
pub const IMAGE_MARKER: &str = "[imagem anexada para você ver]";

/// `view_image`: checks the file is a real image the model may read and says
/// where it is; the turn loop attaches it (images cannot ride inside a tool
/// result in every API). Concept from Codex `view_image` and Goose
/// `read_image` (both Apache-2.0); no code copied.
fn view_image(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    const CAP: u64 = 20 * 1024 * 1024;
    let path = ctx.resolve(str_arg(args, "path")?);
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() {
        return Err(ToolError::Failed(format!(
            "não é um arquivo: {}",
            path.display()
        )));
    }
    if meta.len() > CAP {
        return Err(ToolError::Failed(format!(
            "imagem grande demais ({} MB; limite 20 MB)",
            meta.len() / (1024 * 1024)
        )));
    }
    let mut head = [0u8; 12];
    let n = std::fs::File::open(&path)?.read(&mut head)?;
    let b = &head[..n];
    let kind = if b.starts_with(b"\x89PNG") {
        "png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpeg"
    } else if b.starts_with(b"GIF8") {
        "gif"
    } else if n == 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else {
        return Err(ToolError::Failed(
            "não é uma imagem png, jpeg, gif ou webp".into(),
        ));
    };
    exec.host.note_read(&path);
    let shown = path.display().to_string().replace('\\', "/");
    Ok(format!(
        "{IMAGE_MARKER} {kind}, {} KB: ![imagem](<{shown}>)",
        meta.len().div_ceil(1024)
    ))
}

/// A PDF's text, page by page, as `read_file` lines: `offset`/`limit` count
/// pages here. Concept after opencode's `read` on PDFs (MIT); the text comes
/// from the `pdf-extract` crate. Scanned PDFs (images only) come back empty.
fn read_pdf(path: &std::path::Path, args: &Value, raw: &[u8]) -> Result<String> {
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(raw))
        .map_err(|_| ToolError::Failed("o leitor de PDF falhou neste arquivo".into()))?
        .map_err(|e| ToolError::Failed(format!("não consegui ler o PDF: {e}")))?;
    let total = pages.len();
    let first = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let count = args["limit"].as_u64().map_or(20, |n| n as usize).max(1);
    let mut out = format!("PDF {} ({total} páginas)\n", path.display());
    let mut chars = 0usize;
    let mut shown = 0usize;
    for (i, text) in pages.iter().enumerate().skip(first - 1).take(count) {
        let text = text.trim();
        out.push_str(&format!("\n--- página {} ---\n", i + 1));
        out.push_str(if text.is_empty() {
            "[sem texto: página escaneada ou só imagem]"
        } else {
            text
        });
        out.push('\n');
        chars += text.len();
        shown = i + 1;
        if chars > 60_000 {
            break;
        }
    }
    if shown == 0 {
        return Err(ToolError::Failed(format!(
            "o PDF tem {total} páginas; offset {first} passa do fim"
        )));
    }
    if shown < total {
        out.push_str(&format!(
            "\n[mostradas até a página {shown} de {total}; continue com offset={}]\n",
            shown + 1
        ));
    }
    Ok(out)
}

/// Characters of one line `read_file` shows before cutting it.
const READ_LINE_CHARS: usize = 2000;
/// Characters one `read_file` call returns, roughly (stops on a line boundary).
const READ_TOTAL_CHARS: usize = 100_000;

fn read_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let raw = std::fs::read(&path)?;
    if raw.starts_with(b"%PDF-") {
        exec.host.note_read(&path);
        return read_pdf(&path, args, &raw);
    }
    // Legacy pt-BR files are Latin-1/CP1252, not the console's OEM page:
    // decode them the same way multi_edit/edit_file write them back, so an
    // accented `old_string` copied from here is found there.
    let lossy = std::str::from_utf8(&raw).is_err();
    let text = if lossy {
        raw.iter().map(|&b| b as char).collect()
    } else {
        decode(&raw)
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    exec.host.note_read(&path);
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().unwrap_or(2000) as usize;
    let total = text.lines().count();
    if lossy {
        eprintln!(
            "zerith: {} não está em UTF-8, lido como Latin-1",
            path.display()
        );
    }
    // Line and total caps after Hermes `read_file` (MIT: 2000 chars per
    // line, ~100K chars per read with a next offset): a minified JSON of one
    // 500 KB line used to come back whole and fill the window.
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, l) in text.lines().enumerate().skip(offset - 1).take(limit) {
        if last >= offset && out.len() >= READ_TOTAL_CHARS {
            break;
        }
        let n = l.chars().count();
        if n > READ_LINE_CHARS {
            let head: String = l.chars().take(READ_LINE_CHARS).collect();
            out.push_str(&format!(
                "{}\t{head}… [linha cortada: {n} caracteres]\n",
                i + 1
            ));
        } else {
            out.push_str(&format!("{}\t{l}\n", i + 1));
        }
        last = i + 1;
    }
    if last < total {
        let cut = if out.len() >= READ_TOTAL_CHARS {
            format!(" (limite de {READ_TOTAL_CHARS} caracteres)")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "[linhas {offset}-{last} de {total}{cut}; próximo offset: {}]\n",
            last + 1
        ));
    }
    if total == 0 {
        out.push_str("[arquivo vazio]\n");
    }
    Ok(out)
}

/// Writes a temp file beside `path`, syncs it, then renames it over `path`:
/// a crash or full disk mid-write leaves the old file whole, never a
/// truncated one. The old file's permissions carry over; a symlink is
/// written through (its target replaced), as `fs::write` did.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.zerith-tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // A read-only target fails the rename below, as `fs::write` failed;
        // copying the flag would only strand a read-only temp file.
        if let Some(p) = std::fs::metadata(&target)
            .ok()
            .map(|m| m.permissions())
            .filter(|p| !p.readonly())
        {
            f.set_permissions(p)?;
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_file(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    exec.host.check_editable(&path)?;
    crate::lint::check(&path, content.as_bytes())?;
    let pc = crate::postcheck::before(ctx.workspace, &path);
    exec.host.before_write(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&path, content.as_bytes())?;
    // The model knows what it just wrote; a follow-up edit needs no re-read.
    exec.host.note_read(&path);
    let mut msg = format!("{} bytes escritos em {}", content.len(), path.display());
    crate::postcheck::append(&mut msg, &path, pc);
    Ok(msg)
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
    // Output goes to a model and a <pre>, not a terminal: color codes are noise.
    c.env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .env("FORCE_COLOR", "0");
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

fn looks_like_detached_launch(cmd: &str) -> bool {
    cmd.split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split([';', '\n']))
        .any(|seg| {
            let s = seg.trim_start().to_ascii_lowercase();
            s.starts_with("start ")
                || s.starts_with("start\t")
                || s.starts_with("cmd /c start")
                || s.starts_with("cmd.exe /c start")
                || s.starts_with("start-process ")
                || s.starts_with("pythonw ")
                || s.starts_with("pythonw.exe ")
                || s.starts_with("nohup ")
        })
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
                    "tempo esgotado após {}s: o comando e o que ele iniciou foram encerrados.\nDICA: Se o comando abre uma janela gráfica (GUI) ou servidor contínuo, NÃO repita em primeiro plano — execute UMA ÚNICA VEZ com run_in_background: true.\n--- stdout (parcial)\n{}\n--- stderr (parcial)\n{}",
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
    let killed_descendant = {
        use std::sync::atomic::Ordering::SeqCst;
        !(out_done.load(SeqCst) && err_done.load(SeqCst))
    };
    jailed.kill_tree();
    let stdout = text_of(&out);
    let stderr = text_of(&err);
    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "sinal".into());
    let mut text = format!("exit {code}\n");
    // On failure stderr goes first: if the output is cut, the error survives.
    let mut parts = [("stdout", &stdout), ("stderr", &stderr)];
    if !status.success() {
        parts.swap(0, 1);
        // A full stdout made the model miss `exit 1` (ufw/nft/iptables
        // without root): the error lines and the root hint come first.
        // The first line stays `exit N` (verify::exit_of reads it).
        text.push_str("[o comando FALHOU]\n");
        let errs = error_lines(&stderr, &stdout);
        if !errs.is_empty() {
            text.push_str(&format!("[erros]\n{}\n", errs.join("\n")));
        }
        if let Some(hint) = root_hint(cmd, &format!("{stderr}\n{stdout}")) {
            text.push_str(&hint);
            text.push('\n');
        }
    }
    for (name, body) in parts.iter().filter(|(_, b)| !b.is_empty()) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("--- {name}\n{body}"));
    }
    if status.success() && (killed_descendant || looks_like_detached_launch(cmd)) {
        text.push_str("\n[AVISO] O comando deixou ou tentou abrir um processo filho/janela em segundo plano, que foi ENCERRADO automaticamente ao fim do shell em primeiro plano. Para abrir janelas GUI ou servidores contínuos, execute o comando direto (sem `start`/`nohup`) UMA ÚNICA VEZ com `run_in_background: true`.");
    }
    if status.success() {
        Ok(text)
    } else {
        Err(ToolError::Failed(text))
    }
}

const ROOT_SIGNS: [&str; 7] = [
    "permission denied",
    "must be root",
    "need to be root",
    "operation not permitted",
    "can't initialize iptables",
    "you must be superuser",
    "are you root",
];

/// Up to 6 lines of a failed run that read like errors, stderr first.
fn error_lines(stderr: &str, stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in stderr.lines().chain(stdout.lines()) {
        let low = l.to_ascii_lowercase();
        let hit = ROOT_SIGNS.iter().any(|s| low.contains(s))
            || [
                "error",
                "erro",
                "fatal",
                "failed",
                "falhou",
                "denied",
                "not found",
            ]
            .iter()
            .any(|s| low.contains(s));
        let line = l.trim().to_string();
        if hit && !line.is_empty() && !out.contains(&line) {
            out.push(line);
            if out.len() == 6 {
                break;
            }
        }
    }
    out
}

/// `sudo` in front of each command of a `;`/`&&`/`||` chain (not after a
/// pipe: only the left side reads the protected thing).
fn with_sudo(cmd: &str) -> String {
    let mut out = String::new();
    let mut rest = cmd.trim();
    loop {
        let cut = [";", "&&", "||"]
            .iter()
            .filter_map(|sep| rest.find(sep).map(|i| (i, sep.len())))
            .min();
        let (seg, tail) = match cut {
            Some((i, n)) => (&rest[..i], Some(&rest[i..i + n])),
            None => (rest, None),
        };
        let s = seg.trim();
        if !s.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            if !s.starts_with("sudo ") {
                out.push_str("sudo ");
            }
            out.push_str(s);
        }
        match (cut, tail) {
            (Some((i, n)), Some(sep)) => {
                out.push_str(&format!(" {sep}"));
                rest = &rest[i + n..];
            }
            _ => break,
        }
    }
    out.replace(" ;", ";")
}

/// A note for the model when a failure looks like missing root. For
/// `ssh host 'cmd'` the ready-to-paste `ssh -t host 'sudo cmd'` (`-t`: sudo
/// needs a terminal to ask for the password).
fn root_hint(cmd: &str, output: &str) -> Option<String> {
    let low = output.to_ascii_lowercase();
    if !ROOT_SIGNS.iter().any(|s| low.contains(s)) {
        return None;
    }
    let base = "[precisa de root: rode com sudo; se o sudo pedir senha, mostre o comando exato \
                ao usuário e peça para ele rodar]";
    let c = cmd.trim();
    let ready = match c.strip_prefix("ssh ") {
        Some(rest) => {
            let q = rest.find(['\'', '"'])?;
            let quote = &rest[q..=q];
            let end = rest[q + 1..].rfind(quote)? + q + 1;
            let host = rest[..q].trim();
            let host = if host.split_whitespace().any(|w| w == "-t") {
                host.to_string()
            } else {
                format!("-t {host}")
            };
            format!("ssh {host} {quote}{}{quote}", with_sudo(&rest[q + 1..end]))
        }
        None => with_sudo(c),
    };
    Some(format!("{base}\ncomando pronto: {ready}"))
}

fn memory_recall(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let query = args["query"].as_str().unwrap_or("*").trim();
    let (Some(mem), Some(scope)) = (ctx.memory, ctx.scope) else {
        return Err(ToolError::Denied("nenhuma memória configurada".into()));
    };
    let mut facts: Vec<zerith_memory::Fact> = if query.is_empty() || query == "*" {
        Vec::new()
    } else {
        mem.recall(scope, query, 12)
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .into_iter()
            .map(|h| h.fact)
            .collect()
    };
    if facts.is_empty() {
        facts = mem
            .list(scope, 25)
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .into_iter()
            .filter(|f| f.superseded_by.is_none())
            .collect();
    }
    if facts.is_empty() {
        return Ok("nenhum fato encontrado".into());
    }
    Ok(facts
        .iter()
        .map(|f| {
            format!(
                "[#{}] [{}] ({}) {}",
                f.id,
                f.confidence.as_str(),
                f.kind.as_str(),
                f.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn memory_save(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let text = str_arg(args, "text")?.trim();
    if text.is_empty() {
        return Err(ToolError::BadArguments("text está vazio".into()));
    }
    let kind = match args["kind"].as_str().unwrap_or("fact") {
        k @ ("instruction" | "preference" | "fact" | "entity" | "decision" | "project") => {
            zerith_memory::FactKind::parse(k)
        }
        other => {
            return Err(ToolError::BadArguments(format!(
                "kind precisa ser instruction, preference, fact, entity, decision ou project, não {other}"
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
        .list(scope, 100)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    if let Some(existing) = known
        .iter()
        .find(|f| f.superseded_by.is_none() && f.text.trim().eq_ignore_ascii_case(text))
    {
        return Ok(format!(
            "já estava na memória [#{id}]: {text}",
            id = existing.id
        ));
    }
    let id = mem
        .remember(
            scope,
            kind,
            text,
            zerith_memory::Confidence::Confirmed,
            session.as_deref(),
        )
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(format!(
        "salvo na memória [#{id}] ({}): {text}",
        kind.as_str()
    ))
}

fn memory_update(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let id = args["id"]
        .as_i64()
        .ok_or_else(|| ToolError::BadArguments("id precisa ser um inteiro".into()))?;
    let text = str_arg(args, "text")?.trim();
    if text.is_empty() {
        return Err(ToolError::BadArguments("text está vazio".into()));
    }
    let Some(mem) = ctx.memory else {
        return Err(ToolError::Denied("memória desligada ou pausada".into()));
    };
    mem.update(id, text)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(format!("fato [#{id}] atualizado na memória: {text}"))
}

fn memory_forget(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let id = args["id"]
        .as_i64()
        .ok_or_else(|| ToolError::BadArguments("id precisa ser um inteiro".into()))?;
    let Some(mem) = ctx.memory else {
        return Err(ToolError::Denied("memória desligada ou pausada".into()));
    };
    mem.forget(id)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(format!("fato [#{id}] removido da memória"))
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
mod root_hint_tests {
    use super::{error_lines, root_hint};

    #[test]
    fn missing_root_gets_a_note_and_a_ready_ssh_sudo_command() {
        let out = "Status: active\nERROR: You need to be root to run this script\n";
        let h = root_hint("ssh admin@fw1 'ufw status verbose'", out).unwrap();
        assert!(h.starts_with("[precisa de root: rode com sudo;"), "{h}");
        assert!(
            h.ends_with("comando pronto: ssh -t admin@fw1 'sudo ufw status verbose'"),
            "{h}"
        );
        let nft = "netlink: Error: cache initialization failed: Operation not permitted";
        let h = root_hint(
            "ssh -p 2222 root@10.0.0.2 \"nft list ruleset; iptables -S && ufw status\"",
            nft,
        )
        .unwrap();
        assert!(
            h.ends_with(
                "ssh -t -p 2222 root@10.0.0.2 \"sudo nft list ruleset; sudo iptables -S && sudo ufw status\""
            ),
            "{h}"
        );
        let ipt = "iptables v1.8.7 (nf_tables): can't initialize iptables table `filter': Permission denied (you must be root)";
        assert!(root_hint("iptables -S", ipt)
            .unwrap()
            .ends_with("comando pronto: sudo iptables -S"));
        assert!(root_hint("ls /nope", "ls: cannot access '/nope': No such file").is_none());
        let errs = error_lines(ipt, "Chain INPUT\nline\n");
        assert_eq!(errs.len(), 1);
    }
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

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "old content, longer").unwrap();
        super::write_atomic(&f, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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

    #[test]
    fn repo_map_extracts_code_and_network_config_symbols() {
        let dir = scratch();
        let root = dir.path();
        std::fs::write(
            root.join("lib.rs"),
            "pub struct RouterConfig {\n    pub asn: u32,\n}\n\npub fn apply_bgp(cfg: &RouterConfig) -> bool {\n    true\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("core.cfg"),
            "!\nhostname PE-CORE-01\ninterface GigabitEthernet0/0/0\n description Uplink\nrouter bgp 65001\n neighbor 10.0.0.1 remote-as 65002\n",
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

        let out = reg.call("repo_map", &json!({}), &ctx).unwrap();
        assert!(out.contains("lib.rs"), "{out}");
        assert!(out.contains("pub struct RouterConfig"), "{out}");
        assert!(
            out.contains("pub fn apply_bgp(cfg: &RouterConfig) -> bool"),
            "{out}"
        );
        assert!(out.contains("core.cfg"), "{out}");
        assert!(out.contains("hostname PE-CORE-01"), "{out}");
        assert!(out.contains("interface GigabitEthernet0/0/0"), "{out}");
        assert!(out.contains("router bgp 65001"), "{out}");
    }

    #[test]
    fn read_file_cuts_giant_lines_and_stops_near_the_total_cap() {
        let dir = scratch();
        let root = dir.path();
        // A minified JSON of one 500 KB line.
        let item = r#"{"id":12345,"nome":"porta-ge-0/0/1","vlan":130},"#;
        let mut json = String::from("[");
        while json.len() < 500_000 {
            json.push_str(item);
        }
        json.push_str("{}]");
        std::fs::write(root.join("min.json"), &json).unwrap();
        let pol = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, root);
        let ctx = CallCtx {
            workspace: pol.workspace.as_path(),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let reg = Registry::new();
        let out = reg
            .call("read_file", &json!({"path": "min.json"}), &ctx)
            .unwrap();
        assert!(out.len() < 3000, "{} bytes", out.len());
        assert!(
            out.contains(&format!("… [linha cortada: {} caracteres]", json.len())),
            "{out}"
        );

        // Many long lines: stops near 100K chars and says where to go on.
        let line = "x".repeat(1500);
        let many: String = (0..200)
            .map(|_| {
                format!(
                    "{line}
"
                )
            })
            .collect();
        std::fs::write(root.join("many.txt"), many).unwrap();
        let out = reg
            .call("read_file", &json!({"path": "many.txt"}), &ctx)
            .unwrap();
        assert!(out.len() < 105_000, "{} bytes", out.len());
        let next = out
            .rsplit("próximo offset: ")
            .next()
            .and_then(|t| t.trim_end().trim_end_matches(']').parse::<usize>().ok())
            .expect("próximo offset");
        assert!(next > 1 && next < 200, "{next}");
        let rest = reg
            .call(
                "read_file",
                &json!({"path": "many.txt", "offset": next}),
                &ctx,
            )
            .unwrap();
        assert!(rest.starts_with(&format!("{next}	")), "{}", &rest[..40]);
    }

    #[test]
    fn detached_launch_detector_catches_gui_wrappers_only() {
        use super::looks_like_detached_launch;
        assert!(looks_like_detached_launch("start python calc.py"));
        assert!(looks_like_detached_launch(
            "cd app && Start-Process calc.exe"
        ));
        assert!(looks_like_detached_launch("pythonw app.py"));
        assert!(looks_like_detached_launch("nohup server &"));
        assert!(looks_like_detached_launch("cmd /c start app.exe"));

        assert!(!looks_like_detached_launch("python --version"));
        assert!(!looks_like_detached_launch("cargo test"));
        assert!(!looks_like_detached_launch("echo start"));
    }
}
