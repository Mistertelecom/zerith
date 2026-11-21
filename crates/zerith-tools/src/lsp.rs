//! Lazy stdio Language Server Protocol (LSP) client for real code intelligence:
//! `lsp_diagnostics`, `lsp_definition`, `lsp_references`, and `lsp_symbols`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Cpp,
}

impl Language {
    pub fn detect(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" => Some(Self::Python),
            "go" => Some(Self::Go),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" => Some(Self::Cpp),
            _ => None,
        }
    }

    pub fn language_id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Cpp => "cpp",
        }
    }

    pub fn server_candidates(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::Rust => &[("rust-analyzer", &[])],
            Self::TypeScript => &[("typescript-language-server", &["--stdio"])],
            Self::Python => &[("pyright-langserver", &["--stdio"]), ("pylsp", &[])],
            Self::Go => &[("gopls", &[])],
            Self::Cpp => &[("clangd", &[])],
        }
    }
}

pub fn file_uri(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    let stripped = s.strip_prefix("//?/").unwrap_or(&s);
    if stripped.starts_with('/') {
        format!("file://{stripped}")
    } else {
        format!("file:///{stripped}")
    }
}

pub fn write_lsp_frame(w: &mut impl Write, payload: &Value) -> io::Result<()> {
    let body =
        serde_json::to_vec(payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn read_lsp_frame(r: &mut impl BufRead) -> io::Result<Value> {
    let mut content_len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "LSP EOF"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().ok();
            }
        }
    }
    let len = content_len
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "header sem Content-Length"))?;
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame LSP muito grande",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

struct LspSession {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    next_id: AtomicU64,
    _child: Mutex<Child>,
}

impl Drop for LspSession {
    fn drop(&mut self) {
        if let Ok(mut c) = self._child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl LspSession {
    fn spawn(workspace: &Path, lang: Language) -> std::result::Result<Arc<Self>, String> {
        let mut spawned: Option<(Child, &str)> = None;
        for &(bin, args) in lang.server_candidates() {
            if let Ok(child) = Command::new(bin)
                .args(args)
                .current_dir(workspace)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                spawned = Some((child, bin));
                break;
            }
        }
        let (mut child, _) = spawned.ok_or_else(|| {
            let names: Vec<&str> = lang.server_candidates().iter().map(|c| c.0).collect();
            format!(
                "nenhum servidor LSP encontrado para {} (instale no PATH: {})",
                lang.language_id(),
                names.join(", ")
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "LSP sem stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "LSP sem stdout".to_string())?;
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Default::default();
        let diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>> = Default::default();
        let p_read = pending.clone();
        let d_read = diagnostics.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(msg) = read_lsp_frame(&mut reader) {
                if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                    if let Some(tx) = p_read.lock().ok().and_then(|mut m| m.remove(&id)) {
                        let _ = tx.send(msg);
                    }
                } else if msg.get("method").and_then(Value::as_str)
                    == Some("textDocument/publishDiagnostics")
                {
                    if let (Some(uri), Some(list)) = (
                        msg["params"]["uri"].as_str(),
                        msg["params"]["diagnostics"].as_array(),
                    ) {
                        if let Ok(mut d) = d_read.lock() {
                            d.insert(uri.to_string(), list.clone());
                        }
                    }
                }
            }
        });
        let sess = Arc::new(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            diagnostics,
            next_id: AtomicU64::new(1),
            _child: Mutex::new(child),
        });
        let root_uri = file_uri(workspace);
        sess.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": {},
                        "definition": { "linkSupport": false },
                        "references": {},
                        "documentSymbol": {},
                    }
                }
            }),
            Duration::from_secs(10),
        )?;
        sess.notify("initialized", json!({}))?;
        Ok(sess)
    }

    fn notify(&self, method: &str, params: Value) -> std::result::Result<(), String> {
        let payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut w = self
            .stdin
            .lock()
            .map_err(|e| format!("LSP stdin lock: {e}"))?;
        write_lsp_frame(&mut *w, &payload).map_err(|e| format!("LSP write: {e}"))
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> std::result::Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        if let Ok(mut m) = self.pending.lock() {
            m.insert(id, tx);
        }
        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let write_res = self
            .stdin
            .lock()
            .map_err(|e| format!("LSP lock: {e}"))
            .and_then(|mut w| write_lsp_frame(&mut *w, &payload).map_err(|e| e.to_string()));
        if let Err(e) = write_res {
            if let Ok(mut m) = self.pending.lock() {
                m.remove(&id);
            }
            return Err(e);
        }
        let resp = rx
            .recv_timeout(wait)
            .map_err(|_| format!("LSP timeout esperando resposta de `{method}`"))?;
        if let Some(err) = resp.get("error") {
            return Err(format!("LSP erro: {err}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    fn open_doc(&self, path: &Path, lang: Language) -> std::result::Result<String, String> {
        let uri = file_uri(path);
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("erro lendo {}: {e}", path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": lang.language_id(),
                    "version": 1,
                    "text": text,
                }
            }),
        )?;
        Ok(uri)
    }
}

type LspPool = Mutex<HashMap<(PathBuf, Language), Arc<LspSession>>>;

fn pool() -> &'static LspPool {
    static P: OnceLock<LspPool> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn session_for(workspace: &Path, path: &Path) -> Result<(Arc<LspSession>, Language, String)> {
    let lang = Language::detect(path).ok_or_else(|| {
        ToolError::BadArguments(format!("extensão sem suporte LSP: {}", path.display()))
    })?;
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let key = (ws.clone(), lang);
    let existing = pool().lock().ok().and_then(|m| m.get(&key).cloned());
    let sess = match existing {
        Some(s) => s,
        None => {
            let s = LspSession::spawn(&ws, lang).map_err(ToolError::Failed)?;
            if let Ok(mut m) = pool().lock() {
                m.insert(key, s.clone());
            }
            s
        }
    };
    let uri = sess.open_doc(path, lang).map_err(ToolError::Failed)?;
    Ok((sess, lang, uri))
}

fn format_locations(val: &Value) -> String {
    let items: Vec<&Value> = match val {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![val],
        _ => Vec::new(),
    };
    if items.is_empty() {
        return "nenhum resultado LSP".into();
    }
    let mut lines = Vec::with_capacity(items.len());
    for item in items.into_iter().take(100) {
        let uri = item["uri"]
            .as_str()
            .or_else(|| item["targetUri"].as_str())
            .unwrap_or("-");
        let range = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"))
            .unwrap_or(&Value::Null);
        let line = range["start"]["line"].as_u64().unwrap_or(0) + 1;
        let col = range["start"]["character"].as_u64().unwrap_or(0) + 1;
        lines.push(format!("{uri}:{line}:{col}"));
    }
    lines.join("\n")
}

fn format_diagnostics(diags: &[Value]) -> String {
    if diags.is_empty() {
        return "nenhum diagnóstico reportado pelo LSP".into();
    }
    diags
        .iter()
        .take(100)
        .map(|d| {
            let sev = match d["severity"].as_u64() {
                Some(1) => "erro",
                Some(2) => "aviso",
                Some(3) => "info",
                Some(4) => "dica",
                _ => "diag",
            };
            let line = d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
            let col = d["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
            let msg = d["message"].as_str().unwrap_or("");
            format!("{sev} L{line}:C{col} {msg}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make_with(
            "lsp_diagnostics",
            "Consulta diagnósticos (erros/avisos de tipo e compilação) via Language Server para um arquivo.",
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            "lista de diagnósticos por linha/coluna",
            Permission::Read,
            15,
            true,
            lsp_read_action,
            run_lsp_diagnostics,
        ),
        make_with(
            "lsp_definition",
            "Encontra a definição de um símbolo na posição (`line` 1-based, `character` 0-based) via LSP.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "character": {"type": "integer"}
            }, "required": ["path", "line", "character"]}),
            "localizações URI:linha:coluna",
            Permission::Read,
            15,
            true,
            lsp_read_action,
            run_lsp_definition,
        ),
        make_with(
            "lsp_references",
            "Encontra todas as referências de um símbolo na posição via LSP.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "character": {"type": "integer"}
            }, "required": ["path", "line", "character"]}),
            "lista de referências URI:linha:coluna",
            Permission::Read,
            15,
            true,
            lsp_read_action,
            run_lsp_references,
        ),
        make_with(
            "lsp_symbols",
            "Lista símbolos de um arquivo (`path`) ou busca símbolos no workspace (`query`, com `path` indicando a linguagem) via LSP.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "query": {"type": "string"}
            }, "required": ["path"]}),
            "lista de símbolos com tipo e linha",
            Permission::Read,
            15,
            true,
            lsp_read_action,
            run_lsp_symbols,
        ),
    ]
}

fn lsp_read_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    Ok(Some(Action::Read(ctx.resolve(str_arg(args, "path")?))))
}

fn run_lsp_diagnostics(args: &Value, ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    // Try pull diagnostics if supported; otherwise wait briefly for publishDiagnostics.
    if let Ok(res) = sess.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
        Duration::from_secs(3),
    ) {
        if let Some(items) = res.get("items").and_then(Value::as_array) {
            return Ok(format_diagnostics(items));
        }
    }
    std::thread::sleep(Duration::from_millis(600));
    let diags = sess
        .diagnostics
        .lock()
        .ok()
        .and_then(|d| d.get(&uri).cloned())
        .unwrap_or_default();
    Ok(format_diagnostics(&diags))
}

fn position_args(args: &Value) -> Result<(u64, u64)> {
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| ToolError::BadArguments("line precisa ser >= 1".into()))?
        .saturating_sub(1);
    let ch = args["character"]
        .as_u64()
        .ok_or_else(|| ToolError::BadArguments("character precisa ser >= 0".into()))?;
    Ok((line, ch))
}

fn run_lsp_definition(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (line, character) = position_args(args)?;
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = sess
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
            Duration::from_secs(8),
        )
        .map_err(ToolError::Failed)?;
    Ok(format_locations(&res))
}

fn run_lsp_references(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (line, character) = position_args(args)?;
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = sess
        .request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": true }
            }),
            Duration::from_secs(8),
        )
        .map_err(ToolError::Failed)?;
    Ok(format_locations(&res))
}

fn run_lsp_symbols(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = if let Some(q) = opt_str(args, "query") {
        sess.request(
            "workspace/symbol",
            json!({ "query": q }),
            Duration::from_secs(8),
        )
    } else {
        sess.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(8),
        )
    }
    .map_err(ToolError::Failed)?;
    let Some(arr) = res.as_array() else {
        return Ok("nenhum símbolo encontrado".into());
    };
    if arr.is_empty() {
        return Ok("nenhum símbolo encontrado".into());
    }
    let out: Vec<String> = arr
        .iter()
        .take(120)
        .map(|s| {
            let name = s["name"].as_str().unwrap_or("");
            let line = s["range"]["start"]["line"]
                .as_u64()
                .or_else(|| s["location"]["range"]["start"]["line"].as_u64())
                .unwrap_or(0)
                + 1;
            format!("L{line} {name}")
        })
        .collect();
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn content_length_framing_round_trips_jsonrpc() {
        let mut wire = Vec::new();
        let msg = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        write_lsp_frame(&mut wire, &msg).unwrap();
        let mut reader = BufReader::new(Cursor::new(wire));
        let parsed = read_lsp_frame(&mut reader).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn language_detection_and_formatters_work() {
        assert_eq!(
            Language::detect(Path::new("src/main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::detect(Path::new("App.tsx")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::detect(Path::new("script.py")),
            Some(Language::Python)
        );
        assert_eq!(Language::detect(Path::new("main.go")), Some(Language::Go));
        assert_eq!(Language::detect(Path::new("README.md")), None);

        let diag = format_diagnostics(&[json!({
            "severity": 1,
            "range": {"start": {"line": 9, "character": 4}},
            "message": "mismatched types"
        })]);
        assert_eq!(diag, "erro L10:C5 mismatched types");
        let loc = format_locations(&json!([{
            "uri": "file:///repo/src/lib.rs",
            "range": {"start": {"line": 14, "character": 2}}
        }]));
        assert_eq!(loc, "file:///repo/src/lib.rs:15:3");
    }
}
