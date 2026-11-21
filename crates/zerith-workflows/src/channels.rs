use serde_json::{json, Value};

use crate::model::{ChannelConfig, ChannelKind};
use crate::{Result, WorkflowError};

fn req_str<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WorkflowError::Channel(format!("campo obrigatório ausente no canal: {key}")))
}

fn opt_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn dispatch(
    channel: &ChannelConfig,
    target_override: Option<&str>,
    text: &str,
) -> Result<String> {
    if !channel.enabled {
        return Err(WorkflowError::Channel(format!(
            "canal '{}' está desabilitado",
            channel.name
        )));
    }
    let cfg = &channel.config;
    let target = target_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| opt_str(cfg, "default_target"));

    match channel.kind {
        ChannelKind::EvolutionApi => {
            let base = req_str(cfg, "base_url")?.trim_end_matches('/');
            let instance = req_str(cfg, "instance")?;
            let api_key = req_str(cfg, "api_key")?;
            let number = target.ok_or_else(|| {
                WorkflowError::Channel(
                    "número/grupo destino (target) ausente para Evolution API".into(),
                )
            })?;
            let url = format!("{base}/message/sendText/{instance}");
            let body = json!({
                "number": number,
                "text": text,
                "textMessage": { "text": text }
            });
            post_json(&url, &[("apikey", api_key)], &body)?;
            Ok(format!(
                "WhatsApp enviado via Evolution API ({instance}) → {number}"
            ))
        }
        ChannelKind::Waha => {
            let base = req_str(cfg, "base_url")?.trim_end_matches('/');
            let session = opt_str(cfg, "session").unwrap_or("default");
            let chat_id = target.ok_or_else(|| {
                WorkflowError::Channel("chatId/número destino (target) ausente para WAHA".into())
            })?;
            let url = format!("{base}/api/sendText");
            let body = json!({
                "session": session,
                "chatId": normalize_waha_chat_id(chat_id),
                "text": text
            });
            let mut headers = Vec::new();
            if let Some(key) = opt_str(cfg, "api_key") {
                headers.push(("X-Api-Key", key));
            }
            post_json(&url, &headers, &body)?;
            Ok(format!("WhatsApp enviado via WAHA ({session}) → {chat_id}"))
        }
        ChannelKind::WhatsAppCloud => {
            let phone_id = req_str(cfg, "phone_number_id")?;
            let token = req_str(cfg, "access_token")?;
            let to = target.ok_or_else(|| {
                WorkflowError::Channel(
                    "número destino (target) ausente para WhatsApp Cloud API".into(),
                )
            })?;
            let url = format!("https://graph.facebook.com/v20.0/{phone_id}/messages");
            let auth = format!("Bearer {token}");
            let body = json!({
                "messaging_product": "whatsapp",
                "to": to,
                "type": "text",
                "text": { "body": text }
            });
            post_json(&url, &[("Authorization", &auth)], &body)?;
            Ok(format!("WhatsApp enviado via Cloud API → {to}"))
        }
        ChannelKind::Telegram => {
            let bot_token = req_str(cfg, "bot_token")?;
            let chat_id = target
                .ok_or_else(|| WorkflowError::Channel("chat_id ausente para Telegram".into()))?;
            let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
            let body = json!({
                "chat_id": chat_id,
                "text": text
            });
            post_json(&url, &[], &body)?;
            Ok(format!("Mensagem enviada via Telegram → {chat_id}"))
        }
        ChannelKind::DiscordWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            let body = json!({ "content": text });
            post_json(url, &[], &body)?;
            Ok("Mensagem enviada via Discord Webhook".into())
        }
        ChannelKind::SlackWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            let body = json!({ "text": text });
            post_json(url, &[], &body)?;
            Ok("Mensagem enviada via Slack Webhook".into())
        }
        ChannelKind::GenericWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            let method = opt_str(cfg, "method")
                .unwrap_or("POST")
                .to_ascii_uppercase();
            let mut body_str = opt_str(cfg, "body_template")
                .map(str::to_string)
                .unwrap_or_else(|| r#"{"message": {{message_json}}}"#.to_string());
            let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            body_str = body_str
                .replace("{{message_json}}", &escaped)
                .replace("{{message}}", text)
                .replace("{{target}}", target.unwrap_or(""));

            let mut req = ureq::request(&method, url).set("Content-Type", "application/json");
            if let Some(auth) = opt_str(cfg, "auth_header") {
                if let Some((k, v)) = auth.split_once(':') {
                    req = req.set(k.trim(), v.trim());
                } else {
                    req = req.set("Authorization", auth);
                }
            }
            if let Some(obj) = cfg.get("headers").and_then(|h| h.as_object()) {
                for (k, v) in obj {
                    if let Some(vs) = v.as_str() {
                        req = req.set(k, vs);
                    }
                }
            }
            match req.send_string(&body_str) {
                Ok(resp) => Ok(format!("Webhook {method} {} → HTTP {}", url, resp.status())),
                Err(ureq::Error::Status(code, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    Err(WorkflowError::Channel(format!("HTTP {code}: {body}")))
                }
                Err(e) => Err(WorkflowError::Channel(e.to_string())),
            }
        }
        ChannelKind::DesktopNotify => {
            let preview: String = text.chars().take(180).collect();
            Ok(format!("Notificação local disparada: {preview}"))
        }
    }
}

fn normalize_waha_chat_id(raw: &str) -> String {
    if raw.contains('@') {
        raw.to_string()
    } else {
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            raw.to_string()
        } else {
            format!("{digits}@c.us")
        }
    }
}

fn post_json(url: &str, headers: &[(&str, &str)], body: &Value) -> Result<String> {
    let mut req = ureq::post(url).set("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.send_json(body.clone()) {
        Ok(resp) => Ok(resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => {
            let err_body = resp.into_string().unwrap_or_default();
            Err(WorkflowError::Channel(format!("HTTP {code}: {err_body}")))
        }
        Err(e) => Err(WorkflowError::Channel(e.to_string())),
    }
}
