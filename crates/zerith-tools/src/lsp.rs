//! Lazy stdio Language Server Protocol (LSP) client for real code intelligence:
//! `lsp` (`action`: diagnostics, definition, references, symbols, hover, implementation, rename).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_security::{Action, Verdict};

use crate::fntool::{opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

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
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" | "pyi" => Some(Self::Python),
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

    /// Per-file LSP `languageId` (e.g. `javascript`, `javascriptreact`, `typescriptreact`, `c`, `cpp`).
    pub fn id_str(self, path: &Path) -> &'static str {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("rs") => "rust",
            Some("js" | "mjs" | "cjs") => "javascript",
            Some("jsx") => "javascriptreact",
            Some("ts" | "mts" | "cts") => "typescript",
            Some("tsx") => "typescriptreact",
            Some("py" | "pyi") => "python",
            Some("go") => "go",
            Some("c" | "h") => "c",
            Some("cpp" | "hpp" | "cc" | "cxx" | "hxx") => "cpp",
            _ => self.language_id(),
        }
    }

    pub fn server_candidates(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::Rust => &[("rust-analyzer", &[])],
            Self::TypeScript => &[
                ("typescript-language-server", &["--stdio"]),
                ("vtsls", &["--stdio"]),
            ],
            Self::Python => &[
                ("basedpyright-langserver", &["--stdio"]),
                ("pyright-langserver", &["--stdio"]),
                ("pylsp", &[]),
            ],
            Self::Go => &[("gopls", &[])],
            Self::Cpp => &[("clangd", &[])],
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    let bytes = decoded.as_bytes();
    // Handle `file:///C:/...` -> `C:/...`
    let normalized = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        &decoded[1..]
    } else {
        &decoded
    };
    Some(PathBuf::from(normalized))
}

fn uri_to_display_path(uri: &str, workspace: &Path) -> String {
    let Some(path) = uri_to_path(uri) else {
        return uri.to_string();
    };
    let clean_ws = |p: &Path| -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        s.strip_prefix("//?/").unwrap_or(&s).to_string()
    };
    let path_str = clean_ws(&path);
    let mut roots = vec![clean_ws(workspace)];
    if let Ok(canon) = std::fs::canonicalize(workspace) {
        let cs = clean_ws(&canon);
        if !roots.contains(&cs) {
            roots.push(cs);
        }
    }
    for root in roots {
        let root_trim = root.trim_end_matches('/');
        if root_trim.is_empty() {
            continue;
        }
        let matches = if cfg!(windows) {
            path_str.len() > root_trim.len()
                && path_str[..root_trim.len()].eq_ignore_ascii_case(root_trim)
                && path_str.as_bytes()[root_trim.len()] == b'/'
        } else {
            path_str.len() > root_trim.len()
                && path_str.starts_with(root_trim)
                && path_str.as_bytes()[root_trim.len()] == b'/'
        };
        if matches {
            return path_str[root_trim.len() + 1..].to_string();
        }
    }
    path_str
}

/// Builds a JSON-RPC response for a server-to-client request (a message that has
/// both `"id"` and `"method"`), so servers like `typescript-language-server`,
/// `pyright`, and `rust-analyzer` never stall waiting on client replies.
fn server_request_response(msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").filter(|v| !v.is_null())?;
    let result = match method {
        "workspace/configuration" => {
            let n = msg["params"]["items"].as_array().map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        }
        "workspace/applyEdit" => json!({ "applied": false }),
        _ => Value::Null,
    };
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
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

const MAX_LSP_SESSIONS: usize = 3;
const LSP_IDLE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocSyncKind {
    Open(i32),
    Change(i32),
}

fn next_doc_sync(docs: &mut HashMap<String, i32>, uri: &str) -> DocSyncKind {
    if let Some(ver) = docs.get_mut(uri) {
        *ver += 1;
        DocSyncKind::Change(*ver)
    } else {
        docs.insert(uri.to_string(), 1);
        DocSyncKind::Open(1)
    }
}

type DiagnosticsMap = Arc<(Mutex<HashMap<String, Vec<Value>>>, Condvar)>;

struct LspSession {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    diagnostics: DiagnosticsMap,
    pull_diagnostics: AtomicBool,
    open_docs: Mutex<HashMap<String, i32>>,
    last_used: Mutex<std::time::Instant>,
    next_id: AtomicU64,
    _child: Mutex<Child>,
}

impl Drop for LspSession {
    fn drop(&mut self) {
        let _ = self.notify("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        if let Ok(mut c) = self._child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Resolves an LSP server binary in workspace/ancestor `node_modules/.bin`
/// first, then across `PATH`, including `.cmd`/`.bat`/`.exe` shims on Windows
/// (Rust's `Command::new("typescript-language-server")` only searches `.exe`
/// and fails on npm's `.cmd` shims unless given the resolved path).
fn resolve_server_program(workspace: &Path, bin: &str) -> Option<PathBuf> {
    let probe_dir = |dir: &Path| -> Option<PathBuf> {
        #[cfg(windows)]
        let exts = [".exe", ".cmd", ".bat", ""];
        #[cfg(not(windows))]
        let exts = [""];
        for ext in exts {
            let candidate = dir.join(format!("{bin}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    };

    for anc in workspace.ancestors().take(4) {
        let local_bin = anc.join("node_modules").join(".bin");
        if let Some(hit) = probe_dir(&local_bin) {
            return Some(hit);
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(hit) = probe_dir(&dir) {
                return Some(hit);
            }
        }
    }
    None
}

impl LspSession {
    fn is_alive(&self) -> bool {
        self._child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok())
            .is_some_and(|st| st.is_none())
    }

    fn touch(&self) {
        if let Ok(mut t) = self.last_used.lock() {
            *t = std::time::Instant::now();
        }
    }

    fn last_used_at(&self) -> std::time::Instant {
        self.last_used
            .lock()
            .map(|t| *t)
            .unwrap_or_else(|_| std::time::Instant::now())
    }

    fn spawn(workspace: &Path, lang: Language) -> std::result::Result<Arc<Self>, String> {
        let mut spawned: Option<(Child, &str)> = None;
        for &(bin, args) in lang.server_candidates() {
            let program =
                resolve_server_program(workspace, bin).unwrap_or_else(|| PathBuf::from(bin));
            let mut cmd = Command::new(&program);
            cmd.args(args)
                .current_dir(workspace)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            if let Ok(child) = cmd.spawn() {
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
        let stdin = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .ok_or_else(|| "LSP sem stdin".to_string())?,
        ));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "LSP sem stdout".to_string())?;
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Default::default();
        let diagnostics: DiagnosticsMap = Default::default();
        let p_read = pending.clone();
        let d_read = diagnostics.clone();
        let w_read = stdin.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(msg) = read_lsp_frame(&mut reader) {
                if let Some(resp) = server_request_response(&msg) {
                    if let Ok(mut w) = w_read.lock() {
                        let _ = write_lsp_frame(&mut *w, &resp);
                    }
                } else if let Some(id) = msg.get("id").and_then(Value::as_u64) {
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
                        if let Ok(mut d) = d_read.0.lock() {
                            d.insert(uri.to_string(), list.clone());
                            d_read.1.notify_all();
                        }
                    }
                }
            }
            d_read.1.notify_all();
        });
        let sess = Arc::new(Self {
            stdin,
            pending,
            diagnostics,
            pull_diagnostics: AtomicBool::new(false),
            open_docs: Mutex::new(HashMap::new()),
            last_used: Mutex::new(std::time::Instant::now()),
            next_id: AtomicU64::new(1),
            _child: Mutex::new(child),
        });
        let root_uri = file_uri(workspace);
        let init_res = sess.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": {},
                        "definition": { "linkSupport": false },
                        "implementation": { "linkSupport": false },
                        "references": {},
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "rename": { "prepareSupport": false },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    }
                }
            }),
            Duration::from_secs(10),
        )?;
        let supports_pull = init_res
            .get("capabilities")
            .and_then(|c| c.get("diagnosticProvider"))
            .is_some_and(|v| !v.is_null() && *v != Value::Bool(false));
        sess.pull_diagnostics
            .store(supports_pull, Ordering::Relaxed);
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
        self.touch();
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

    fn wait_diagnostics(&self, uri: &str, timeout: Duration) -> Option<Vec<Value>> {
        let (lock, cv) = &*self.diagnostics;
        let deadline = std::time::Instant::now() + timeout;
        let mut guard = lock.lock().ok()?;
        loop {
            if let Some(diags) = guard.get(uri) {
                return Some(diags.clone());
            }
            let rem = deadline.checked_duration_since(std::time::Instant::now())?;
            let (g, res) = cv.wait_timeout(guard, rem).ok()?;
            guard = g;
            if res.timed_out() {
                return guard.get(uri).cloned();
            }
        }
    }

    fn sync_doc(
        &self,
        path: &Path,
        lang: Language,
        emit_save_if_open: bool,
    ) -> std::result::Result<String, String> {
        self.touch();
        let uri = file_uri(path);
        let text =
            read_text_lossy(path).map_err(|e| format!("erro lendo {}: {e}", path.display()))?;
        let sync = {
            let mut docs = self
                .open_docs
                .lock()
                .map_err(|e| format!("LSP docs lock: {e}"))?;
            next_doc_sync(&mut docs, &uri)
        };
        match sync {
            DocSyncKind::Open(version) => {
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": lang.id_str(path),
                            "version": version,
                            "text": text,
                        }
                    }),
                )?;
            }
            DocSyncKind::Change(version) => {
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "version": version,
                        },
                        "contentChanges": [{ "text": text }],
                    }),
                )?;
                if emit_save_if_open {
                    self.notify(
                        "textDocument/didSave",
                        json!({
                            "textDocument": { "uri": uri },
                            "text": text,
                        }),
                    )?;
                }
            }
        }
        Ok(uri)
    }

    fn open_doc(&self, path: &Path, lang: Language) -> std::result::Result<String, String> {
        self.sync_doc(path, lang, false)
    }

    fn save_doc(&self, path: &Path, lang: Language) -> std::result::Result<String, String> {
        self.sync_doc(path, lang, true)
    }

    fn is_doc_open(&self, uri: &str) -> bool {
        self.open_docs
            .lock()
            .ok()
            .is_some_and(|docs| docs.contains_key(uri))
    }
}

fn read_text_lossy(path: &Path) -> io::Result<String> {
    let raw = std::fs::read(path)?;
    Ok(match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => e.into_bytes().into_iter().map(|b| b as char).collect(),
    })
}

type LspPool = Mutex<HashMap<(PathBuf, Language), Arc<LspSession>>>;

fn pool() -> &'static LspPool {
    static P: OnceLock<LspPool> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Closes all pooled Language Server child processes.
pub fn shutdown_all() {
    let drained: Vec<Arc<LspSession>> = pool()
        .lock()
        .map(|mut m| m.drain().map(|(_, s)| s).collect())
        .unwrap_or_default();
    drop(drained);
}

fn warm_session(workspace: &Path, path: &Path) -> Option<(Arc<LspSession>, Language)> {
    let lang = Language::detect(path)?;
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let m = pool().lock().ok()?;
    let s = m.get(&(ws, lang))?.clone();
    if !s.is_alive() {
        return None;
    }
    s.touch();
    Some((s, lang))
}

/// Notifies an already-warm Language Server of a save (`didChange` + `didSave`)
/// when that session already has `path` open, without blocking for diagnostics.
pub fn notify_save(workspace: &Path, path: &Path) {
    let Some((sess, lang)) = warm_session(workspace, path) else {
        return;
    };
    let uri = file_uri(path);
    if sess.is_doc_open(&uri) {
        let _ = sess.save_doc(path, lang);
    }
}

/// If a Language Server is ALREADY warm in the pool for `(workspace, language)`,
/// notifies it of the file edit (`didOpen`/`didChange` + `didSave` if already open)
/// and returns any non-empty diagnostics immediately without spawning a cold server process.
pub fn warm_diagnostics_for_edit(workspace: &Path, path: &Path) -> Option<String> {
    let (sess, lang) = warm_session(workspace, path)?;
    // What the server said about the old text is not about this one: drop it
    // before the change, so only a fresh publish can answer below.
    if let Ok(mut d) = sess.diagnostics.0.lock() {
        d.remove(&file_uri(path));
    }
    let uri = sess.save_doc(path, lang).ok()?;
    if sess.pull_diagnostics.load(Ordering::Relaxed) {
        if let Ok(res) = sess.request(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_millis(500),
        ) {
            if let Some(items) = res.get("items").and_then(Value::as_array) {
                if !items.is_empty() {
                    return Some(format_diagnostics(items));
                }
                return None;
            }
        }
    }
    // Push servers (rust-analyzer) publish when they finish: wait on the
    // Condvar for that publish with a 300ms bound.
    sess.wait_diagnostics(&uri, Duration::from_millis(300))
        .filter(|diags| !diags.is_empty())
        .map(|diags| format_diagnostics(&diags))
}

fn session_for(workspace: &Path, path: &Path) -> Result<(Arc<LspSession>, Language, String)> {
    let lang = Language::detect(path).ok_or_else(|| {
        ToolError::BadArguments(format!("extensão sem suporte LSP: {}", path.display()))
    })?;
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let key = (ws.clone(), lang);

    let (existing, evicted_stale) = {
        let mut stale = Vec::new();
        let mut reuse = None;
        if let Ok(mut m) = pool().lock() {
            let now = std::time::Instant::now();
            let dead_keys: Vec<(PathBuf, Language)> = m
                .iter()
                .filter_map(|(k, s)| {
                    let expired = now.duration_since(s.last_used_at()) > LSP_IDLE_TTL;
                    if !s.is_alive() || expired {
                        Some(k.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for dk in dead_keys {
                if let Some(old) = m.remove(&dk) {
                    stale.push(old);
                }
            }
            if let Some(s) = m.get(&key).cloned() {
                s.touch();
                reuse = Some(s);
            }
        }
        (reuse, stale)
    };
    drop(evicted_stale);

    let sess = match existing {
        Some(s) => s,
        None => {
            let s = LspSession::spawn(&ws, lang).map_err(ToolError::Failed)?;
            let evicted_lru: Vec<Arc<LspSession>> = if let Ok(mut m) = pool().lock() {
                let mut lru = Vec::new();
                while m.len() >= MAX_LSP_SESSIONS {
                    if let Some(oldest_key) = m
                        .iter()
                        .min_by_key(|(_, sess)| sess.last_used_at())
                        .map(|(k, _)| k.clone())
                    {
                        if let Some(old) = m.remove(&oldest_key) {
                            lru.push(old);
                        }
                    } else {
                        break;
                    }
                }
                m.insert(key, s.clone());
                lru
            } else {
                Vec::new()
            };
            drop(evicted_lru);
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

fn format_hover(val: &Value) -> String {
    if val.is_null() {
        return "nenhuma informação de hover encontrada".into();
    }
    let Some(contents) = val.get("contents") else {
        return "nenhuma informação de hover encontrada".into();
    };
    fn render_marked(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => {
                let t = s.trim();
                (!t.is_empty()).then(|| t.to_string())
            }
            Value::Object(map) => {
                let val = map.get("value").and_then(Value::as_str)?.trim();
                if val.is_empty() {
                    return None;
                }
                if let Some(lang) = map.get("language").and_then(Value::as_str) {
                    if !lang.trim().is_empty() {
                        return Some(format!("```{lang}\n{val}\n```"));
                    }
                }
                Some(val.to_string())
            }
            _ => None,
        }
    }
    let rendered = match contents {
        Value::Array(arr) => arr
            .iter()
            .filter_map(render_marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => render_marked(other).unwrap_or_default(),
    };
    if rendered.is_empty() {
        "nenhuma informação de hover encontrada".into()
    } else {
        rendered
    }
}

fn format_workspace_symbols(arr: &[Value], workspace: &Path) -> String {
    if arr.is_empty() {
        return "nenhum símbolo encontrado".into();
    }
    arr.iter()
        .take(120)
        .map(|s| {
            let name = s["name"].as_str().unwrap_or("");
            let uri = s["location"]["uri"].as_str().unwrap_or("");
            let path = if uri.is_empty() {
                String::new()
            } else {
                uri_to_display_path(uri, workspace)
            };
            let line = s["location"]["range"]["start"]["line"]
                .as_u64()
                .or_else(|| s["range"]["start"]["line"].as_u64())
                .map(|l| l + 1);
            match (path.is_empty(), line) {
                (false, Some(l)) => format!("{path}:{l} {name}"),
                (false, None) => format!("{path} {name}"),
                (true, Some(l)) => format!("L{l} {name}"),
                (true, None) => name.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_document_symbols(items: &[Value], depth: usize, out: &mut Vec<String>, limit: usize) {
    let indent = "  ".repeat(depth);
    for s in items {
        if out.len() >= limit {
            break;
        }
        let name = s["name"].as_str().unwrap_or("");
        let line = s["selectionRange"]["start"]["line"]
            .as_u64()
            .or_else(|| s["range"]["start"]["line"].as_u64())
            .or_else(|| s["location"]["range"]["start"]["line"].as_u64())
            .unwrap_or(0)
            + 1;
        out.push(format!("{indent}L{line} {name}"));
        if let Some(children) = s.get("children").and_then(Value::as_array) {
            collect_document_symbols(children, depth + 1, out, limit);
        }
    }
}

fn format_document_symbols(arr: &[Value]) -> String {
    if arr.is_empty() {
        return "nenhum símbolo encontrado".into();
    }
    let mut out = Vec::new();
    collect_document_symbols(arr, 0, &mut out, 200);
    out.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LspTextEdit {
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    new_text: String,
}

fn lsp_pos_to_byte(text: &str, line: usize, character_utf16: usize) -> usize {
    let mut line_starts = vec![0usize];
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(idx + 1);
        }
    }
    if line >= line_starts.len() {
        return text.len();
    }
    let start = line_starts[line];
    let raw_end = line_starts
        .get(line + 1)
        .map_or(text.len(), |&next| next.saturating_sub(1));
    let line_slice = text[start..raw_end].trim_end_matches('\r');
    let mut utf16 = 0usize;
    for (off, ch) in line_slice.char_indices() {
        if utf16 >= character_utf16 {
            return start + off;
        }
        utf16 += ch.len_utf16();
    }
    start + line_slice.len()
}

fn apply_text_edits(text: &str, edits: &[LspTextEdit]) -> Result<String> {
    let mut spans: Vec<(usize, usize, usize, &str)> = edits
        .iter()
        .enumerate()
        .map(|(idx, e)| {
            let s = lsp_pos_to_byte(text, e.start_line, e.start_char);
            let en = lsp_pos_to_byte(text, e.end_line, e.end_char);
            if s > en {
                return Err(ToolError::Failed(format!(
                    "LSP retornou range invertido ({s} > {en}) em rename"
                )));
            }
            Ok((s, en, idx, e.new_text.as_str()))
        })
        .collect::<Result<Vec<_>>>()?;
    spans.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
    });
    let mut prev_start = usize::MAX;
    for &(s, en, _, _) in &spans {
        if en > prev_start {
            return Err(ToolError::Failed(
                "LSP retornou edições sobrepostas em rename; nada foi gravado".into(),
            ));
        }
        prev_start = s;
    }
    let mut out = text.to_string();
    for (s, en, _, rep) in spans {
        out.replace_range(s..en, rep);
    }
    Ok(out)
}

fn parse_text_edit(v: &Value) -> Option<LspTextEdit> {
    let range = v.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(LspTextEdit {
        start_line: start.get("line")?.as_u64()? as usize,
        start_char: start.get("character")?.as_u64()? as usize,
        end_line: end.get("line")?.as_u64()? as usize,
        end_char: end.get("character")?.as_u64()? as usize,
        new_text: v.get("newText")?.as_str()?.to_string(),
    })
}

fn parse_workspace_edit(val: &Value) -> Result<BTreeMap<String, Vec<LspTextEdit>>> {
    let mut by_uri: BTreeMap<String, Vec<LspTextEdit>> = BTreeMap::new();
    if val.is_null() {
        return Ok(by_uri);
    }
    if let Some(doc_changes) = val.get("documentChanges").and_then(Value::as_array) {
        for item in doc_changes {
            if let Some(kind) = item.get("kind").and_then(Value::as_str) {
                return Err(ToolError::Failed(format!(
                    "rename retornou operação de arquivo `{kind}` não suportada automaticamente; nada foi alterado"
                )));
            }
            let uri = item["textDocument"]["uri"]
                .as_str()
                .ok_or_else(|| ToolError::Failed("WorkspaceEdit sem textDocument.uri".into()))?;
            let edits = item["edits"]
                .as_array()
                .ok_or_else(|| ToolError::Failed("WorkspaceEdit sem edits".into()))?;
            let entry = by_uri.entry(uri.to_string()).or_default();
            for e in edits {
                let parsed = parse_text_edit(e).ok_or_else(|| {
                    ToolError::Failed("TextEdit inválido no WorkspaceEdit".into())
                })?;
                entry.push(parsed);
            }
        }
    } else if let Some(changes) = val.get("changes").and_then(Value::as_object) {
        for (uri, edits_val) in changes {
            let edits = edits_val
                .as_array()
                .ok_or_else(|| ToolError::Failed("WorkspaceEdit.changes inválido".into()))?;
            let entry = by_uri.entry(uri.clone()).or_default();
            for e in edits {
                let parsed = parse_text_edit(e).ok_or_else(|| {
                    ToolError::Failed("TextEdit inválido no WorkspaceEdit".into())
                })?;
                entry.push(parsed);
            }
        }
    }
    Ok(by_uri)
}

struct LspTool {
    spec: ToolSpec,
}

impl Tool for LspTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        lsp_action(args, ctx)
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        run_lsp(args, ctx, exec)
    }

    fn concurrency_safe(&self, args: &Value) -> bool {
        args["action"].as_str() != Some("rename")
    }
}

// Uma ferramenta com `action` no lugar de lsp_diagnostics/definition/
// references/symbols, que seguem aceitos como apelido (zerith_core::tool_alias).
pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![Box::new(LspTool {
        spec: ToolSpec {
            name: "lsp".into(),
            description: "Consulta o Language Server de um arquivo. diagnostics: erros e avisos de tipo/compilação. definition: onde o símbolo na posição foi definido. implementation: implementações do símbolo. references: todos os usos dele. hover: tipo e documentação na posição. symbols: símbolos do arquivo (incluindo aninhados), ou do workspace com `query` (`path` indica a linguagem). rename: renomeia o símbolo em todo o workspace usando `new_name`.".into(),
            parameters: json!({"type": "object", "properties": {
                "action": {"type": "string", "enum": ["diagnostics", "definition", "references", "symbols", "hover", "implementation", "rename"]},
                "path": {"type": "string"},
                "line": {"type": "integer", "description": "definition/references/hover/implementation/rename: linha, 1-based"},
                "character": {"type": "integer", "description": "definition/references/hover/implementation/rename: coluna, 0-based"},
                "query": {"type": "string", "description": "symbols: busca no workspace"},
                "new_name": {"type": "string", "description": "rename: novo nome do símbolo"}
            }, "required": ["action", "path"]}),
            returns: "diagnostics por linha/coluna, localizações URI:linha:coluna, hover, símbolos com linha ou resumo do rename",
            permission: Permission::Read,
            timeout: Duration::from_secs(15),
            cancellable: true,
            local: true,
            source: "built-in".into(),
        },
    })]
}

fn run_lsp(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    match args["action"].as_str().unwrap_or("") {
        "diagnostics" => run_lsp_diagnostics(args, ctx, exec),
        "definition" => run_lsp_definition(args, ctx, exec),
        "references" => run_lsp_references(args, ctx, exec),
        "symbols" => run_lsp_symbols(args, ctx, exec),
        "hover" => run_lsp_hover(args, ctx, exec),
        "implementation" => run_lsp_implementation(args, ctx, exec),
        "rename" => run_lsp_rename(args, ctx, exec),
        other => Err(ToolError::BadArguments(format!(
            "action precisa ser diagnostics, definition, references, symbols, hover, implementation ou rename, não {other:?}"
        ))),
    }
}

fn lsp_action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    let path = ctx.resolve(str_arg(args, "path")?);
    if args["action"].as_str() == Some("rename") {
        Ok(Some(Action::Write(path)))
    } else {
        Ok(Some(Action::Read(path)))
    }
}

fn run_lsp_diagnostics(args: &Value, ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    // Use pull diagnostics if the server advertised diagnosticProvider; otherwise wait for publishDiagnostics.
    if sess.pull_diagnostics.load(Ordering::Relaxed) {
        if let Ok(res) = sess.request(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(3),
        ) {
            if let Some(items) = res.get("items").and_then(Value::as_array) {
                return Ok(format_diagnostics(items));
            }
        }
    }
    let diags = sess
        .wait_diagnostics(&uri, Duration::from_millis(600))
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
    let query = opt_str(args, "query");
    let res = if let Some(q) = query {
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
    if query.is_some() {
        Ok(format_workspace_symbols(arr, ctx.workspace))
    } else {
        Ok(format_document_symbols(arr))
    }
}

fn run_lsp_hover(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (line, character) = position_args(args)?;
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = sess
        .request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
            Duration::from_secs(8),
        )
        .map_err(ToolError::Failed)?;
    Ok(format_hover(&res))
}

fn run_lsp_implementation(args: &Value, ctx: &CallCtx<'_>, _: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (line, character) = position_args(args)?;
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = sess
        .request(
            "textDocument/implementation",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
            Duration::from_secs(8),
        )
        .map_err(ToolError::Failed)?;
    Ok(format_locations(&res))
}

fn check_rename_target_policy(
    policy: &zerith_security::Policy,
    primary_path: &Path,
    target_path: &Path,
) -> Result<()> {
    match policy.check_tool("lsp", &Action::Write(target_path.to_path_buf())) {
        Verdict::Allow => Ok(()),
        Verdict::Deny(why) => Err(ToolError::Denied(format!(
            "{}: {why}",
            target_path.display()
        ))),
        Verdict::Ask(why) => {
            let crosses_boundary =
                !policy.inside_workspace(target_path) || policy.is_own_control(target_path);
            let same_as_primary = std::fs::canonicalize(primary_path)
                .ok()
                .zip(std::fs::canonicalize(target_path).ok())
                .map_or(primary_path == target_path, |(a, b)| a == b);
            if crosses_boundary && !same_as_primary {
                Err(ToolError::Denied(format!(
                    "{}: {why}",
                    target_path.display()
                )))
            } else {
                Ok(())
            }
        }
    }
}

fn run_lsp_rename(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let path = ctx.resolve(str_arg(args, "path")?);
    let (line, character) = position_args(args)?;
    let new_name = str_arg(args, "new_name")?;
    let (sess, _, uri) = session_for(ctx.workspace, &path)?;
    let res = sess
        .request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "newName": new_name
            }),
            Duration::from_secs(10),
        )
        .map_err(ToolError::Failed)?;
    let edits_by_uri = parse_workspace_edit(&res)?;
    if edits_by_uri.is_empty() {
        return Ok("nenhuma alteração retornada pelo LSP para rename".into());
    }

    // Plan and validate every target file before touching disk.
    struct PlannedWrite {
        path: PathBuf,
        display: String,
        bytes: Vec<u8>,
        edit_count: usize,
    }
    let mut planned = Vec::with_capacity(edits_by_uri.len());
    for (target_uri, edits) in &edits_by_uri {
        let target_path = uri_to_path(target_uri).ok_or_else(|| {
            ToolError::Failed(format!(
                "URI não suportada em rename: {target_uri}; nada foi alterado"
            ))
        })?;
        check_rename_target_policy(ctx.policy, &path, &target_path)?;
        let raw = std::fs::read(&target_path).map_err(|e| {
            ToolError::Failed(format!("{}: {e}; nada foi alterado", target_path.display()))
        })?;
        let (text, is_latin1) = match String::from_utf8(raw) {
            Ok(s) => (s, false),
            Err(e) => (
                e.into_bytes().into_iter().map(|b| b as char).collect(),
                true,
            ),
        };
        let new_text = apply_text_edits(&text, edits)?;
        let out_bytes = if is_latin1 {
            let mut bytes = Vec::with_capacity(new_text.len());
            for ch in new_text.chars() {
                let b = u8::try_from(ch as u32).map_err(|_| {
                    ToolError::Failed(format!(
                        "{}: caractere fora de Latin-1 em rename; nada foi alterado",
                        target_path.display()
                    ))
                })?;
                bytes.push(b);
            }
            bytes
        } else {
            new_text.into_bytes()
        };
        crate::lint::check(&target_path, &out_bytes)?;
        planned.push(PlannedWrite {
            display: uri_to_display_path(target_uri, ctx.workspace),
            path: target_path,
            bytes: out_bytes,
            edit_count: edits.len(),
        });
    }

    exec.host.snapshot_workspace(ctx.workspace);
    for item in &planned {
        if item.path.exists() {
            exec.host.before_write(&item.path)?;
        }
    }

    let mut total_edits = 0usize;
    let mut summary = Vec::with_capacity(planned.len());
    let mut checked = Vec::with_capacity(planned.len());
    for item in planned {
        let pc = crate::postcheck::before(ctx.workspace, &item.path);
        crate::builtin::write_atomic(&item.path, &item.bytes)?;
        exec.host.note_read(&item.path);
        notify_save(ctx.workspace, &item.path);
        total_edits += item.edit_count;
        summary.push(format!(
            "  M {} ({} edição(ões))",
            item.display, item.edit_count
        ));
        checked.push((item.path, pc));
    }

    let mut out = format!(
        "rename `{new_name}` aplicado em {} arquivo(s) ({total_edits} edição(ões)):\n{}",
        summary.len(),
        summary.join("\n")
    );
    for (p, pc) in checked {
        let block = crate::postcheck::after(&p, pc);
        if !block.is_empty() {
            out.push_str(&format!("\n\n{}:\n{block}", p.display()));
        }
    }
    Ok(out)
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

    #[test]
    fn doc_sync_tracks_did_open_then_increments_for_did_change() {
        let mut docs = HashMap::new();
        assert_eq!(
            next_doc_sync(&mut docs, "file:///a.rs"),
            DocSyncKind::Open(1)
        );
        assert_eq!(
            next_doc_sync(&mut docs, "file:///a.rs"),
            DocSyncKind::Change(2)
        );
        assert_eq!(
            next_doc_sync(&mut docs, "file:///a.rs"),
            DocSyncKind::Change(3)
        );
        assert_eq!(
            next_doc_sync(&mut docs, "file:///b.rs"),
            DocSyncKind::Open(1)
        );
    }

    #[test]
    fn language_id_str_maps_variants_accurately() {
        let cases = [
            ("a.js", Language::TypeScript, "javascript"),
            ("a.mjs", Language::TypeScript, "javascript"),
            ("a.cjs", Language::TypeScript, "javascript"),
            ("a.jsx", Language::TypeScript, "javascriptreact"),
            ("a.ts", Language::TypeScript, "typescript"),
            ("a.mts", Language::TypeScript, "typescript"),
            ("a.cts", Language::TypeScript, "typescript"),
            ("a.tsx", Language::TypeScript, "typescriptreact"),
            ("a.c", Language::Cpp, "c"),
            ("a.h", Language::Cpp, "c"),
            ("a.cpp", Language::Cpp, "cpp"),
            ("a.hpp", Language::Cpp, "cpp"),
            ("a.cc", Language::Cpp, "cpp"),
            ("a.cxx", Language::Cpp, "cpp"),
            ("a.rs", Language::Rust, "rust"),
            ("a.py", Language::Python, "python"),
            ("a.go", Language::Go, "go"),
        ];
        for (file, expected_lang, expected_id) in cases {
            let p = Path::new(file);
            let detected = Language::detect(p).expect(file);
            assert_eq!(detected, expected_lang, "{file}");
            assert_eq!(detected.id_str(p), expected_id, "{file}");
        }
    }

    #[test]
    fn server_requests_get_valid_jsonrpc_responses() {
        assert_eq!(
            server_request_response(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "workspace/configuration",
                "params": {}
            })),
            Some(json!({"jsonrpc": "2.0", "id": 1, "result": []}))
        );
        assert_eq!(
            server_request_response(&json!({
                "jsonrpc": "2.0",
                "id": "cfg-2",
                "method": "workspace/configuration",
                "params": {"items": [{"section": "python"}, {"section": "ts"}]}
            })),
            Some(json!({"jsonrpc": "2.0", "id": "cfg-2", "result": [null, null]}))
        );
        assert_eq!(
            server_request_response(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "client/registerCapability",
                "params": {}
            })),
            Some(json!({"jsonrpc": "2.0", "id": 3, "result": null}))
        );
        assert_eq!(
            server_request_response(&json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "window/workDoneProgress/create",
                "params": {"token": "1"}
            })),
            Some(json!({"jsonrpc": "2.0", "id": 4, "result": null}))
        );
        assert_eq!(
            server_request_response(&json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "workspace/applyEdit",
                "params": {}
            })),
            Some(json!({"jsonrpc": "2.0", "id": 5, "result": {"applied": false}}))
        );
        // Client response (no method) and server notification (no id) are ignored here.
        assert_eq!(
            server_request_response(&json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            None
        );
        assert_eq!(
            server_request_response(
                &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {}})
            ),
            None
        );
    }

    #[test]
    fn workspace_and_document_symbols_include_paths_and_nested_children() {
        let ws = Path::new("C:/repo");
        let ws_syms = format_workspace_symbols(
            &[
                json!({
                    "name": "Router",
                    "location": {
                        "uri": "file:///C%3A/repo/src/net%20mod/router.rs",
                        "range": {"start": {"line": 41, "character": 0}}
                    }
                }),
                json!({
                    "name": "External",
                    "location": {
                        "uri": "file:///D:/other/lib.rs"
                    }
                }),
            ],
            ws,
        );
        assert_eq!(
            ws_syms,
            "src/net mod/router.rs:42 Router\nD:/other/lib.rs External"
        );

        let doc_syms = format_document_symbols(&[json!({
            "name": "Config",
            "range": {"start": {"line": 2, "character": 0}},
            "selectionRange": {"start": {"line": 2, "character": 11}},
            "children": [
                {
                    "name": "port",
                    "range": {"start": {"line": 3, "character": 4}}
                },
                {
                    "name": "load",
                    "range": {"start": {"line": 6, "character": 4}},
                    "children": [
                        {
                            "name": "inner_helper",
                            "range": {"start": {"line": 8, "character": 8}}
                        }
                    ]
                }
            ]
        })]);
        assert_eq!(
            doc_syms,
            "L3 Config\n  L4 port\n  L7 load\n    L9 inner_helper"
        );
    }

    #[test]
    fn hover_formatting_and_rename_text_edits_work() {
        assert_eq!(
            format_hover(
                &json!({"contents": {"kind": "markdown", "value": "```rust\nfn foo()\n```"}})
            ),
            "```rust\nfn foo()\n```"
        );
        assert_eq!(
            format_hover(
                &json!({"contents": [{"language": "ts", "value": "const x: number"}, "Doc text"]})
            ),
            "```ts\nconst x: number\n```\n\nDoc text"
        );
        assert_eq!(
            format_hover(&Value::Null),
            "nenhuma informação de hover encontrada"
        );

        // UTF-16 offset: '🦀' takes 2 UTF-16 code units.
        let src = "let 🦀 = old_name;\r\nreturn old_name + 1;\r\n";
        let we = json!({
            "documentChanges": [{
                "textDocument": {"uri": "file:///repo/a.rs", "version": 1},
                "edits": [
                    {"range": {"start": {"line": 0, "character": 9}, "end": {"line": 0, "character": 17}}, "newText": "new_name"},
                    {"range": {"start": {"line": 1, "character": 7}, "end": {"line": 1, "character": 15}}, "newText": "new_name"}
                ]
            }]
        });
        let map = parse_workspace_edit(&we).unwrap();
        let edited = apply_text_edits(src, &map["file:///repo/a.rs"]).unwrap();
        assert_eq!(edited, "let 🦀 = new_name;\r\nreturn new_name + 1;\r\n");

        // Overlapping edits are rejected cleanly.
        let overlap = apply_text_edits(
            "abcdef",
            &[
                LspTextEdit {
                    start_line: 0,
                    start_char: 1,
                    end_line: 0,
                    end_char: 4,
                    new_text: "X".into(),
                },
                LspTextEdit {
                    start_line: 0,
                    start_char: 2,
                    end_line: 0,
                    end_char: 5,
                    new_text: "Y".into(),
                },
            ],
        );
        assert!(overlap.is_err());

        // Tool metadata: schema & concurrency_safe
        let tool = all().into_iter().next().unwrap();
        let actions = tool.spec().parameters["properties"]["action"]["enum"]
            .as_array()
            .unwrap();
        for needed in ["hover", "implementation", "rename"] {
            assert!(
                actions.iter().any(|v| v.as_str() == Some(needed)),
                "missing action {needed}"
            );
        }
        assert!(tool.concurrency_safe(&json!({"action": "hover"})));
        assert!(tool.concurrency_safe(&json!({"action": "implementation"})));
        assert!(!tool.concurrency_safe(&json!({"action": "rename"})));
    }

    #[test]
    fn rename_policy_allows_approved_in_workspace_and_blocks_unapproved_boundary() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let primary = dir.path().join("src_a.rs");
        let sibling = dir.path().join("src_b.rs");
        let outside = scratch.join("outside_rename_target.rs");
        let control = dir.path().join(".zerith").join("settings.toml");
        std::fs::write(&primary, "fn a() {}\n").unwrap();
        std::fs::write(&sibling, "fn b() {}\n").unwrap();

        let pol = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            dir.path(),
        );
        // After user approves Mode::Default rename on `primary`, in-workspace files proceed.
        assert!(check_rename_target_policy(&pol, &primary, &primary).is_ok());
        assert!(check_rename_target_policy(&pol, &primary, &sibling).is_ok());
        // Unapproved boundary targets (outside workspace or .zerith/) are still blocked.
        assert!(check_rename_target_policy(&pol, &primary, &outside).is_err());
        assert!(check_rename_target_policy(&pol, &primary, &control).is_err());
        // Plan mode (Deny) blocks even the primary path.
        let pol_plan = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Plan,
            dir.path(),
        );
        assert!(check_rename_target_policy(&pol_plan, &primary, &primary).is_err());
    }

    #[test]
    fn resolve_server_program_finds_local_node_modules_shim_in_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules").join(".bin");
        let sub = dir.path().join("apps").join("web");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let shim_name = if cfg!(windows) {
            "fake-ts-lsp.cmd"
        } else {
            "fake-ts-lsp"
        };
        let shim = bin_dir.join(shim_name);
        std::fs::write(&shim, "").unwrap();
        let found = resolve_server_program(&sub, "fake-ts-lsp");
        assert_eq!(found.as_deref(), Some(shim.as_path()));
    }
}
