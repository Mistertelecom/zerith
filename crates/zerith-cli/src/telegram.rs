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
use std::path::PathBuf;
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
    let resp = agent
        .post(&api_url(token, method))
        .send_json(body.clone())
        .map_err(|e| format!("Telegram {method} falhou: {}", sanitize_err(token, e)))?;
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

    let mut offset: i64 = 0;
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
    }

    Ok(())
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
        ..AgentOptions::for_profile(zerith_runtime::active_profile())
    };
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
    send_message(t.agent, t.token, t.chat_id, &report.text)
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
}
