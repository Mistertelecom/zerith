//! Canais de aviso dos workflows (WhatsApp, Telegram, webhooks).
//!
//! Formatos de API seguidos (só o formato do JSON/rotas, nenhum código
//! copiado):
//! - Evolution API v2 (EvolutionAPI/evolution-api, Apache-2.0), conferido na
//!   v2.3.8: `src/api/routes/sendMessage.router.ts`,
//!   `src/api/dto/sendMessage.dto.ts`, `src/validate/message.schema.ts`,
//!   `group.router.ts` e `instance.controller.ts` (`connectionState`).
//! - WAHA (devlikeapro/waha): `/api/sendText`, `/api/sendFile`,
//!   `/api/sessions/{session}`.
//! - WhatsApp Cloud API (Meta, Graph API): `/{phone_number_id}/messages`,
//!   mensagens `text` e `template`. A Cloud API não envia para grupos (só
//!   números); a versão do Graph é configurável (`graph_version`).
//! - Telegram Bot API: `sendMessage`, `sendDocument`, `getMe`.

use std::path::Path;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::{ChannelConfig, ChannelKind};
use crate::{Result, WorkflowError};

/// Versão do Graph API usada pela Cloud API quando o canal não define
/// `graph_version` (v26.0 saiu em 29/07/2026).
pub const DEFAULT_GRAPH_VERSION: &str = "v26.0";

/// Texto enviado por [`test_channel`] quando pedem envio de teste.
pub const TEST_MESSAGE: &str = "Mensagem de teste do Zerith";

const MAX_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;

fn req_str<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    opt_str(v, key)
        .ok_or_else(|| WorkflowError::Channel(format!("campo obrigatório ausente no canal: {key}")))
}

fn opt_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Arquivo anexado a uma mensagem (relatório, PDF, imagem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub file_name: String,
    pub mimetype: String,
    pub media: Media,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    /// Endereço público que o servidor do canal baixa sozinho.
    Url(String),
    /// Conteúdo do arquivo (vai em base64 ou multipart).
    Bytes(Vec<u8>),
}

impl Attachment {
    /// Lê um arquivo local (limite de 64 MB).
    pub fn from_path(path: &Path) -> Result<Self> {
        let len = std::fs::metadata(path)?.len();
        if len > MAX_ATTACHMENT_BYTES {
            return Err(WorkflowError::Channel(format!(
                "anexo grande demais ({} MB, limite 64 MB): {}",
                len / (1024 * 1024),
                path.display()
            )));
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "arquivo".into());
        Ok(Self {
            mimetype: mime_for(&file_name).into(),
            file_name,
            media: Media::Bytes(std::fs::read(path)?),
        })
    }

    pub fn from_url(url: &str) -> Self {
        let file_name = url
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').next())
            .filter(|s| !s.is_empty() && !s.contains(':'))
            .unwrap_or("arquivo")
            .to_string();
        Self {
            mimetype: mime_for(&file_name).into(),
            file_name,
            media: Media::Url(url.to_string()),
        }
    }

    /// URL ou base64 puro (sem `data:`), como a Evolution e o WAHA esperam.
    fn media_string(&self) -> String {
        match &self.media {
            Media::Url(u) => u.clone(),
            Media::Bytes(b) => STANDARD.encode(b),
        }
    }

    /// `mediatype` da Evolution: image | video | audio | document.
    fn evolution_mediatype(&self) -> &'static str {
        match self.mimetype.split('/').next() {
            Some("image") if self.mimetype != "image/svg+xml" => "image",
            Some("video") => "video",
            Some("audio") => "audio",
            _ => "document",
        }
    }
}

/// Tipo MIME pela extensão (tabela mínima; o resto vira octet-stream).
pub fn mime_for(file_name: &str) -> &'static str {
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// Destino de WhatsApp normalizado: JID colado (`…@g.us`, `…@s.whatsapp.net`,
/// `…@c.us`, `…@lid`) passa sem mudança; id de grupo sem sufixo (mais de 15
/// dígitos, como `120363…` ou o antigo `criador-timestamp`) ganha `@g.us`;
/// o resto vira só dígitos, com `55` na frente quando tem 10 ou 11 dígitos
/// (DDD + número brasileiro sem código do país) e não começa com `+`.
pub fn normalize_whatsapp_target(raw: &str) -> String {
    let t: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if t.contains('@') {
        return t;
    }
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    if digits.len() > 15 {
        let id: String = t
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        return format!("{}@g.us", id.trim_matches('-'));
    }
    match digits.len() {
        0 => t,
        // Com `+` o DDI já veio digitado.
        10 | 11 if !t.starts_with('+') => format!("55{digits}"),
        _ => digits,
    }
}

/// Envia só texto. Ver [`dispatch_with`].
pub fn dispatch(
    channel: &ChannelConfig,
    target_override: Option<&str>,
    text: &str,
) -> Result<String> {
    dispatch_with(channel, target_override, text, None)
}

/// Envia `text` pelo canal, com anexo opcional. Canais sem mídia mandam só o
/// texto e avisam no retorno (que vai para o log do nó).
pub fn dispatch_with(
    channel: &ChannelConfig,
    target_override: Option<&str>,
    text: &str,
    attachment: Option<&Attachment>,
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
    let timeout = Duration::from_secs(
        cfg.get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(if attachment.is_some() { 120 } else { 30 })
            .clamp(1, 600),
    );
    let with_file = |a: Option<&Attachment>| {
        a.map(|a| format!(" com anexo {}", a.file_name))
            .unwrap_or_default()
    };
    let no_media = if attachment.is_some() {
        " (anexo ignorado: este canal só envia texto)"
    } else {
        ""
    };

    match channel.kind {
        ChannelKind::EvolutionApi => {
            let ev = Evolution::from_cfg(cfg)?;
            let number = normalize_whatsapp_target(target.ok_or_else(|| {
                WorkflowError::Channel(
                    "número/grupo destino (target) ausente para Evolution API".into(),
                )
            })?);
            // v2: campos na raiz. `textMessage` repete o texto para a v1.
            // ponytail: mídia só no formato v2 (a v1, descontinuada, aninhava
            // em `mediaMessage`); adicionar se alguém ainda rodar v1.
            let (route, mut body) = match attachment {
                None => (
                    "sendText",
                    json!({ "number": number, "text": text, "textMessage": { "text": text } }),
                ),
                Some(a) => (
                    "sendMedia",
                    json!({
                        "number": number,
                        "mediatype": a.evolution_mediatype(),
                        "mimetype": a.mimetype,
                        "fileName": a.file_name,
                        "caption": text,
                        "media": a.media_string(),
                    }),
                ),
            };
            if let Some(d) = cfg.get("delay").and_then(Value::as_u64) {
                body["delay"] = d.into();
            }
            if attachment.is_none() {
                if let Some(lp) = cfg.get("link_preview").and_then(Value::as_bool) {
                    body["linkPreview"] = lp.into();
                }
            }
            ev.call(
                "POST",
                &format!("/message/{route}/{}", ev.instance),
                Body::Json(&body),
                timeout,
            )?;
            Ok(format!(
                "WhatsApp enviado via Evolution API ({}) → {number}{}",
                ev.instance,
                with_file(attachment)
            ))
        }
        ChannelKind::Waha => {
            let base = req_str(cfg, "base_url")?.trim_end_matches('/');
            let session = opt_str(cfg, "session").unwrap_or("default");
            let raw = target.ok_or_else(|| {
                WorkflowError::Channel("chatId/número destino (target) ausente para WAHA".into())
            })?;
            let chat_id = waha_chat_id(raw);
            let (route, body) = match attachment {
                None => (
                    "sendText",
                    json!({ "session": session, "chatId": chat_id, "text": text }),
                ),
                Some(a) => {
                    let mut file = json!({ "mimetype": a.mimetype, "filename": a.file_name });
                    match &a.media {
                        Media::Url(u) => file["url"] = u.as_str().into(),
                        Media::Bytes(_) => file["data"] = a.media_string().into(),
                    }
                    (
                        "sendFile",
                        json!({ "session": session, "chatId": chat_id, "file": file, "caption": text }),
                    )
                }
            };
            let key = opt_str(cfg, "api_key");
            let headers: Vec<(&str, &str)> = key.map(|k| ("X-Api-Key", k)).into_iter().collect();
            http(
                "POST",
                &format!("{base}/api/{route}"),
                &headers,
                Body::Json(&body),
                timeout,
                &key.into_iter().collect::<Vec<_>>(),
            )?;
            Ok(format!(
                "WhatsApp enviado via WAHA ({session}) → {chat_id}{}",
                with_file(attachment)
            ))
        }
        ChannelKind::WhatsAppCloud => {
            let cloud = Cloud::from_cfg(cfg)?;
            let to = normalize_whatsapp_target(target.ok_or_else(|| {
                WorkflowError::Channel(
                    "número destino (target) ausente para WhatsApp Cloud API".into(),
                )
            })?);
            if to.contains('@') {
                return Err(WorkflowError::Channel(format!(
                    "a WhatsApp Cloud API não envia para grupos nem JIDs ({to}); use um número com DDI"
                )));
            }
            // Fora da janela de 24 h desde a última mensagem do contato a Meta
            // só aceita template aprovado. O texto do aviso vai na variável
            // {{1}} do corpo; `template_no_params: true` para template fixo
            // (ex.: `hello_world`), senão a Meta recusa pela contagem.
            let body = match opt_str(cfg, "template_name") {
                Some(name) => {
                    let mut tpl = json!({
                        "name": name,
                        "language": { "code": opt_str(cfg, "template_language").unwrap_or("pt_BR") },
                    });
                    if !cfg
                        .get("template_no_params")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        tpl["components"] = json!([{
                            "type": "body",
                            "parameters": [{ "type": "text", "text": template_param(text) }]
                        }]);
                    }
                    json!({
                        "messaging_product": "whatsapp",
                        "to": to,
                        "type": "template",
                        "template": tpl
                    })
                }
                None => json!({
                    "messaging_product": "whatsapp",
                    "to": to,
                    "type": "text",
                    "text": { "body": text }
                }),
            };
            cloud.call(
                "POST",
                &format!("{}/messages", cloud.phone_id),
                Body::Json(&body),
                timeout,
            )?;
            Ok(format!("WhatsApp enviado via Cloud API → {to}{no_media}"))
        }
        ChannelKind::Telegram => {
            let tg = Telegram::from_cfg(cfg)?;
            let chat_id = target
                .ok_or_else(|| WorkflowError::Channel("chat_id ausente para Telegram".into()))?;
            match attachment {
                None => {
                    let body = json!({ "chat_id": chat_id, "text": text });
                    tg.call("sendMessage", Body::Json(&body), timeout)?;
                }
                Some(a) => {
                    // Legenda do Telegram tem limite de 1024 caracteres.
                    let caption: String = text.chars().take(1024).collect();
                    match &a.media {
                        Media::Url(u) => {
                            let body =
                                json!({ "chat_id": chat_id, "document": u, "caption": caption });
                            tg.call("sendDocument", Body::Json(&body), timeout)?;
                        }
                        Media::Bytes(bytes) => {
                            let (ctype, body) = multipart(
                                &[("chat_id", chat_id), ("caption", &caption)],
                                ("document", a, bytes),
                            );
                            tg.call("sendDocument", Body::Raw(&ctype, &body), timeout)?;
                        }
                    }
                }
            }
            Ok(format!(
                "Mensagem enviada via Telegram → {chat_id}{}",
                with_file(attachment)
            ))
        }
        ChannelKind::DiscordWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            http(
                "POST",
                url,
                &[],
                Body::Json(&json!({ "content": text })),
                timeout,
                &[url],
            )?;
            Ok(format!("Mensagem enviada via Discord Webhook{no_media}"))
        }
        ChannelKind::SlackWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            http(
                "POST",
                url,
                &[],
                Body::Json(&json!({ "text": text })),
                timeout,
                &[url],
            )?;
            Ok(format!("Mensagem enviada via Slack Webhook{no_media}"))
        }
        ChannelKind::GenericWebhook => {
            let url = req_str(cfg, "webhook_url")?;
            let method = opt_str(cfg, "method")
                .unwrap_or("POST")
                .to_ascii_uppercase();
            let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            let body_str = opt_str(cfg, "body_template")
                .unwrap_or(r#"{"message": {{message_json}}}"#)
                .replace("{{message_json}}", &escaped)
                .replace("{{message}}", text)
                .replace("{{target}}", target.unwrap_or(""));
            let mut headers: Vec<(&str, &str)> = Vec::new();
            let auth = opt_str(cfg, "auth_header");
            if let Some(auth) = auth {
                headers.push(match auth.split_once(':') {
                    Some((k, v)) => (k.trim(), v.trim()),
                    None => ("Authorization", auth),
                });
            }
            if let Some(obj) = cfg.get("headers").and_then(|h| h.as_object()) {
                headers.extend(
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.as_str(), v))),
                );
            }
            http(
                &method,
                url,
                &headers,
                Body::Raw("application/json", body_str.as_bytes()),
                timeout,
                &auth.into_iter().collect::<Vec<_>>(),
            )?;
            Ok(format!("Webhook {method} {url} → OK{no_media}"))
        }
        ChannelKind::DesktopNotify => {
            let preview: String = text.chars().take(180).collect();
            Ok(format!("Notificação local disparada: {preview}{no_media}"))
        }
    }
}

/// Verifica se o canal está pronto (instância conectada, token válido) e,
/// com `send_test`, envia [`TEST_MESSAGE`] ao destino padrão do canal.
pub fn test_channel(channel: &ChannelConfig, send_test: bool) -> Result<String> {
    let cfg = &channel.config;
    let timeout = Duration::from_secs(20);
    let status = match channel.kind {
        ChannelKind::EvolutionApi => {
            let ev = Evolution::from_cfg(cfg)?;
            let raw = ev.call(
                "GET",
                &format!("/instance/connectionState/{}", ev.instance),
                Body::None,
                timeout,
            )?;
            let v: Value = serde_json::from_str(&raw).unwrap_or_default();
            let state = v
                .pointer("/instance/state")
                .or_else(|| v.get("state"))
                .and_then(Value::as_str)
                .unwrap_or("desconhecido");
            if state != "open" {
                return Err(WorkflowError::Channel(format!(
                    "Evolution API: instância '{}' não conectada (estado: {state}); leia o QR code no painel da Evolution",
                    ev.instance
                )));
            }
            format!(
                "Evolution API: instância '{}' conectada (open)",
                ev.instance
            )
        }
        ChannelKind::Waha => {
            let base = req_str(cfg, "base_url")?.trim_end_matches('/');
            let session = opt_str(cfg, "session").unwrap_or("default");
            let key = opt_str(cfg, "api_key");
            let headers: Vec<(&str, &str)> = key.map(|k| ("X-Api-Key", k)).into_iter().collect();
            let raw = http(
                "GET",
                &format!("{base}/api/sessions/{session}"),
                &headers,
                Body::None,
                timeout,
                &key.into_iter().collect::<Vec<_>>(),
            )?;
            let v: Value = serde_json::from_str(&raw).unwrap_or_default();
            let status = v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("desconhecido");
            if status != "WORKING" {
                return Err(WorkflowError::Channel(format!(
                    "WAHA: sessão '{session}' não está pronta (status: {status})"
                )));
            }
            format!("WAHA: sessão '{session}' conectada (WORKING)")
        }
        ChannelKind::WhatsAppCloud => {
            let cloud = Cloud::from_cfg(cfg)?;
            let raw = cloud.call(
                "GET",
                &format!(
                    "{}?fields=display_phone_number,verified_name",
                    cloud.phone_id
                ),
                Body::None,
                timeout,
            )?;
            let v: Value = serde_json::from_str(&raw).unwrap_or_default();
            let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
            format!(
                "Cloud API ({}): número {} ({})",
                cloud.version,
                field("display_phone_number"),
                field("verified_name")
            )
        }
        ChannelKind::Telegram => {
            let raw = Telegram::from_cfg(cfg)?.call("getMe", Body::None, timeout)?;
            let v: Value = serde_json::from_str(&raw).unwrap_or_default();
            let user = v
                .pointer("/result/username")
                .and_then(Value::as_str)
                .unwrap_or("?");
            format!("Telegram: bot @{user} ativo")
        }
        _ => "sem verificação de conexão para este tipo de canal".to_string(),
    };
    if !send_test {
        return Ok(status);
    }
    let sent = dispatch(channel, None, TEST_MESSAGE)?;
    Ok(format!("{status}\n{sent}"))
}

/// Grupo de WhatsApp que pode ser destino (`id` termina em `@g.us`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// Grupos da instância (só Evolution API), para escolher o destino.
pub fn list_groups(channel: &ChannelConfig) -> Result<Vec<ChannelGroup>> {
    if channel.kind != ChannelKind::EvolutionApi {
        return Err(WorkflowError::Channel(
            "listar grupos só está disponível na Evolution API".into(),
        ));
    }
    let ev = Evolution::from_cfg(&channel.config)?;
    // A Evolution busca a foto de cada grupo: pode demorar.
    let raw = ev.call(
        "GET",
        &format!(
            "/group/fetchAllGroups/{}?getParticipants=false",
            ev.instance
        ),
        Body::None,
        Duration::from_secs(90),
    )?;
    let v: Value = serde_json::from_str(&raw)?;
    let mut groups: Vec<ChannelGroup> = v
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|g| {
            let id = g.get("id")?.as_str()?.to_string();
            let name = g
                .get("subject")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_string();
            Some(ChannelGroup {
                id,
                name,
                size: g.get("size").and_then(Value::as_u64),
            })
        })
        .collect();
    groups.sort_by_key(|g| g.name.to_lowercase());
    Ok(groups)
}

struct Evolution<'a> {
    base: &'a str,
    instance: &'a str,
    api_key: &'a str,
}

impl<'a> Evolution<'a> {
    fn from_cfg(cfg: &'a Value) -> Result<Self> {
        Ok(Self {
            base: req_str(cfg, "base_url")?.trim_end_matches('/'),
            instance: req_str(cfg, "instance")?,
            api_key: req_str(cfg, "api_key")?,
        })
    }

    fn call(&self, method: &str, path: &str, body: Body, timeout: Duration) -> Result<String> {
        http(
            method,
            &format!("{}{path}", self.base),
            &[("apikey", self.api_key)],
            body,
            timeout,
            &[self.api_key],
        )
        .map_err(|e| prefix(e, "Evolution API"))
    }
}

struct Cloud<'a> {
    base: &'a str,
    version: &'a str,
    phone_id: &'a str,
    token: &'a str,
}

impl<'a> Cloud<'a> {
    fn from_cfg(cfg: &'a Value) -> Result<Self> {
        Ok(Self {
            base: opt_str(cfg, "api_base")
                .unwrap_or("https://graph.facebook.com")
                .trim_end_matches('/'),
            version: opt_str(cfg, "graph_version").unwrap_or(DEFAULT_GRAPH_VERSION),
            phone_id: req_str(cfg, "phone_number_id")?,
            token: req_str(cfg, "access_token")?,
        })
    }

    fn call(&self, method: &str, path: &str, body: Body, timeout: Duration) -> Result<String> {
        let auth = format!("Bearer {}", self.token);
        http(
            method,
            &format!("{}/{}/{path}", self.base, self.version),
            &[("Authorization", &auth)],
            body,
            timeout,
            &[self.token],
        )
        .map_err(|e| prefix(e, "Cloud API"))
    }
}

struct Telegram<'a> {
    base: &'a str,
    token: &'a str,
}

impl<'a> Telegram<'a> {
    fn from_cfg(cfg: &'a Value) -> Result<Self> {
        Ok(Self {
            // `api_base` serve para servidor local da Bot API (e testes).
            base: opt_str(cfg, "api_base")
                .unwrap_or("https://api.telegram.org")
                .trim_end_matches('/'),
            token: req_str(cfg, "bot_token")?,
        })
    }

    fn call(&self, method: &str, body: Body, timeout: Duration) -> Result<String> {
        let verb = if matches!(body, Body::None) {
            "GET"
        } else {
            "POST"
        };
        http(
            verb,
            &format!("{}/bot{}/{method}", self.base, self.token),
            &[],
            body,
            timeout,
            &[self.token],
        )
        .map_err(|e| prefix(e, "Telegram"))
    }
}

fn waha_chat_id(raw: &str) -> String {
    let t = normalize_whatsapp_target(raw);
    if t.contains('@') || t.is_empty() {
        t
    } else {
        format!("{t}@c.us")
    }
}

/// Parâmetro de template da Meta: sem quebra de linha/tab nem mais de 4
/// espaços seguidos, até 1024 caracteres.
fn template_param(text: &str) -> String {
    let flat = text
        .split(['\n', '\r', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    let compact = flat
        .split(' ')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    compact.chars().take(1024).collect()
}

/// multipart/form-data mínimo: campos de texto + um arquivo.
fn multipart(fields: &[(&str, &str)], file: (&str, &Attachment, &[u8])) -> (String, Vec<u8>) {
    let boundary = format!("zerith{:x}", crate::now_ms());
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    let (name, a, bytes) = file;
    let fname = a.file_name.replace(['"', '\r', '\n'], "_");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\nContent-Type: {}\r\n\r\n",
            a.mimetype
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

enum Body<'a> {
    None,
    Json(&'a Value),
    Raw(&'a str, &'a [u8]),
}

/// Requisição HTTP com erro legível: status, URL e corpo da resposta (até 500
/// caracteres), sempre com os segredos (`secrets`) trocados por `***`.
fn http(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Body,
    timeout: Duration,
    secrets: &[&str],
) -> Result<String> {
    let mut req = ureq::request(method, url).timeout(timeout);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let res = match body {
        Body::None => req.call(),
        Body::Json(v) => req.send_json(v),
        Body::Raw(ctype, bytes) => req.set("Content-Type", ctype).send_bytes(bytes),
    };
    let msg = match res {
        Ok(resp) => return Ok(resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => {
            let body: String = resp
                .into_string()
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            format!("HTTP {code} em {method} {url}: {}", body.trim())
        }
        Err(e) => format!("falha de conexão em {method} {url}: {e}"),
    };
    Err(WorkflowError::Channel(redact(msg, secrets)))
}

fn redact(mut msg: String, secrets: &[&str]) -> String {
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        msg = msg.replace(s, "***");
    }
    msg
}

fn prefix(e: WorkflowError, label: &str) -> WorkflowError {
    match e {
        WorkflowError::Channel(m) => WorkflowError::Channel(format!("{label}: {m}")),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    struct Req {
        line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Req {
        fn header(&self, k: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(h, _)| h.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.as_str())
        }
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    /// Servidor HTTP falso: responde `responses` em ordem, uma por conexão,
    /// e devolve cada requisição recebida.
    fn serve(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<Req>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
                let len = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.parse().unwrap())
                    .unwrap_or(0);
                let mut req_body = vec![0; len];
                r.read_exact(&mut req_body).unwrap();
                tx.send(Req {
                    line: line.trim_end().to_string(),
                    headers,
                    body: req_body,
                })
                .unwrap();
                write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (base, rx)
    }

    fn channel(kind: ChannelKind, config: Value) -> ChannelConfig {
        ChannelConfig {
            id: "c1".into(),
            name: "teste".into(),
            kind,
            enabled: true,
            config,
            updated_at_ms: 0,
        }
    }

    const KEY: &str = "SEGREDO-apikey-123";

    fn evolution(base: &str, extra: Value) -> ChannelConfig {
        let mut cfg =
            json!({ "base_url": format!("{base}/"), "instance": "empresa", "api_key": KEY });
        for (k, v) in extra.as_object().unwrap() {
            cfg[k] = v.clone();
        }
        channel(ChannelKind::EvolutionApi, cfg)
    }

    #[test]
    fn normaliza_numero_e_grupo() {
        assert_eq!(
            normalize_whatsapp_target("(11) 98888-7777"),
            "5511988887777"
        );
        assert_eq!(normalize_whatsapp_target("11 3333-4444"), "551133334444");
        assert_eq!(
            normalize_whatsapp_target("+55 11 98888-7777"),
            "5511988887777"
        );
        assert_eq!(normalize_whatsapp_target("+1 415 555 0100"), "14155550100");
        assert_eq!(
            normalize_whatsapp_target(" 120363025246125486@g.us "),
            "120363025246125486@g.us"
        );
        assert_eq!(
            normalize_whatsapp_target("120363025246125486"),
            "120363025246125486@g.us"
        );
        assert_eq!(
            normalize_whatsapp_target("5511988887777-1612345678"),
            "5511988887777-1612345678@g.us"
        );
        assert_eq!(waha_chat_id("11988887777"), "5511988887777@c.us");
        assert_eq!(template_param("a\n\nb\tc      d"), "a | b | c d");
    }

    #[test]
    fn evolution_texto_para_numero() {
        let (base, rx) = serve(vec![(201, r#"{"key":{"id":"1"}}"#)]);
        let ch = evolution(
            &base,
            json!({ "default_target": "(11) 98888-7777", "delay": 1200, "link_preview": false }),
        );
        let out = dispatch(&ch, None, "olá").unwrap();
        assert!(out.contains("5511988887777"), "{out}");
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "POST /message/sendText/empresa HTTP/1.1");
        assert_eq!(req.header("apikey"), Some(KEY));
        let b = req.json();
        assert_eq!(b["number"], "5511988887777");
        assert_eq!(b["text"], "olá");
        assert_eq!(b["textMessage"]["text"], "olá");
        assert_eq!(b["delay"], 1200);
        assert_eq!(b["linkPreview"], false);
    }

    #[test]
    fn evolution_midia_para_grupo() {
        let (base, rx) = serve(vec![(201, "{}"), (201, "{}")]);
        let ch = evolution(&base, json!({}));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relatorio.pdf");
        std::fs::write(&path, b"%PDF-1.4 teste").unwrap();
        let pdf = Attachment::from_path(&path).unwrap();
        let out = dispatch_with(&ch, Some("120363025246125486@g.us"), "segue", Some(&pdf)).unwrap();
        assert!(out.contains("relatorio.pdf"), "{out}");
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "POST /message/sendMedia/empresa HTTP/1.1");
        let b = req.json();
        assert_eq!(b["number"], "120363025246125486@g.us");
        assert_eq!(b["mediatype"], "document");
        assert_eq!(b["mimetype"], "application/pdf");
        assert_eq!(b["fileName"], "relatorio.pdf");
        assert_eq!(b["caption"], "segue");
        assert_eq!(b["media"], STANDARD.encode(b"%PDF-1.4 teste"));

        let img = Attachment::from_url("https://exemplo.com/a/grafico.png?x=1");
        dispatch_with(&ch, Some("5511988887777"), "", Some(&img)).unwrap();
        let b = rx.recv().unwrap().json();
        assert_eq!(b["mediatype"], "image");
        assert_eq!(b["fileName"], "grafico.png");
        assert_eq!(b["media"], "https://exemplo.com/a/grafico.png?x=1");
    }

    #[test]
    fn erro_mostra_corpo_sem_apikey() {
        let (base, _rx) = serve(vec![(
            401,
            r#"{"status":401,"error":"Unauthorized SEGREDO-apikey-123"}"#,
        )]);
        let ch = evolution(&base, json!({ "default_target": "5511988887777" }));
        let err = dispatch(&ch, None, "x").unwrap_err().to_string();
        assert!(err.contains("Evolution API: HTTP 401"), "{err}");
        assert!(err.contains("Unauthorized ***"), "{err}");
        assert!(!err.contains(KEY), "{err}");
    }

    #[test]
    fn evolution_test_channel_open_e_close() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"instance":{"instanceName":"empresa","state":"open"}}"#,
            ),
            (
                200,
                r#"{"instance":{"instanceName":"empresa","state":"close"}}"#,
            ),
        ]);
        let ch = evolution(&base, json!({}));
        let ok = test_channel(&ch, false).unwrap();
        assert!(ok.contains("open"), "{ok}");
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "GET /instance/connectionState/empresa HTTP/1.1");
        assert_eq!(req.header("apikey"), Some(KEY));
        let err = test_channel(&ch, true).unwrap_err().to_string();
        assert!(err.contains("estado: close"), "{err}");
    }

    #[test]
    fn evolution_lista_grupos() {
        let (base, rx) = serve(vec![(
            200,
            r#"[{"id":"2@g.us","subject":"Zeta","size":3},{"id":"1@g.us","subject":"Alerta TI","size":12}]"#,
        )]);
        let groups = list_groups(&evolution(&base, json!({}))).unwrap();
        assert_eq!(
            rx.recv().unwrap().line,
            "GET /group/fetchAllGroups/empresa?getParticipants=false HTTP/1.1"
        );
        assert_eq!(
            groups,
            vec![
                ChannelGroup {
                    id: "1@g.us".into(),
                    name: "Alerta TI".into(),
                    size: Some(12)
                },
                ChannelGroup {
                    id: "2@g.us".into(),
                    name: "Zeta".into(),
                    size: Some(3)
                },
            ]
        );
        let waha = channel(ChannelKind::Waha, json!({ "base_url": base }));
        assert!(list_groups(&waha).is_err());
    }

    #[test]
    fn cloud_template_e_sem_grupo() {
        let (base, rx) = serve(vec![(200, r#"{"messages":[{"id":"x"}]}"#)]);
        let ch = channel(
            ChannelKind::WhatsAppCloud,
            json!({
                "api_base": base, "phone_number_id": "PHONE", "access_token": "TOKEN-meta-1",
                "template_name": "alerta_zerith", "default_target": "11988887777"
            }),
        );
        dispatch(&ch, None, "falhou\nduas vezes").unwrap();
        let req = rx.recv().unwrap();
        assert_eq!(
            req.line,
            format!("POST /{DEFAULT_GRAPH_VERSION}/PHONE/messages HTTP/1.1")
        );
        assert_eq!(req.header("authorization"), Some("Bearer TOKEN-meta-1"));
        let b = req.json();
        assert_eq!(b["to"], "5511988887777");
        assert_eq!(b["type"], "template");
        assert_eq!(b["template"]["name"], "alerta_zerith");
        assert_eq!(b["template"]["language"]["code"], "pt_BR");
        assert_eq!(
            b["template"]["components"][0]["parameters"][0]["text"],
            "falhou | duas vezes"
        );
        let err = dispatch(&ch, Some("1203@g.us"), "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("não envia para grupos"), "{err}");

        let (base, rx) = serve(vec![(200, "{}")]);
        let mut cfg = ch.config.clone();
        cfg["api_base"] = base.into();
        cfg["graph_version"] = "v25.0".into();
        cfg["template_name"] = "hello_world".into();
        cfg["template_language"] = "en_US".into();
        cfg["template_no_params"] = true.into();
        dispatch(&channel(ChannelKind::WhatsAppCloud, cfg), None, "x").unwrap();
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "POST /v25.0/PHONE/messages HTTP/1.1");
        let b = req.json();
        assert_eq!(b["template"]["language"]["code"], "en_US");
        assert!(b["template"].get("components").is_none());
    }

    #[test]
    fn telegram_documento_multipart_sem_token_no_erro() {
        let (base, rx) = serve(vec![(200, r#"{"ok":true}"#), (400, r#"{"ok":false}"#)]);
        let ch = channel(
            ChannelKind::Telegram,
            json!({ "api_base": base, "bot_token": "123:TOKEN-tg", "default_target": "-100" }),
        );
        let a = Attachment {
            file_name: "r.txt".into(),
            mimetype: "text/plain".into(),
            media: Media::Bytes(b"conteudo".to_vec()),
        };
        dispatch_with(&ch, None, "relatório", Some(&a)).unwrap();
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "POST /bot123:TOKEN-tg/sendDocument HTTP/1.1");
        assert!(req
            .header("content-type")
            .unwrap()
            .starts_with("multipart/form-data"));
        let body = String::from_utf8(req.body).unwrap();
        assert!(body.contains("filename=\"r.txt\"") && body.contains("conteudo"));
        assert!(body.contains("name=\"chat_id\"\r\n\r\n-100\r\n"));
        let err = dispatch(&ch, None, "x").unwrap_err().to_string();
        assert!(
            err.contains("HTTP 400") && !err.contains("TOKEN-tg"),
            "{err}"
        );
    }

    #[test]
    fn waha_sessao_e_arquivo() {
        let (base, rx) = serve(vec![
            (200, r#"{"name":"default","status":"WORKING"}"#),
            (201, "{}"),
        ]);
        let ch = channel(
            ChannelKind::Waha,
            json!({ "base_url": base, "api_key": "wkey-1", "default_target": "11988887777" }),
        );
        assert!(test_channel(&ch, false).unwrap().contains("WORKING"));
        assert_eq!(
            rx.recv().unwrap().line,
            "GET /api/sessions/default HTTP/1.1"
        );
        let a = Attachment::from_url("https://exemplo.com/r.pdf");
        dispatch_with(&ch, None, "oi", Some(&a)).unwrap();
        let req = rx.recv().unwrap();
        assert_eq!(req.line, "POST /api/sendFile HTTP/1.1");
        assert_eq!(req.header("x-api-key"), Some("wkey-1"));
        let b = req.json();
        assert_eq!(b["chatId"], "5511988887777@c.us");
        assert_eq!(b["file"]["url"], "https://exemplo.com/r.pdf");
        assert_eq!(b["file"]["mimetype"], "application/pdf");
        assert_eq!(b["caption"], "oi");
    }
}
