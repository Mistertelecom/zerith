//! Telegram Bot API gateway (`zerith gateway telegram setup|run`) and
//! outbound notifications used by the cron daemon (`send_telegram_alert`).
//!
//! Security invariants:
//! - Bot token is stored exclusively in the OS keyring (`telegram_bot_token`),
//!   never written to `settings.toml` and scrubbed from any returned errors.
//! - Inbound messages and inline-button callbacks fail closed unless the
//!   sender/chat ID appears in `settings.gateway.telegram.allowed_chats`.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use zerith_core::{secrets, Paths, Settings};
use zerith_mcp::McpRegistry;
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::{Cancel, Client};
use zerith_runtime::guards::Guards;
use zerith_runtime::{
    approval_channel_with_grants, run_agent_turn, AgentOptions, Approval, ApprovalSender, RunEvent,
    SharedGrants, TurnContext,
};
use zerith_security::Policy;
use zerith_sessions::{SessionSource, Store};
use zerith_skills::Library;
use zerith_tools::Registry;

use crate::headless::{load_settings, provider_config, resolve_model};
use crate::signals;

pub const TELEGRAM_SECRET_KEY: &str = "telegram_bot_token";
const TELEGRAM_MAX_MSG: usize = 4096;

/// Fail-closed allowlist check: an empty allowlist rejects every chat.
pub fn is_chat_allowed(allowed: &[i64], chat_id: i64) -> bool {
    !allowed.is_empty() && allowed.contains(&chat_id)
}

/// Parses inline-keyboard callback data in the form `"approve:<id>:once|session|deny"`.
pub fn parse_callback_decision(data: &str) -> Option<(u64, Approval)> {
    let mut parts = data.trim().split(':');
    if parts.next()? != "approve" {
        return None;
    }
    let id: u64 = parts.next()?.parse().ok()?;
    let decision = match parts.next()? {
        "once" => Approval::AllowOnce,
        "session" => Approval::AllowSession,
        "deny" => Approval::Deny,
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((id, decision))
}

/// Splits `text` into UTF-8 chunks each having at most `max_chars` Unicode
/// scalar values, preferring newline or whitespace boundaries.
pub fn chunk_telegram_text(text: &str, max_chars: usize) -> Vec<String> {
    let cap = max_chars.max(1);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec!["(sem conteúdo)".to_string()];
    }
    let mut out = Vec::new();
    let mut rest = trimmed;
    while !rest.is_empty() {
        if rest.chars().count() <= cap {
            out.push(rest.to_string());
            break;
        }
        let mut split_byte = rest.len();
        let mut last_nl = None;
        let mut last_ws = None;
        for (count, (idx, ch)) in rest.char_indices().enumerate() {
            if count == cap {
                split_byte = idx;
                break;
            }
            if ch == '\n' {
                last_nl = Some(idx);
            } else if ch.is_whitespace() {
                last_ws = Some(idx);
            }
        }
        let cut = last_nl
            .filter(|&i| i > 0)
            .or_else(|| last_ws.filter(|&i| i > 0))
            .unwrap_or(split_byte);
        let head = rest[..cut].trim_end();
        if !head.is_empty() {
            out.push(head.to_string());
        }
        rest = rest[cut..].trim_start();
    }
    if out.is_empty() {
        out.push("(sem conteúdo)".to_string());
    }
    out
}

fn sanitize_err(token: &str, msg: impl std::fmt::Display) -> String {
    let s = msg.to_string();
    if token.is_empty() {
        s
    } else {
        s.replace(token, "<redacted>")
    }
}

fn api_url(token: &str, method: &str) -> String {
    format!("https://api.telegram.org/bot{token}/{method}")
}

fn post_json(
    agent: &ureq::Agent,
    token: &str,
    method: &str,
    body: &Value,
) -> std::result::Result<Value, String> {
    let resp = agent.post(&api_url(token, method)).send_json(body.clone());
    parse_reply(token, method, resp)
}

fn parse_reply(
    token: &str,
    method: &str,
    resp: std::result::Result<ureq::Response, ureq::Error>,
) -> std::result::Result<Value, String> {
    let resp = resp.map_err(|e| format!("Telegram {method} falhou: {}", sanitize_err(token, e)))?;
    let val: Value = resp.into_json().map_err(|e| {
        format!(
            "Telegram {method} JSON inválido: {}",
            sanitize_err(token, e)
        )
    })?;
    if !val["ok"].as_bool().unwrap_or(false) {
        let desc = val["description"].as_str().unwrap_or("erro desconhecido");
        return Err(format!(
            "Telegram {method} recusado: {}",
            sanitize_err(token, desc)
        ));
    }
    Ok(val["result"].clone())
}

fn send_message(
    agent: &ureq::Agent,
    token: &str,
    chat_id: i64,
    text: &str,
) -> std::result::Result<(), String> {
    for chunk in chunk_telegram_text(text, TELEGRAM_MAX_MSG) {
        let _ = post_json(
            agent,
            token,
            "sendMessage",
            &json!({
                "chat_id": chat_id,
                "text": chunk,
                "disable_web_page_preview": true
            }),
        )?;
    }
    Ok(())
}

// Envio de arquivos. Ideia do gateway do Hermes (MIT): a resposta traz linhas
// `MEDIA:<caminho>` que o gateway tira do texto e entrega como anexo nativo
// (`gateway/platforms/base.py::extract_media`, `gateway/media_policy.py`).

/// Limite da Bot API para upload (sendDocument/sendVideo).
const TELEGRAM_MAX_UPLOAD: u64 = 50 * 1024 * 1024;
/// sendPhoto recusa acima disso; a imagem vai como documento.
const TELEGRAM_MAX_PHOTO: u64 = 10 * 1024 * 1024;

/// Lembrete do turno (não entra no prompt de sistema global).
const TELEGRAM_MEDIA_HINT: &str = "Você está respondendo pelo Telegram. Para mandar um arquivo ao \
usuário, escreva numa linha própria MEDIA:<caminho> (absoluto ou relativo ao workspace); a linha \
some do texto e o arquivo chega como anexo.";

/// Tira as linhas `MEDIA:<caminho>` da resposta. Devolve os caminhos (sem
/// repetição, na ordem) e o texto restante. Linhas dentro de bloco de código
/// cercado são exemplos e ficam no texto.
pub fn extract_media(text: &str) -> (Vec<String>, String) {
    let mut paths: Vec<String> = Vec::new();
    let mut kept = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
        }
        let raw = if in_fence {
            None
        } else {
            t.strip_prefix("MEDIA:")
        }
        .map(|p| p.trim().trim_matches(|c| "`\"'<>".contains(c)).trim())
        .filter(|p| !p.is_empty());
        match raw {
            Some(p) => {
                if !paths.iter().any(|x| x == p) {
                    paths.push(p.to_string());
                }
            }
            None => kept.push(line),
        }
    }
    (paths, kept.join("\n").trim().to_string())
}

/// Arquivos que nunca saem, mesmo dentro do workspace: segredos e chaves.
fn is_sensitive(path: &Path) -> bool {
    let in_ssh = path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".ssh"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    in_ssh
        || name == ".env"
        || name.starts_with(".env.")
        || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .any(|k| name.starts_with(k))
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

/// Valida um caminho de `MEDIA:`: o destino real (links simbólicos
/// resolvidos) tem que ser um arquivo dentro do workspace ou da pasta de mídia
/// gerada, não sensível e até 50 MB. Devolve o caminho canônico e o tamanho.
fn resolve_media_path(
    raw: &str,
    workspace: &Path,
    media_root: &Path,
) -> std::result::Result<(PathBuf, u64), String> {
    let given = Path::new(raw);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        workspace.join(given)
    };
    if is_sensitive(&joined) {
        return Err("arquivo sensível, não envio".into());
    }
    let real = joined
        .canonicalize()
        .map_err(|e| format!("não encontrei o arquivo ({e})"))?;
    let inside = [workspace, media_root]
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| real.starts_with(r));
    if !inside {
        return Err("fora do workspace e da pasta de mídia".into());
    }
    if is_sensitive(&real) {
        return Err("arquivo sensível, não envio".into());
    }
    let meta = real.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("não é um arquivo".into());
    }
    if meta.len() > TELEGRAM_MAX_UPLOAD {
        return Err(format!(
            "{} MB passa do limite de 50 MB do Telegram",
            meta.len().div_ceil(1024 * 1024)
        ));
    }
    Ok((real, meta.len()))
}

/// Método e campo da Bot API para o arquivo, pela extensão e tamanho.
fn upload_method(path: &Path, size: u64) -> (&'static str, &'static str) {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" if size <= TELEGRAM_MAX_PHOTO => {
            ("sendPhoto", "photo")
        }
        "mp4" | "mov" | "webm" => ("sendVideo", "video"),
        _ => ("sendDocument", "document"),
    }
}

/// Caminhos de `Arquivo: <caminho> (<tipo>, <n> KB)` no resultado de
/// generate_image/generate_video (`zerith_tools::media::report`).
fn generated_files(tool_output: &str) -> Vec<String> {
    tool_output
        .lines()
        .filter_map(|l| l.strip_prefix("Arquivo: "))
        .filter_map(|l| l.rsplit_once(" (").map(|(p, _)| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect()
}

/// multipart/form-data à mão (ureq 2 não tem): `chat_id` e o arquivo.
fn send_file(
    agent: &ureq::Agent,
    token: &str,
    chat_id: i64,
    path: &Path,
    size: u64,
) -> std::result::Result<(), String> {
    let (method, field) = upload_method(path, size);
    let bytes = std::fs::read(path).map_err(|e| format!("não consegui ler: {e}"))?;
    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arquivo".into())
        .chars()
        .map(|c| if "\"\r\n\\".contains(c) { '_' } else { c })
        .collect();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut boundary = format!("zerith-{seed:x}");
    while bytes
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary.push('x');
    }
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"chat_id\"\r\n\r\n{chat_id}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = agent
        .post(&api_url(token, method))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send_bytes(&body);
    parse_reply(token, method, resp).map(|_| ())
}

/// Manda cada arquivo; o que falhar vira uma mensagem curta com o motivo.
fn deliver_media(token: &str, chat_id: i64, paths: &[String], workspace: &Path, media_root: &Path) {
    if paths.is_empty() {
        return;
    }
    // Upload de até 50 MB não cabe no prazo de 15 s do agente de mensagens.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .build();
    let mut sent: Vec<PathBuf> = Vec::new();
    for raw in paths {
        let res = resolve_media_path(raw, workspace, media_root).and_then(|(real, size)| {
            if sent.contains(&real) {
                return Ok(());
            }
            send_file(&agent, token, chat_id, &real, size)?;
            sent.push(real);
            Ok(())
        });
        if let Err(e) = res {
            let msg = format!("Não enviei {raw}: {}", sanitize_err(token, e));
            let _ = send_message(&agent, token, chat_id, &msg);
        }
    }
}

fn send_approval_prompt(
    agent: &ureq::Agent,
    token: &str,
    chat_id: i64,
    approval_id: u64,
    tool: &str,
    summary: &str,
) -> std::result::Result<(), String> {
    let preview: String = summary.chars().take(1200).collect();
    let msg = format!("Aprovação necessária (#{approval_id})\nFerramenta: {tool}\n\n{preview}");
    let _ = post_json(
        agent,
        token,
        "sendMessage",
        &json!({
            "chat_id": chat_id,
            "text": msg,
            "reply_markup": {
                "inline_keyboard": [[
                    {"text": "Permitir 1x", "callback_data": format!("approve:{approval_id}:once")},
                    {"text": "Permitir sessão", "callback_data": format!("approve:{approval_id}:session")},
                    {"text": "Negar", "callback_data": format!("approve:{approval_id}:deny")}
                ]]
            }
        }),
    )?;
    Ok(())
}

/// Sends an alert message to every chat in `settings.gateway.telegram.allowed_chats`
/// using the bot token stored in the OS keyring (`telegram_bot_token`).
pub fn send_telegram_alert(settings: &Settings, text: &str) -> std::result::Result<(), String> {
    if settings.gateway.telegram.allowed_chats.is_empty() {
        return Err("nenhum chat autorizado em gateway.telegram.allowed_chats".into());
    }
    let token = secrets::get_secret(TELEGRAM_SECRET_KEY)
        .map_err(|e| format!("token Telegram indisponível: {e}"))?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let mut errs = Vec::new();
    for &chat_id in &settings.gateway.telegram.allowed_chats {
        if let Err(e) = send_message(&agent, token.trim(), chat_id, text) {
            errs.push(format!("chat {chat_id}: {e}"));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

pub fn cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // Accept both `zerith gateway telegram setup|run ...` and `zerith gateway setup|run ...`.
    let slice = if args.get(1).map(String::as_str) == Some("telegram") {
        &args[1..]
    } else {
        args
    };
    match slice.get(1).map(String::as_str) {
        Some("setup") => setup_cmd(paths, &slice[2..]),
        Some("run") | None => run_cmd(paths, slice.get(2..).unwrap_or(&[])),
        Some(other) => Err(format!(
            "subcomando gateway desconhecido: {other} (use `zerith gateway telegram setup` ou `run`)"
        )
        .into()),
    }
}

fn setup_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut token_arg: Option<String> = None;
    let mut chats_arg: Option<String> = None;
    let mut model_arg: Option<String> = None;
    let mut workspace_arg: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{a} precisa de um valor"))
        };
        match a {
            "--token" => {
                token_arg = Some(next()?);
                i += 2;
            }
            "--chats" => {
                chats_arg = Some(next()?);
                i += 2;
            }
            "--model" => {
                model_arg = Some(next()?);
                i += 2;
            }
            "--workspace" | "-C" => {
                workspace_arg = Some(next()?);
                i += 2;
            }
            other => return Err(format!("opção desconhecida em gateway setup: {other}").into()),
        }
    }

    let raw_token = token_arg.ok_or("--token <BOT_TOKEN | -> é obrigatório")?;
    let token = if raw_token == "-" {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        line.trim().to_string()
    } else {
        raw_token.trim().to_string()
    };
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err("token do bot Telegram inválido".into());
    }

    let raw_chats = chats_arg.ok_or("--chats <id1,id2,...> é obrigatório")?;
    let mut allowed_chats = Vec::new();
    for part in raw_chats.split(',') {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        let id: i64 = t
            .parse()
            .map_err(|_| format!("chat id inválido: {t} (esperado número inteiro)"))?;
        if !allowed_chats.contains(&id) {
            allowed_chats.push(id);
        }
    }
    if allowed_chats.is_empty() {
        return Err("informe pelo menos um chat id válido em --chats".into());
    }

    let workspace = workspace_arg.map(|w| {
        let p = PathBuf::from(&w);
        p.canonicalize().unwrap_or(p).to_string_lossy().into_owned()
    });

    secrets::set_secret(TELEGRAM_SECRET_KEY, &token)?;
    Settings::save_user(paths, |s| {
        s.gateway.telegram.enabled = true;
        s.gateway.telegram.allowed_chats = allowed_chats.clone();
        if let Some(m) = model_arg.as_ref() {
            s.gateway.telegram.default_model = Some(m.trim().to_string());
        }
        if let Some(ws) = workspace.as_ref() {
            s.gateway.telegram.default_workspace = Some(ws.clone());
        }
    })?;

    println!(
        "Telegram gateway configurado ({} chat(s) autorizado(s); token armazenado no cofre '{TELEGRAM_SECRET_KEY}').",
        allowed_chats.len()
    );
    Ok(())
}

type PendingApprovals = Arc<Mutex<HashMap<(i64, u64), ApprovalSender>>>;

fn run_cmd(paths: &Paths, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut poll_secs: u64 = 25;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--poll-secs" => {
                let v = args.get(i + 1).ok_or("--poll-secs precisa de um número")?;
                poll_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("--poll-secs inválido: {v}"))?
                    .clamp(5, 55);
                i += 2;
            }
            other => return Err(format!("opção desconhecida em gateway run: {other}").into()),
        }
    }

    let mut settings = load_settings(paths);
    let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }

    let allowed = settings.gateway.telegram.allowed_chats.clone();
    if allowed.is_empty() {
        return Err(
            "nenhum chat autorizado em gateway.telegram.allowed_chats; execute `zerith gateway telegram setup` primeiro"
                .into(),
        );
    }

    let token = secrets::get_secret(TELEGRAM_SECRET_KEY)
        .map_err(|e| format!("falha ao ler '{TELEGRAM_SECRET_KEY}' no cofre: {e}"))?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("token Telegram vazio no cofre".into());
    }

    zerith_runtime::bili::attach(paths.root(), settings.context.billion_context);
    let cfg = provider_config(&settings)?;
    let model = resolve_model(
        settings.gateway.telegram.default_model.clone(),
        &settings,
        &cfg,
    )
    .ok_or(
        "sem modelo: configure routing.main_model, gateway.telegram.default_model ou ZERITH_MODEL",
    )?;
    let client = Arc::new(Client::new(cfg)?);

    let workspace = settings
        .gateway
        .telegram
        .default_workspace
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf()));

    paths.ensure()?;
    let _instance_lock = crate::daemon::PidLock::acquire(paths.root().join("telegram.lock"))?;
    let main_store = Store::open(paths.db())?;

    let cancel = Cancel::default();
    signals::install(cancel.clone());

    let poll_agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(poll_secs + 15))
        .build();
    let send_agent = Arc::new(
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build(),
    );

    let mut chat_sessions: HashMap<i64, String> = HashMap::new();
    let grants_by_chat: Arc<Mutex<HashMap<i64, SharedGrants>>> = Default::default();
    let approvals: PendingApprovals = Default::default();
    let busy_chats: Arc<Mutex<HashSet<i64>>> = Default::default();

    eprintln!(
        "zerith: Telegram gateway ativo (modelo={model}, workspace={}, chats={})",
        workspace.display(),
        allowed.len()
    );

    let mut offset: i64 = load_offset(&main_store);
    while !cancel.is_cancelled() {
        let updates = match post_json(
            &poll_agent,
            &token,
            "getUpdates",
            &json!({
                "offset": offset,
                "timeout": poll_secs,
                "allowed_updates": ["message", "callback_query"]
            }),
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("zerith: {e}");
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };

        let Some(items) = updates.as_array() else {
            continue;
        };

        let prev_offset = offset;
        for upd in items {
            if let Some(uid) = upd["update_id"].as_i64() {
                offset = offset.max(uid + 1);
            }

            if let Some(cq) = upd.get("callback_query").filter(|v| !v.is_null()) {
                handle_callback(&send_agent, &token, &allowed, &approvals, cq);
                continue;
            }

            let Some(msg) = upd.get("message").filter(|v| !v.is_null()) else {
                continue;
            };
            let Some(chat_id) = msg["chat"]["id"].as_i64() else {
                continue;
            };
            if !is_chat_allowed(&allowed, chat_id) {
                eprintln!("zerith: mensagem ignorada de chat não autorizado ({chat_id})");
                continue;
            }
            let Some(text) = msg["text"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };

            if text == "/new" || text == "/clear" {
                let s = main_store
                    .create_session(&format!("Telegram ({chat_id})"), SessionSource::Native)?;
                chat_sessions.insert(chat_id, s.id.clone());
                if let Ok(mut g) = grants_by_chat.lock() {
                    g.remove(&chat_id);
                }
                let _ = send_message(&send_agent, &token, chat_id, "Nova sessão iniciada.");
                continue;
            }

            if text == "/status" {
                let active_id = chat_sessions
                    .get(&chat_id)
                    .map(|s| s.chars().take(8).collect::<String>())
                    .unwrap_or_else(|| "nenhuma".into());
                let busy = busy_chats
                    .lock()
                    .map(|b| b.contains(&chat_id))
                    .unwrap_or(false);
                let status_msg = format!(
                    "Modelo: {model}\nWorkspace: {}\nSessão: {active_id}\nEstado: {}",
                    workspace.display(),
                    if busy { "executando" } else { "livre" }
                );
                let _ = send_message(&send_agent, &token, chat_id, &status_msg);
                continue;
            }

            {
                let mut busy = busy_chats.lock().expect("busy lock");
                if !busy.insert(chat_id) {
                    let _ = send_message(
                        &send_agent,
                        &token,
                        chat_id,
                        "Já existe um turno em execução neste chat; aguarde concluir ou responda à aprovação pendente.",
                    );
                    continue;
                }
            }

            let session_id = match chat_sessions.get(&chat_id) {
                Some(id) => id.clone(),
                None => {
                    let s = main_store
                        .create_session(&format!("Telegram ({chat_id})"), SessionSource::Native)?;
                    chat_sessions.insert(chat_id, s.id.clone());
                    s.id
                }
            };
            let chat_grants = {
                let mut g = grants_by_chat.lock().expect("grants lock");
                g.entry(chat_id).or_default().clone()
            };

            let worker_paths = paths.clone();
            let worker_settings = settings.clone();
            let worker_client = client.clone();
            let worker_model = model.clone();
            let worker_ws = workspace.clone();
            let worker_prompt = text.to_string();
            let worker_token = token.clone();
            let worker_agent = send_agent.clone();
            let worker_approvals = approvals.clone();
            let worker_busy = busy_chats.clone();
            let worker_cancel = cancel.clone();
            let plugin_skills = contrib.as_ref().map(|c| c.skill_dirs()).unwrap_or_default();
            let plugin_mcp = contrib
                .as_ref()
                .map(|c| c.mcp.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>())
                .unwrap_or_default();

            std::thread::spawn(move || {
                let res = run_telegram_turn(TelegramTurnArgs {
                    paths: &worker_paths,
                    settings: &worker_settings,
                    client: &worker_client,
                    model: &worker_model,
                    workspace: &worker_ws,
                    session_id: &session_id,
                    prompt: &worker_prompt,
                    chat_id,
                    token: &worker_token,
                    agent: &worker_agent,
                    grants: chat_grants,
                    approvals: &worker_approvals,
                    cancel: &worker_cancel,
                    plugin_skills,
                    plugin_mcp,
                });
                if let Err(e) = res {
                    let clean = sanitize_err(&worker_token, e);
                    let _ = send_message(
                        &worker_agent,
                        &worker_token,
                        chat_id,
                        &format!("Erro: {clean}"),
                    );
                }
                if let Ok(mut b) = worker_busy.lock() {
                    b.remove(&chat_id);
                }
            });
        }
        if offset > prev_offset {
            save_offset(&main_store, offset);
        }
    }

    Ok(())
}

const TELEGRAM_OFFSET_KEY: &str = "telegram.offset";

fn load_offset(store: &Store) -> i64 {
    store
        .setting(TELEGRAM_OFFSET_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(0)
}

fn save_offset(store: &Store, offset: i64) {
    let _ = store.set_setting(TELEGRAM_OFFSET_KEY, &offset.to_string());
}

fn handle_callback(
    agent: &ureq::Agent,
    token: &str,
    allowed: &[i64],
    approvals: &PendingApprovals,
    cq: &Value,
) {
    let cq_id = cq["id"].as_str().unwrap_or("");
    let chat_id = cq["message"]["chat"]["id"]
        .as_i64()
        .or_else(|| cq["from"]["id"].as_i64());
    let Some(chat_id) = chat_id.filter(|id| is_chat_allowed(allowed, *id)) else {
        return;
    };
    let Some(data) = cq["data"].as_str() else {
        return;
    };
    let Some((req_id, decision)) = parse_callback_decision(data) else {
        let _ = post_json(
            agent,
            token,
            "answerCallbackQuery",
            &json!({"callback_query_id": cq_id, "text": "Ação inválida"}),
        );
        return;
    };
    let sender = approvals
        .lock()
        .ok()
        .and_then(|mut m| m.remove(&(chat_id, req_id)));
    let ack = match sender {
        Some(s) if s.answer(req_id, decision) => match decision {
            Approval::AllowOnce => "Permitido 1x",
            Approval::AllowSession => "Permitido na sessão",
            Approval::Deny => "Negado",
        },
        _ => "Solicitação expirada",
    };
    let _ = post_json(
        agent,
        token,
        "answerCallbackQuery",
        &json!({"callback_query_id": cq_id, "text": ack}),
    );
}

struct TelegramTurnArgs<'a> {
    paths: &'a Paths,
    settings: &'a Settings,
    client: &'a Client,
    model: &'a str,
    workspace: &'a PathBuf,
    session_id: &'a str,
    prompt: &'a str,
    chat_id: i64,
    token: &'a str,
    agent: &'a ureq::Agent,
    grants: SharedGrants,
    approvals: &'a PendingApprovals,
    cancel: &'a Cancel,
    plugin_skills: Vec<PathBuf>,
    plugin_mcp: Vec<zerith_mcp::ServerConfig>,
}

fn run_telegram_turn(t: TelegramTurnArgs<'_>) -> std::result::Result<(), String> {
    let store = Store::open(t.paths.db()).map_err(|e| e.to_string())?;
    let mut policy = Policy::from_settings(t.settings, t.workspace).map_err(|e| e.to_string())?;
    let mut tools = Registry::new();
    let mut mcp = McpRegistry::load(t.paths, Some(t.workspace));
    if !t.plugin_mcp.is_empty() {
        mcp.extend(t.plugin_mcp);
    }
    let _ = mcp.register_into(&mut tools);

    let guards = Guards::new(t.settings.clone(), &mut policy, t.session_id.to_string());
    tools.register(guards.enter_plan_mode_tool());
    tools.register(guards.exit_plan_mode_tool());
    let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
    for tool in zerith_tools::authoring_tools(zerith_tools::Authoring {
        paths: t.paths.clone(),
        project_dir: Some(t.workspace.clone()),
        tools: have,
        mode: policy.mode().settings_name().to_string(),
    }) {
        tools.register(tool);
    }

    let mem_db = SqliteMemory::open(t.paths.db()).map_err(|e| e.to_string())?;
    let scope = Scope::workspace(t.workspace);
    let memory = (!t.settings.memory.paused).then_some(&mem_db as &dyn MemoryBackend);
    let skills = Library::open(t.paths.skills()).ok().map(|l| {
        if t.plugin_skills.is_empty() {
            l
        } else {
            l.with_extra(t.plugin_skills)
        }
    });

    let (approver, gate) = approval_channel_with_grants(t.grants);
    let mut registered_ids = Vec::new();

    let mut opts = AgentOptions {
        tools: Some(&tools),
        policy: Some(&policy),
        approvals: Some(&gate),
        middleware: vec![Box::new(guards)],
        spill_dir: Some(t.paths.root().join("spill")),
        dynamic_context: Some(TELEGRAM_MEDIA_HINT.to_string()),
        ..AgentOptions::for_profile(zerith_runtime::active_profile())
    };
    let mut generated: Vec<String> = Vec::new();
    let _ = zerith_runtime::compact::auto_compact_routed(
        &store,
        t.settings,
        t.client,
        t.session_id,
        t.model,
        opts.history,
        t.cancel,
    );
    // Re-borrow options cleanly after pre-compaction check.
    opts.spill_dir = Some(t.paths.root().join("spill"));

    let res = run_agent_turn(
        &store,
        t.client,
        TurnContext {
            memory,
            scope: Some(&scope),
            skills: skills.as_ref(),
            settings: Some(t.settings),
            project_dir: Some(t.workspace),
            ..TurnContext::default()
        },
        opts,
        t.session_id,
        t.model,
        t.prompt,
        t.cancel,
        |e| {
            if let RunEvent::ToolResult {
                name,
                ok: true,
                output_preview,
                ..
            } = &e
            {
                if name == "generate_image" || name == "generate_video" {
                    generated.extend(generated_files(output_preview));
                }
            }
            if let RunEvent::ApprovalNeeded { id, tool, summary } = e {
                if let Ok(mut map) = t.approvals.lock() {
                    map.insert((t.chat_id, id), approver.clone());
                }
                registered_ids.push(id);
                if let Err(err) =
                    send_approval_prompt(t.agent, t.token, t.chat_id, id, &tool, &summary)
                {
                    eprintln!("zerith: {}", sanitize_err(t.token, err));
                    approver.answer(id, Approval::Deny);
                }
            }
        },
    );

    if let Ok(mut map) = t.approvals.lock() {
        for id in registered_ids {
            map.remove(&(t.chat_id, id));
        }
    }

    let report = res.map_err(|e| sanitize_err(t.token, e))?;
    let (mut files, text) = extract_media(&report.text);
    if !text.is_empty() || files.is_empty() {
        send_message(t.agent, t.token, t.chat_id, &text)?;
    }
    files.extend(generated);
    let media_root = t.settings.media.resolve_output_dir(t.paths);
    deliver_media(t.token, t.chat_id, &files, t.workspace, &media_root);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_fails_closed_when_empty_or_mismatching() {
        assert!(!is_chat_allowed(&[], 12345));
        assert!(is_chat_allowed(&[12345, -98765], 12345));
        assert!(is_chat_allowed(&[12345, -98765], -98765));
        assert!(!is_chat_allowed(&[12345], 99999));
    }

    #[test]
    fn callback_decision_parses_all_variants_and_rejects_malformed() {
        assert_eq!(
            parse_callback_decision("approve:42:once"),
            Some((42, Approval::AllowOnce))
        );
        assert_eq!(
            parse_callback_decision("approve:7:session"),
            Some((7, Approval::AllowSession))
        );
        assert_eq!(
            parse_callback_decision("approve:0:deny"),
            Some((0, Approval::Deny))
        );
        assert_eq!(parse_callback_decision("approve:abc:once"), None);
        assert_eq!(parse_callback_decision("approve:1:always"), None);
        assert_eq!(parse_callback_decision("approve:1:once:extra"), None);
        assert_eq!(parse_callback_decision("other"), None);
    }

    #[test]
    fn chunking_splits_cleanly_with_utf8_and_newlines() {
        let short = chunk_telegram_text("olá rede", 4096);
        assert_eq!(short, vec!["olá rede"]);

        let long = "linha um Ação\nlinha dois Segurança\nlinha três Roteador";
        let chunks = chunk_telegram_text(long, 20);
        assert!(chunks.len() >= 3, "{chunks:?}");
        for c in &chunks {
            assert!(c.chars().count() <= 20, "chunk excedeu limite: {c:?}");
        }
        assert!(chunks.join(" ").contains("Ação"));
        assert!(chunks.join(" ").contains("Roteador"));
    }

    #[test]
    fn sanitize_err_scrubs_bot_token() {
        let cleaned = sanitize_err(
            "123456:ABC-SecretToken",
            "POST https://api.telegram.org/bot123456:ABC-SecretToken/sendMessage failed",
        );
        assert!(!cleaned.contains("ABC-SecretToken"), "{cleaned}");
        assert!(cleaned.contains("<redacted>"), "{cleaned}");
    }

    #[test]
    fn extract_media_strips_lines_dedupes_and_skips_code_fences() {
        let text = "Pronto, segue:\nMEDIA:/tmp/a.png\n  MEDIA: `rel/b.pdf`  \nMEDIA:/tmp/a.png\n\
                    ```\nMEDIA:/exemplo.png\n```\nfim MEDIA:/no/meio.txt\nMEDIA:";
        let (paths, rest) = extract_media(text);
        assert_eq!(paths, vec!["/tmp/a.png", "rel/b.pdf"]);
        assert!(rest.starts_with("Pronto, segue:"), "{rest}");
        assert!(rest.contains("MEDIA:/exemplo.png"), "{rest}");
        assert!(rest.contains("fim MEDIA:/no/meio.txt"), "{rest}");
        assert!(!rest.contains("a.png"), "{rest}");
        assert_eq!(extract_media("só texto").0, Vec::<String>::new());
    }

    #[test]
    fn media_path_validation_enforces_roots_secrets_and_size() {
        let base = std::env::temp_dir().join(format!("zerith-tg-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Tmp(PathBuf);
        impl Tmp {
            fn path(&self) -> &Path {
                &self.0
            }
        }
        let mk = |n: &str| {
            let d = base.join(n);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        };
        let (ws, media, outside) = (mk("ws"), mk("media"), mk("fora"));
        std::fs::write(ws.path().join("relatorio.pdf"), b"pdf").unwrap();
        std::fs::write(ws.path().join(".env"), b"SECRET=1").unwrap();
        std::fs::write(ws.path().join("server.pem"), b"k").unwrap();
        std::fs::write(ws.path().join("id_rsa.pub"), b"k").unwrap();
        std::fs::create_dir(ws.path().join(".ssh")).unwrap();
        std::fs::write(ws.path().join(".ssh").join("config"), b"k").unwrap();
        std::fs::write(media.path().join("gato.png"), b"png").unwrap();
        std::fs::write(outside.path().join("fora.txt"), b"x").unwrap();

        let (p, size) = resolve_media_path("relatorio.pdf", ws.path(), media.path()).unwrap();
        assert_eq!(size, 3);
        assert!(p.ends_with("relatorio.pdf"));
        let img = media.path().join("gato.png");
        assert!(resolve_media_path(img.to_str().unwrap(), ws.path(), media.path()).is_ok());

        let fora = outside.path().join("fora.txt");
        let err = resolve_media_path(fora.to_str().unwrap(), ws.path(), media.path()).unwrap_err();
        assert!(err.contains("fora do workspace"), "{err}");
        let err = resolve_media_path("../fora/fora.txt", ws.path(), media.path()).unwrap_err();
        assert!(!err.is_empty());
        for s in [".env", "server.pem", "id_rsa.pub", ".ssh/config"] {
            let err = resolve_media_path(s, ws.path(), media.path()).unwrap_err();
            assert!(err.contains("sensível"), "{s}: {err}");
        }
        assert!(resolve_media_path("sumiu.txt", ws.path(), media.path()).is_err());
        assert!(resolve_media_path(".", ws.path(), media.path()).is_err());

        let big = std::fs::File::create(ws.path().join("big.bin")).unwrap();
        big.set_len(TELEGRAM_MAX_UPLOAD + 1).unwrap();
        let err = resolve_media_path("big.bin", ws.path(), media.path()).unwrap_err();
        assert!(err.contains("50 MB"), "{err}");

        // Link simbólico de dentro para fora é recusado pelo destino real.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&fora, ws.path().join("link.txt")).unwrap();
            assert!(resolve_media_path("link.txt", ws.path(), media.path()).is_err());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn upload_method_and_generated_files() {
        assert_eq!(upload_method(Path::new("a.PNG"), 10).0, "sendPhoto");
        assert_eq!(
            upload_method(Path::new("a.png"), TELEGRAM_MAX_PHOTO + 1).0,
            "sendDocument"
        );
        assert_eq!(upload_method(Path::new("v.mp4"), 10).0, "sendVideo");
        assert_eq!(upload_method(Path::new("r.pdf"), 10).0, "sendDocument");
        let out = "Imagem gerada com x.\nCusto: US$ 0.01\nArquivo: C:\\m dia\\gato-1.png (image/png, 12 KB)\n![g](x)";
        assert_eq!(generated_files(out), vec!["C:\\m dia\\gato-1.png"]);
    }

    #[test]
    fn telegram_offset_persists_in_store() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(load_offset(&store), 0);
        save_offset(&store, 4219);
        assert_eq!(load_offset(&store), 4219);
        save_offset(&store, 9900);
        assert_eq!(load_offset(&store), 9900);
    }
}
