//! Image and video generation through OpenRouter's media endpoints.
//!
//! Separate from the chat client on purpose: different endpoints, different
//! key (`OPENROUTER_API_KEY`, never the chat provider's), minutes-long jobs,
//! and a bill attached to every call.
//!
//! Verified against the live public endpoints on 2026-11-16: the model lists
//! (`/images/models`, `/videos/models`, `/models?output_modalities=`) and
//! their typed parameter descriptors. NOT verified with a paid call (no key
//! was used): the request/response shapes of `POST /images`, `POST /videos`,
//! the polling payload and `GET /key`. They follow the published description;
//! every field read is tolerant, and an unexpected shape is a
//! [`MediaError::Malformed`], never a panic.
//!
//! Sync only (ureq + one thread per request so a cancel is honoured while a
//! request is in flight). The key lives in this struct and in a request
//! header; error text is scrubbed of it.

use crate::Cancel;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Largest decoded image payload accepted from one call.
pub const MAX_IMAGE_BYTES: u64 = 50 << 20;
/// Largest video file accepted.
pub const MAX_VIDEO_BYTES: u64 = 500 << 20;
/// Largest local reference image uploaded.
pub const MAX_REFERENCE_BYTES: u64 = 20 << 20;
/// How long a video job is polled before giving up.
pub const VIDEO_MAX_WAIT: Duration = Duration::from_secs(600);
/// Default gap between polls (the API suggests 10-30 s).
pub const VIDEO_POLL: Duration = Duration::from_secs(10);

const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(60);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);
const CATALOG_TTL: Duration = Duration::from_secs(600);
/// Parallel requests for the per-model image prices.
const PRICE_WORKERS: usize = 8;
/// A model this recent (by `created`) gets the "novo" badge.
pub const NEW_WINDOW_SECS: i64 = 30 * 86_400;
const REFERER: &str = "https://github.com/Mistertelecom/zerith";
const TITLE: &str = "Zerith";
/// Params whose value must be sent as a JSON integer.
const INT_PARAMS: [&str; 4] = ["duration", "n", "seed", "output_compression"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Image,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "image" | "imagem" => Some(Kind::Image),
            "video" | "vídeo" => Some(Kind::Video),
            _ => None,
        }
    }

    fn list_path(self) -> &'static str {
        match self {
            Kind::Image => "/images/models",
            Kind::Video => "/videos/models",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    /// One of these values (numbers are rendered as text: `5` is `"5"`).
    Enum(Vec<String>),
    Range {
        min: f64,
        max: f64,
    },
    /// True/false.
    Flag,
    /// Free text or a number the descriptor does not bound (`seed`, `size`).
    Text,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaModel {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub description: String,
    /// Price line from the OpenRouter API (image: `/images/models/{id}/endpoints`,
    /// else `/models`; video: `pricing_skus`). Never a figure from this code;
    /// empty when the API gives none.
    pub price_hint: String,
    /// Parameters the model accepts. Excludes reference images / frames.
    pub params: Vec<ParamSpec>,
    pub created: i64,
    /// Image: how many reference images the model takes (0 = text only).
    pub max_references: u32,
    /// Video: accepted frame images (`first_frame`, `last_frame`).
    pub frame_images: Vec<String>,
}

impl MediaModel {
    /// Published less than [`NEW_WINDOW_SECS`] before `now` (unix seconds).
    pub fn is_new(&self, now: i64) -> bool {
        self.created > 0 && now - self.created < NEW_WINDOW_SECS
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPage {
    pub models: Vec<MediaModel>,
    /// Matches of the query, before `offset`/`limit`.
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub label: String,
    pub usage_usd: Option<f64>,
    pub limit_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedFile {
    pub path: PathBuf,
    pub media_type: String,
    pub bytes: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("função ainda não implementada")]
    NotImplemented,
    #[error(
        "nenhuma chave do OpenRouter configurada: rode `zerith media key set` ou defina OPENROUTER_API_KEY"
    )]
    NoKey,
    #[error("{}", http_message(*status, body))]
    Http { status: u16, body: String },
    #[error("limite de requisições do OpenRouter atingido{}", retry_hint(*retry_after_ms))]
    RateLimited {
        retry_after_ms: Option<u64>,
        body: String,
    },
    #[error("pagamento necessário: {0}")]
    Payment(String),
    #[error("falha de rede: {0}")]
    Transport(String),
    #[error("resposta inesperada do OpenRouter: {0}")]
    Malformed(String),
    #[error("cancelado")]
    Cancelled,
    #[error("tempo esgotado")]
    Timeout,
    #[error("{0}")]
    Invalid(String),
    /// The provider accepted the job and reported it failed/cancelled/expired.
    #[error("a geração falhou no provedor: {0}")]
    Failed(String),
}

fn retry_hint(ms: Option<u64>) -> String {
    ms.map(|ms| format!("; tente de novo em {} s", ms.div_ceil(1000)))
        .unwrap_or_default()
}

fn http_message(status: u16, body: &str) -> String {
    let detail = error_message(body);
    let lower = body.to_ascii_lowercase();
    match status {
        401 => "chave do OpenRouter inválida ou revogada (HTTP 401); confira com `zerith media key check`".into(),
        403 => format!("bloqueado por orçamento ou guardrail da conta (HTTP 403): {detail}"),
        400 if lower.contains("zdr") || lower.contains("zero data") => format!(
            "pedido recusado (HTTP 400): chaves com retenção zero de dados (ZDR) não podem gerar vídeo: {detail}"
        ),
        400 => format!("pedido recusado pelo OpenRouter (HTTP 400): {detail}"),
        _ => format!("OpenRouter respondeu HTTP {status}: {detail}"),
    }
}

/// `error.message` of an OpenRouter error body, or the raw text, trimmed.
fn error_message(body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    clip(&msg, 400)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

impl MediaError {
    /// Removes the API key from every text the error carries.
    fn scrub(self, key: Option<&str>) -> Self {
        let Some(k) = key.filter(|k| !k.is_empty()) else {
            return self;
        };
        let s = |t: String| t.replace(k, "[chave]");
        match self {
            MediaError::Http { status, body } => MediaError::Http {
                status,
                body: s(body),
            },
            MediaError::RateLimited {
                retry_after_ms,
                body,
            } => MediaError::RateLimited {
                retry_after_ms,
                body: s(body),
            },
            MediaError::Payment(t) => MediaError::Payment(s(t)),
            MediaError::Transport(t) => MediaError::Transport(s(t)),
            MediaError::Malformed(t) => MediaError::Malformed(s(t)),
            MediaError::Invalid(t) => MediaError::Invalid(s(t)),
            MediaError::Failed(t) => MediaError::Failed(s(t)),
            other => other,
        }
    }
}

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug, Clone, Default)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub params: BTreeMap<String, Value>,
    /// URLs, `data:` URLs or local image paths.
    pub reference_images: Vec<String>,
    /// Becomes the start of the file name (sanitized).
    pub filename_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub params: BTreeMap<String, Value>,
    pub reference_images: Vec<String>,
    pub first_frame: Option<String>,
    pub last_frame: Option<String>,
    pub filename_hint: Option<String>,
}

/// What a request would really send, decided against the model's descriptors.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub model: MediaModel,
    /// Parameters that will be sent, typed as the API wants them.
    pub sent: BTreeMap<String, Value>,
    /// pt-BR lines about what was dropped.
    pub notes: Vec<String>,
}

/// A model list and when it was fetched.
type CachedList = (Instant, Arc<Vec<MediaModel>>);
/// Image price hints by model id and when they were fetched.
type CachedPrices = (Instant, Arc<HashMap<String, String>>);

pub struct OpenRouter {
    key: Option<String>,
    base: String,
    agent: ureq::Agent,
    poll: Duration,
    max_wait: Duration,
    cache: Mutex<HashMap<Kind, CachedList>>,
    prices: Mutex<Option<CachedPrices>>,
}

impl std::fmt::Debug for OpenRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouter")
            .field("base", &self.base)
            .field("key", &self.key.as_ref().map(|_| "[chave]"))
            .finish()
    }
}

impl OpenRouter {
    /// `base_url` empty/None: the public endpoint. A gateway can be used.
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        let base = base_url
            .as_deref()
            .map(|b| b.trim().trim_end_matches('/'))
            .filter(|b| !b.is_empty())
            .unwrap_or(DEFAULT_BASE_URL)
            .to_string();
        // No redirects: the Bearer header must never follow one to another host.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(120))
            .timeout_write(Duration::from_secs(120))
            .redirects(0)
            .user_agent("zerith")
            .build();
        Self {
            key: api_key.filter(|k| !k.trim().is_empty()),
            base,
            agent,
            poll: VIDEO_POLL,
            max_wait: VIDEO_MAX_WAIT,
            cache: Mutex::new(HashMap::new()),
            prices: Mutex::new(None),
        }
    }

    /// Gap between video polls (tests use milliseconds).
    pub fn with_poll_interval(mut self, d: Duration) -> Self {
        self.poll = d;
        self
    }

    /// How long a video job is awaited.
    pub fn with_max_wait(mut self, d: Duration) -> Self {
        self.max_wait = d;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn key(&self) -> Result<&str> {
        self.key.as_deref().ok_or(MediaError::NoKey)
    }

    // --- transport ----------------------------------------------------------

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn open(
        &self,
        method: &'static str,
        url: String,
        auth: bool,
        body: Option<Value>,
        total: Option<Duration>,
        cancel: &Cancel,
    ) -> Result<ureq::Response> {
        let key = if auth {
            Some(self.key()?.to_string())
        } else {
            None
        };
        let agent = if auth {
            self.agent.clone()
        } else {
            // A signed URL may redirect to a CDN; no key travels there.
            crate::shared_http_agent(120_000)
        };
        let scrub = key.clone();
        let limit = total.unwrap_or(Duration::from_secs(150)) + Duration::from_secs(5);
        run_cancellable(cancel, limit, move || {
            let mut req = agent.request(method, &url);
            if auth {
                req = req.set("HTTP-Referer", REFERER).set("X-Title", TITLE);
            }
            if let Some(k) = &key {
                req = req.set("Authorization", &format!("Bearer {k}"));
            }
            if let Some(t) = total {
                req = req.timeout(t);
            }
            let r = match body {
                Some(b) => req.send_json(b),
                None => req.call(),
            };
            match r {
                Ok(r) => Ok(r),
                Err(ureq::Error::Status(code, resp)) => Err(status_error(code, resp)),
                Err(e) => Err(MediaError::Transport(e.to_string())),
            }
        })
        .map_err(|e| e.scrub(scrub.as_deref()))
    }

    #[allow(clippy::too_many_arguments)]
    fn json(
        &self,
        method: &'static str,
        path: &str,
        auth: bool,
        body: Option<Value>,
        total: Duration,
        cap: u64,
        cancel: &Cancel,
    ) -> Result<Value> {
        let resp = self.open(method, self.url(path), auth, body, Some(total), cancel)?;
        let bytes = read_capped(resp, cap, cancel)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            MediaError::Malformed(format!(
                "JSON inválido ({e}): {}",
                clip(&lossy(&bytes), 200)
            ))
        })
    }

    // --- catalogue ----------------------------------------------------------

    /// Models of one kind, filtered client-side by `query` (every word must
    /// appear in id or name). The lists are small (dozens) and the typed
    /// endpoints have no documented search parameter, so a server-side `q`
    /// would be a guess. Public: needs no key.
    pub fn models(
        &self,
        kind: Kind,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ModelPage> {
        let all = self.catalog(kind)?;
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let hits: Vec<&MediaModel> = all
            .iter()
            .filter(|m| {
                let hay = format!("{} {}", m.id, m.name).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        let mut models: Vec<MediaModel> = hits
            .iter()
            .skip(offset)
            .take(limit)
            .copied()
            .cloned()
            .collect();
        if kind == Kind::Image {
            let prices = self.image_prices(&all);
            for m in &mut models {
                if let Some(h) = prices.get(&m.id) {
                    m.price_hint = h.clone();
                }
            }
        }
        Ok(ModelPage {
            total: hits.len(),
            models,
        })
    }

    fn catalog(&self, kind: Kind) -> Result<Arc<Vec<MediaModel>>> {
        if let Some((at, list)) = self.cache.lock().ok().and_then(|c| c.get(&kind).cloned()) {
            if at.elapsed() < CATALOG_TTL {
                return Ok(list);
            }
        }
        let none = Cancel::default();
        let typed = self.json(
            "GET",
            kind.list_path(),
            false,
            None,
            CATALOG_TIMEOUT,
            16 << 20,
            &none,
        )?;
        let list = Arc::new(parse_list(kind, &typed)?);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(kind, (Instant::now(), list.clone()));
        }
        Ok(list)
    }

    /// Image price per model id, cached like the lists. Only the model list
    /// screen asks for it, never a generation. Video prices come inline
    /// (`pricing_skus`) and need no extra request.
    fn image_prices(&self, models: &[MediaModel]) -> Arc<HashMap<String, String>> {
        if let Some((at, p)) = self.prices.lock().ok().and_then(|c| c.clone()) {
            if at.elapsed() < CATALOG_TTL {
                return p;
            }
        }
        // Fallback first: the generic listing's per-token figure.
        let mut out = self.generic_image_prices();
        // Then the real per-image / per-megapixel prices, one small request
        // per model (the typed list has none), a few at a time.
        let next = std::sync::atomic::AtomicUsize::new(0);
        let found = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..PRICE_WORKERS.min(models.len()) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(m) = models.get(i) else { break };
                    // The id goes into a URL path: nothing but a model slug.
                    if !m
                        .id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "/-._:".contains(c))
                        || m.id.contains("..")
                    {
                        continue;
                    }
                    let Ok(v) = self.json(
                        "GET",
                        &format!("/images/models/{}/endpoints", m.id),
                        false,
                        None,
                        CATALOG_TIMEOUT,
                        4 << 20,
                        &Cancel::default(),
                    ) else {
                        continue;
                    };
                    let hint = image_endpoint_price(&v["endpoints"]);
                    if !hint.is_empty() {
                        if let Ok(mut f) = found.lock() {
                            f.push((m.id.clone(), hint));
                        }
                    }
                });
            }
        });
        out.extend(found.into_inner().unwrap_or_default());
        let p = Arc::new(out);
        if let Ok(mut c) = self.prices.lock() {
            *c = Some((Instant::now(), p.clone()));
        }
        p
    }

    /// `pricing.image_output` (USD per image token) of `/models`. Its
    /// `pricing.image` is the price of an INPUT image, so it is not used.
    fn generic_image_prices(&self) -> HashMap<String, String> {
        let none = Cancel::default();
        let mut out = HashMap::new();
        let mut offset = 0usize;
        for _ in 0..5 {
            let Ok(v) = self.json(
                "GET",
                &format!("/models?output_modalities=image&limit=200&offset={offset}"),
                false,
                None,
                CATALOG_TIMEOUT,
                16 << 20,
                &none,
            ) else {
                break;
            };
            let data = v["data"].as_array().cloned().unwrap_or_default();
            for m in &data {
                if let Some(id) = m["id"].as_str() {
                    let hint = image_price_hint(id, &m["pricing"]);
                    if !hint.is_empty() {
                        out.insert(id.to_string(), hint);
                    }
                }
            }
            offset += data.len();
            let total = v["total_count"].as_u64().unwrap_or(0) as usize;
            if data.is_empty() || offset >= total {
                break;
            }
        }
        out
    }

    fn find_model(&self, kind: Kind, id: &str) -> Result<MediaModel> {
        let all = self.catalog(kind)?;
        // `vendor/model:free` shares the descriptors of `vendor/model`.
        let base = id.split(':').next().unwrap_or(id);
        all.iter()
            .find(|m| m.id == id)
            .or_else(|| all.iter().find(|m| m.id == base))
            .cloned()
            .ok_or_else(|| {
                MediaError::Invalid(format!(
                    "o modelo '{id}' não está na lista de geração de {} do OpenRouter; \
                     ele pode só funcionar pelo caminho chat/completions com `modalities`, \
                     que o Zerith ainda não implementa. Escolha outro modelo.",
                    if kind == Kind::Image {
                        "imagens"
                    } else {
                        "vídeos"
                    }
                ))
            })
    }

    // --- key ----------------------------------------------------------------

    /// `GET /key`: label and spend of the configured key. Field names follow
    /// the public docs and were not exercised with a real key.
    pub fn check_key(&self) -> Result<KeyInfo> {
        let v = self.json(
            "GET",
            "/key",
            true,
            None,
            CATALOG_TIMEOUT,
            1 << 20,
            &Cancel::default(),
        )?;
        let d = if v["data"].is_object() {
            &v["data"]
        } else {
            &v
        };
        Ok(KeyInfo {
            label: d["label"].as_str().unwrap_or("").to_string(),
            usage_usd: d["usage"].as_f64(),
            limit_usd: d["limit"].as_f64(),
        })
    }

    // --- planning -----------------------------------------------------------

    /// Validates `req.params` against the model and says what would be sent.
    /// Unknown parameter names are dropped (with a note); a bad value is an
    /// error. Needs the (cached) public model list.
    pub fn plan_image(&self, req: &ImageRequest) -> Result<Plan> {
        let model = self.find_model(Kind::Image, &req.model)?;
        let (sent, mut notes) = plan_params(&model, &req.params)?;
        let n = req.reference_images.len();
        if n > 0 && model.max_references == 0 {
            notes.push(format!(
                "{n} imagem(ns) de referência ignorada(s): o modelo só aceita texto"
            ));
        } else if n as u32 > model.max_references {
            return Err(MediaError::Invalid(format!(
                "o modelo aceita no máximo {} imagens de referência; recebeu {n}",
                model.max_references
            )));
        }
        Ok(Plan { model, sent, notes })
    }

    pub fn plan_video(&self, req: &VideoRequest) -> Result<Plan> {
        let model = self.find_model(Kind::Video, &req.model)?;
        let (sent, mut notes) = plan_params(&model, &req.params)?;
        for (frame, given) in [
            ("first_frame", &req.first_frame),
            ("last_frame", &req.last_frame),
        ] {
            if given.is_some() && !model.frame_images.iter().any(|f| f == frame) {
                notes.push(format!("{frame} ignorado: o modelo não aceita esse quadro"));
            }
        }
        Ok(Plan { model, sent, notes })
    }

    // --- image --------------------------------------------------------------

    pub fn generate_image(
        &self,
        req: &ImageRequest,
        out_dir: &Path,
        cancel: &Cancel,
    ) -> Result<Vec<GeneratedFile>> {
        self.key()?;
        if req.prompt.trim().is_empty() {
            return Err(MediaError::Invalid("o prompt está vazio".into()));
        }
        let plan = self.plan_image(req)?;
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert("prompt".into(), json!(req.prompt));
        for (k, v) in &plan.sent {
            body.insert(k.clone(), v.clone());
        }
        if plan.model.max_references > 0 && !req.reference_images.is_empty() {
            let refs = req
                .reference_images
                .iter()
                .map(|r| {
                    reference_to_url(r)
                        .map(|u| json!({"type": "image_url", "image_url": {"url": u}}))
                })
                .collect::<Result<Vec<_>>>()?;
            body.insert("input_references".into(), Value::Array(refs));
        }
        let v = self.json(
            "POST",
            "/images",
            true,
            Some(Value::Object(body)),
            IMAGE_TIMEOUT,
            MAX_IMAGE_BYTES * 4 / 3 + (1 << 20),
            cancel,
        )?;
        let items = v["data"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| MediaError::Malformed("resposta sem `data` de imagens".into()))?;
        let cost = v["usage"]["cost"].as_f64().map(|c| c / items.len() as f64);
        let mut decoded = Vec::new();
        let mut total = 0u64;
        for it in items {
            let b64 = it["b64_json"]
                .as_str()
                .ok_or_else(|| MediaError::Malformed("imagem sem `b64_json`".into()))?;
            let bytes = b64_decode(b64)
                .ok_or_else(|| MediaError::Malformed("base64 inválido na imagem".into()))?;
            total += bytes.len() as u64;
            if total > MAX_IMAGE_BYTES {
                return Err(MediaError::Invalid(format!(
                    "as imagens passaram de {} MB; recusadas",
                    MAX_IMAGE_BYTES >> 20
                )));
            }
            let mt = it["media_type"]
                .as_str()
                .map(str::to_string)
                .or_else(|| sniff(&bytes).map(str::to_string))
                .unwrap_or_default();
            let ext = ext_for(&mt).ok_or_else(|| {
                MediaError::Malformed(format!("tipo de mídia desconhecido: '{mt}'"))
            })?;
            decoded.push((bytes, mt, ext));
        }
        let stem = stem_for(req.filename_hint.as_deref(), "imagem");
        let mut out = Vec::new();
        for (bytes, mt, ext) in decoded {
            let (path, mut f) = create_unique(out_dir, &stem, ext)?;
            f.write_all(&bytes).map_err(|e| {
                MediaError::Transport(format!("não consegui gravar {}: {e}", path.display()))
            })?;
            out.push(GeneratedFile {
                path,
                media_type: mt,
                bytes: bytes.len() as u64,
                cost_usd: cost,
            });
        }
        Ok(out)
    }

    // --- video --------------------------------------------------------------

    pub fn generate_video(
        &self,
        req: &VideoRequest,
        out_dir: &Path,
        cancel: &Cancel,
        progress: &mut dyn FnMut(&str),
    ) -> Result<GeneratedFile> {
        self.key()?;
        if req.prompt.trim().is_empty() {
            return Err(MediaError::Invalid("o prompt está vazio".into()));
        }
        let plan = self.plan_video(req)?;
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert("prompt".into(), json!(req.prompt));
        for (k, v) in &plan.sent {
            body.insert(k.clone(), v.clone());
        }
        let mut frames = Vec::new();
        for (frame, given) in [
            ("first_frame", &req.first_frame),
            ("last_frame", &req.last_frame),
        ] {
            if let Some(g) = given
                .as_ref()
                .filter(|_| plan.model.frame_images.iter().any(|f| f == frame))
            {
                frames.push(json!({
                    "type": "image_url",
                    "image_url": {"url": reference_to_url(g)?},
                    "frame_type": frame,
                }));
            }
        }
        if !frames.is_empty() {
            body.insert("frame_images".into(), Value::Array(frames));
        }
        if !req.reference_images.is_empty() {
            let refs = req
                .reference_images
                .iter()
                .map(|r| {
                    reference_to_url(r)
                        .map(|u| json!({"type": "image_url", "image_url": {"url": u}}))
                })
                .collect::<Result<Vec<_>>>()?;
            body.insert("input_references".into(), Value::Array(refs));
        }
        let sub = self.json(
            "POST",
            "/videos",
            true,
            Some(Value::Object(body)),
            SUBMIT_TIMEOUT,
            1 << 20,
            cancel,
        )?;
        let id = sub["id"]
            .as_str()
            .filter(|i| {
                !i.is_empty()
                    && i.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
            })
            .ok_or_else(|| {
                MediaError::Malformed("o envio do vídeo não devolveu um `id` válido".into())
            })?
            .to_string();
        progress("enviado");

        let start = Instant::now();
        let mut transient = 0u32;
        let done = loop {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            if start.elapsed() >= self.max_wait {
                return Err(MediaError::Timeout);
            }
            match self.json(
                "GET",
                &format!("/videos/{id}"),
                true,
                None,
                CATALOG_TIMEOUT,
                1 << 20,
                cancel,
            ) {
                Ok(v) => {
                    transient = 0;
                    let status = v["status"].as_str().unwrap_or("").to_string();
                    progress(&format!("{status} ({} s)", start.elapsed().as_secs()));
                    match status.as_str() {
                        "completed" => break v,
                        "pending" | "in_progress" => {}
                        "failed" | "cancelled" | "expired" => {
                            let why = v["error"]["message"]
                                .as_str()
                                .or_else(|| v["error"].as_str())
                                .unwrap_or("sem detalhe");
                            return Err(MediaError::Failed(format!(
                                "{status}: {}",
                                clip(why, 300)
                            )));
                        }
                        other => {
                            return Err(MediaError::Malformed(format!(
                                "status desconhecido '{other}'"
                            )))
                        }
                    }
                }
                // A flaky poll must not lose a job that is already paid for.
                Err(MediaError::Transport(_))
                | Err(MediaError::Http {
                    status: 500..=599, ..
                }) if transient < 3 => {
                    transient += 1;
                }
                Err(e) => return Err(e),
            }
            sleep_cancellable(self.poll, cancel)?;
        };

        let cost = done["usage"]["cost"].as_f64();
        let resp = match self.open(
            "GET",
            self.url(&format!("/videos/{id}/content")),
            true,
            None,
            None,
            cancel,
        ) {
            Ok(r) => r,
            // Fall back to the signed URL (no key needed, temporary).
            Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
            Err(first) => match done["urls"][0]
                .as_str()
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            {
                Some(u) => self
                    .open("GET", u.to_string(), false, None, None, cancel)
                    .map_err(|_| first)?,
                None => return Err(first),
            },
        };
        let mt = resp
            .header("content-type")
            .map(|c| c.split(';').next().unwrap_or("").trim().to_string())
            .filter(|c| c.starts_with("video/"))
            .unwrap_or_else(|| "video/mp4".into());
        let ext = ext_for(&mt).unwrap_or("mp4");
        let stem = stem_for(req.filename_hint.as_deref(), "video");
        let (path, mut file) = create_unique(out_dir, &stem, ext)?;
        let bytes = pump(resp, MAX_VIDEO_BYTES, cancel, |c| {
            file.write_all(c)
                .map_err(|e| MediaError::Transport(format!("não consegui gravar o vídeo: {e}")))
        })
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?;
        Ok(GeneratedFile {
            path,
            media_type: mt,
            bytes,
            cost_usd: cost,
        })
    }
}

// --- helpers ------------------------------------------------------------------

/// Runs `f` on a thread and waits in short slices, so a cancel or the limit is
/// honoured while a blocking request is in flight. The thread ends with its
/// own request timeout.
fn run_cancellable<T: Send + 'static>(
    cancel: &Cancel,
    limit: Duration,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let end = Instant::now() + limit;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return Err(MediaError::Cancelled);
                }
                if Instant::now() >= end {
                    return Err(MediaError::Timeout);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(MediaError::Transport(
                    "a requisição terminou sem resposta".into(),
                ))
            }
        }
    }
}

fn sleep_cancellable(d: Duration, cancel: &Cancel) -> Result<()> {
    let end = Instant::now() + d;
    loop {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        std::thread::sleep((end - now).min(Duration::from_millis(50)));
    }
}

fn status_error(code: u16, resp: ureq::Response) -> MediaError {
    let retry = resp
        .header("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| *s >= 0.0)
        .map(|s| (s * 1000.0) as u64);
    let mut raw = Vec::new();
    let _ = resp.into_reader().take(64 << 10).read_to_end(&mut raw);
    let body = lossy(&raw);
    match code {
        429 => MediaError::RateLimited {
            retry_after_ms: retry,
            body: clip(&body, 600),
        },
        402 => MediaError::Payment(payment_message(&body)),
        _ => MediaError::Http {
            status: code,
            body: clip(&body, 600),
        },
    }
}

fn payment_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let meta = &v["error"]["metadata"];
    let why = ["limit_source", "reason"]
        .iter()
        .filter_map(|k| meta[*k].as_str().map(|s| format!("{k}: {s}")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "créditos insuficientes ou limite de gasto da chave atingido — {}{}",
        error_message(body),
        if why.is_empty() {
            String::new()
        } else {
            format!(" ({why})")
        }
    )
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Streams a body through `sink` in chunks, checking `cancel` between reads
/// and refusing more than `cap` bytes.
fn pump(
    resp: ureq::Response,
    cap: u64,
    cancel: &Cancel,
    mut sink: impl FnMut(&[u8]) -> Result<()>,
) -> Result<u64> {
    let too_big =
        || MediaError::Invalid(format!("a resposta passou de {} MB; recusada", cap >> 20));
    if resp
        .header("content-length")
        .and_then(|l| l.trim().parse::<u64>().ok())
        .is_some_and(|l| l > cap)
    {
        return Err(too_big());
    }
    let mut r = resp.into_reader().take(cap + 1);
    let mut buf = vec![0u8; 64 << 10];
    let mut total = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let n = r
            .read(&mut buf)
            .map_err(|e| MediaError::Transport(e.to_string()))?;
        if n == 0 {
            return Ok(total);
        }
        total += n as u64;
        if total > cap {
            return Err(too_big());
        }
        sink(&buf[..n])?;
    }
}

fn read_capped(resp: ureq::Response, cap: u64, cancel: &Cancel) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    pump(resp, cap, cancel, |c| {
        out.extend_from_slice(c);
        Ok(())
    })?;
    Ok(out)
}

// --- parsing ------------------------------------------------------------------

fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn parse_descriptor(name: &str, d: &Value) -> ParamKind {
    match d["type"].as_str() {
        Some("enum") => ParamKind::Enum(
            d["values"]
                .as_array()
                .map(|a| a.iter().map(render).collect())
                .unwrap_or_default(),
        ),
        Some("range") => match (d["min"].as_f64(), d["max"].as_f64()) {
            (Some(min), Some(max)) => ParamKind::Range { min, max },
            _ => ParamKind::Other,
        },
        // For `seed` and `size` the descriptor's boolean means "supported";
        // the value itself is a number / "WxH". Reported as free text.
        Some("boolean") if name == "seed" || name == "size" => ParamKind::Text,
        Some("boolean") => ParamKind::Flag,
        _ => ParamKind::Other,
    }
}

/// Every entry of a typed list (`{"data": [...]}`) that has an id, once per
/// id, newest first (`created` descending; ties keep the API's order).
pub fn parse_list(kind: Kind, v: &Value) -> Result<Vec<MediaModel>> {
    let data = v["data"]
        .as_array()
        .ok_or_else(|| MediaError::Malformed("lista de modelos sem `data`".into()))?;
    let mut seen = std::collections::HashSet::new();
    let mut models: Vec<MediaModel> = data
        .iter()
        .filter_map(|m| parse_model(kind, m))
        .filter(|m| seen.insert(m.id.clone()))
        .collect();
    models.sort_by_key(|m| std::cmp::Reverse(m.created));
    Ok(models)
}

fn parse_model(kind: Kind, m: &Value) -> Option<MediaModel> {
    let id = m["id"]
        .as_str()
        .map(str::trim)
        .filter(|i| !i.is_empty())?
        .to_string();
    let mut model = MediaModel {
        name: m["name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&id)
            .to_string(),
        description: m["description"].as_str().unwrap_or("").to_string(),
        created: num(&m["created"]).map_or(0, |x| x as i64),
        id,
        kind,
        price_hint: String::new(),
        params: Vec::new(),
        max_references: 0,
        frame_images: Vec::new(),
    };
    match kind {
        Kind::Image => {
            for (name, d) in m["supported_parameters"].as_object().into_iter().flatten() {
                if name == "input_references" {
                    model.max_references = d["max"].as_u64().unwrap_or(0) as u32;
                } else {
                    model.params.push(ParamSpec {
                        name: name.clone(),
                        kind: parse_descriptor(name, d),
                    });
                }
            }
        }
        Kind::Video => {
            // `-1` in `supported_durations` means "the model decides", the
            // same as sending no duration, so it is not offered as a value.
            let list = |key: &str| -> Option<Vec<String>> {
                m[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|v| !v.is_null() && num(v) != Some(-1.0))
                            .map(render)
                            .collect::<Vec<_>>()
                    })
                    .filter(|a| !a.is_empty())
            };
            for (name, key) in [
                ("duration", "supported_durations"),
                ("resolution", "supported_resolutions"),
                ("aspect_ratio", "supported_aspect_ratios"),
                ("size", "supported_sizes"),
            ] {
                if let Some(values) = list(key) {
                    model.params.push(ParamSpec {
                        name: name.into(),
                        kind: ParamKind::Enum(values),
                    });
                }
            }
            if m["generate_audio"].as_bool() == Some(true) {
                model.params.push(ParamSpec {
                    name: "generate_audio".into(),
                    kind: ParamKind::Flag,
                });
            }
            if m["seed"].as_bool() == Some(true) {
                model.params.push(ParamSpec {
                    name: "seed".into(),
                    kind: ParamKind::Text,
                });
            }
            model.frame_images = list("supported_frame_images").unwrap_or_default();
            model.price_hint = video_price_hint(&m["pricing_skus"]);
        }
    }
    Some(model)
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `US$ 0,05`, `US$ 0,0875`, `US$ 1,20`: pt-BR comma, at least two decimals,
/// at most four.
fn fmt_usd(x: f64) -> String {
    let s = if x >= 1.0 {
        format!("{x:.2}")
    } else {
        format!("{x:.4}")
    };
    let (int, dec) = s.split_once('.').unwrap_or((&s, "00"));
    let mut dec = dec.trim_end_matches('0').to_string();
    while dec.len() < 2 {
        dec.push('0');
    }
    format!("US$ {int},{dec}")
}

/// Most price lines carry a few variants; more than this is cut to `+N`.
const PRICE_PARTS: usize = 3;

/// `(price, variant)` pairs, in display order, as `a · b (x / y) · +2`.
/// Variants sharing one price are merged; a price that also exists without a
/// variant is shown plain.
fn join_parts(parts: Vec<(String, String)>) -> String {
    let mut merged: Vec<(String, Vec<String>, bool)> = Vec::new();
    for (price, label) in parts {
        let i = match merged.iter().position(|(p, ..)| *p == price) {
            Some(i) => i,
            None => {
                merged.push((price, Vec::new(), false));
                merged.len() - 1
            }
        };
        let (_, labels, plain) = &mut merged[i];
        if label.is_empty() {
            *plain = true;
        } else if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let extra = merged.len().saturating_sub(PRICE_PARTS);
    let mut out: Vec<String> = merged
        .into_iter()
        .take(PRICE_PARTS)
        .map(|(price, labels, plain)| {
            if plain || labels.is_empty() {
                price
            } else {
                format!("{price} ({})", labels.join(" / "))
            }
        })
        .collect();
    if extra > 0 {
        out.push(format!("+{extra}"));
    }
    out.join(" · ")
}

/// `2k` -> `2K`, `low_1k` -> `low 1K`, `high_resolution` -> `high
/// resolution`, `720p` as is.
fn variant_label(v: &str) -> String {
    v.trim()
        .split('_')
        .map(|w| {
            if w.ends_with('k') && w[..w.len() - 1].parse::<f64>().is_ok() {
                w.to_uppercase()
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fallback from the generic `/models` listing: `pricing.image_output` is
/// USD per generated image token. (`pricing.image` there is the price of an
/// INPUT image, so it is never shown as the price of a picture.)
fn image_price_hint(id: &str, p: &Value) -> String {
    if let Some(x) = num(&p["image_output"]).filter(|x| *x > 0.0) {
        return format!("{} por 1M tokens de imagem gerada", fmt_usd(x * 1e6));
    }
    if id.ends_with(":free") {
        return "grátis".into();
    }
    String::new()
}

/// Price of a generated image from `/images/models/{id}/endpoints`: the
/// first endpoint's `output_image` entries, per image, per megapixel or per
/// token, with their variant (`2K`, `high resolution`). Surcharges per
/// request and input prices are left out. Empty when there is nothing.
fn image_endpoint_price(endpoints: &Value) -> String {
    let Some(ep) = endpoints.as_array().and_then(|a| {
        a.iter().find(|e| {
            e["pricing"]
                .as_array()
                .is_some_and(|p| p.iter().any(|x| x["billable"] == "output_image"))
        })
    }) else {
        return String::new();
    };
    let out: Vec<&Value> = ep["pricing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| x["billable"] == "output_image")
        .filter(|x| matches!(x["unit"].as_str(), Some("image" | "megapixel" | "token")))
        .collect();
    if out.is_empty() {
        return String::new();
    }
    if out.iter().all(|x| num(&x["cost_usd"]) == Some(0.0)) {
        return "grátis".into();
    }
    let mut parts: Vec<(String, String)> = out
        .iter()
        .filter_map(|x| {
            let c = num(&x["cost_usd"]).filter(|c| *c > 0.0)?;
            let variant = x["variant"].as_str().map(variant_label).unwrap_or_default();
            let text = match x["unit"].as_str()? {
                "image" => format!("{}/imagem", fmt_usd(c)),
                // 1024x1024 is 1.048576 MP: the usual square, as a reference.
                "megapixel" => format!(
                    "{}/megapixel (~{} a 1024×1024)",
                    fmt_usd(c),
                    fmt_usd(c * 1.048_576)
                ),
                _ => format!("{} por 1M tokens de imagem gerada", fmt_usd(c * 1e6)),
            };
            Some((text, variant))
        })
        .collect();
    // The plain price first, then the variants in the API's order.
    parts.sort_by_key(|(_, v)| !v.is_empty());
    join_parts(parts)
}

/// One readable SKU of a video model.
struct Sku {
    /// USD per unit (per token for the token SKUs).
    usd: f64,
    /// `/s`, `/megapixel·s` or `por 1M tokens de vídeo`.
    unit: &'static str,
    label: String,
    /// A variant for a special case (long clips, references, continuation,
    /// per input type) rather than the plain price.
    secondary: bool,
}

/// Words of a SKU key around its unit, as pt-BR: `with_audio_4k` ->
/// `com áudio, 4K`. Unknown words pass through.
fn sku_label(rest: &str) -> String {
    const PHRASES: [(&[&str], &str); 8] = [
        (&["text", "to", "video"], "texto→vídeo"),
        (&["image", "to", "video"], "imagem→vídeo"),
        (&["with", "video", "input"], "com vídeo de entrada"),
        (&["video", "continuation"], "continuação"),
        (&["with", "audio"], "com áudio"),
        (&["without", "audio"], "sem áudio"),
        (&["long"], "longo"),
        (&["reference"], "com referência"),
    ];
    let words: Vec<&str> = rest.split('_').filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        match PHRASES.iter().find(|(p, _)| words[i..].starts_with(p)) {
            Some((p, text)) => {
                out.push((*text).to_string());
                i += p.len();
            }
            None => {
                out.push(variant_label(words[i]));
                i += 1;
            }
        }
    }
    out.join(", ")
}

/// Splits a `pricing_skus` key into unit and variant. `None` for what is not
/// the price of the generated video (input images, minimum charge).
fn parse_sku(key: &str, value: &Value) -> Option<Sku> {
    // (unit marker in the key, USD divisor, unit shown); first match wins.
    const FORMS: [(&str, f64, &str); 6] = [
        ("cents_per_megapixel_second", 100.0, "/megapixel·s"),
        ("cents_per_video_output_second", 100.0, "/s"),
        ("cents_per_second_output", 100.0, "/s"),
        ("cents_per_second", 100.0, "/s"),
        ("duration_seconds", 1.0, "/s"),
        ("video_tokens", 1.0, " por 1M tokens de vídeo"),
    ];
    let x = num(value).filter(|x| *x > 0.0)?;
    let k = key.to_ascii_lowercase();
    if k.starts_with("minimum") || k.contains("image_input") || k.ends_with("images") {
        return None;
    }
    let secondary = [
        "long",
        "reference",
        "continuation",
        "text_to",
        "image_to",
        "video_input",
    ]
    .iter()
    .any(|w| k.contains(w));
    FORMS.iter().find_map(|(marker, div, unit)| {
        let pos = k.find(marker)?;
        Some(Sku {
            usd: x / div,
            unit,
            label: sku_label(&format!("{}_{}", &k[..pos], &k[pos + marker.len()..])),
            secondary,
        })
    })
}

/// Price line of a video model from its `pricing_skus`, cheapest first:
/// `US$ 0,05/s (1080p) · US$ 0,0875/s (2K)`. Per-token SKUs are shown raw
/// and labelled as such (the token count of a clip is the provider's).
/// Special-case variants (long, reference, continuation) are shown only
/// when there is no plain price. A minimum charge is appended.
fn video_price_hint(skus: &Value) -> String {
    let Some(map) = skus.as_object() else {
        return String::new();
    };
    let all: Vec<Sku> = map.iter().filter_map(|(k, v)| parse_sku(k, v)).collect();
    let mut chosen: Vec<&Sku> = if all.iter().any(|s| !s.secondary) {
        all.iter().filter(|s| !s.secondary).collect()
    } else {
        all.iter().collect()
    };
    chosen.sort_by(|a, b| {
        (a.unit, a.usd)
            .partial_cmp(&(b.unit, b.usd))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let parts = chosen
        .into_iter()
        .map(|s| {
            let usd = if s.unit.starts_with(" por 1M") {
                fmt_usd(s.usd * 1e6)
            } else {
                fmt_usd(s.usd)
            };
            (format!("{usd}{}", s.unit), s.label.clone())
        })
        .collect();
    let mut line = join_parts(parts);
    if let Some(min) = map
        .iter()
        .find(|(k, _)| k.starts_with("minimum_cents"))
        .and_then(|(_, v)| num(v))
        .filter(|x| *x > 0.0)
    {
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(&format!("mínimo {} por vídeo", fmt_usd(min / 100.0)));
    }
    line
}

// --- parameter planning ---------------------------------------------------------

fn plan_params(
    model: &MediaModel,
    wanted: &BTreeMap<String, Value>,
) -> Result<(BTreeMap<String, Value>, Vec<String>)> {
    let mut sent = BTreeMap::new();
    let mut notes = Vec::new();
    for (name, value) in wanted {
        let Some(spec) = model.params.iter().find(|p| &p.name == name) else {
            notes.push(format!(
                "parâmetro '{name}' ignorado: o modelo {} não o suporta",
                model.id
            ));
            continue;
        };
        sent.insert(name.clone(), check_value(model, spec, value)?);
    }
    Ok((sent, notes))
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn check_value(model: &MediaModel, spec: &ParamSpec, v: &Value) -> Result<Value> {
    let bad = |why: String| {
        MediaError::Invalid(format!(
            "parâmetro '{}' inválido para {}: {why}",
            spec.name, model.id
        ))
    };
    let typed = |s: &str| -> Value {
        if INT_PARAMS.contains(&spec.name.as_str()) {
            s.parse::<i64>()
                .map(Value::from)
                .unwrap_or_else(|_| json!(s))
        } else {
            json!(s)
        }
    };
    match &spec.kind {
        ParamKind::Enum(values) => {
            let r = render(v);
            if values.contains(&r) {
                Ok(typed(&r))
            } else {
                Err(bad(format!(
                    "'{r}' não é aceito; opções: {}",
                    values.join(", ")
                )))
            }
        }
        ParamKind::Range { min, max } => {
            let x = num(v).ok_or_else(|| bad(format!("'{}' não é um número", render(v))))?;
            if x < *min || x > *max {
                return Err(bad(format!("{x} fora do intervalo {min}..{max}")));
            }
            if INT_PARAMS.contains(&spec.name.as_str()) || x.fract() == 0.0 {
                Ok(json!(x as i64))
            } else {
                Ok(json!(x))
            }
        }
        ParamKind::Flag => match v {
            Value::Bool(_) => Ok(v.clone()),
            Value::String(s) if s == "true" || s == "false" => Ok(json!(s == "true")),
            _ => Err(bad("esperava true ou false".into())),
        },
        ParamKind::Text if spec.name == "seed" => as_int(v)
            .map(Value::from)
            .ok_or_else(|| bad("esperava um número inteiro".into())),
        ParamKind::Text if spec.name == "size" => {
            let s = render(v);
            let ok = s
                .split_once('x')
                .is_some_and(|(w, h)| w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok());
            if ok {
                Ok(json!(s))
            } else {
                Err(bad("esperava LARGURAxALTURA, por exemplo 1024x1024".into()))
            }
        }
        ParamKind::Text | ParamKind::Other => Ok(v.clone()),
    }
}

// --- files ------------------------------------------------------------------------

/// A file-name stem from a free-form hint: lowercase ASCII letters, digits and
/// `-`, at most 40 chars. No separators, dots or extension can survive, so the
/// result cannot leave the output directory. Empty input gives `fallback`.
pub fn sanitize_filename(hint: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in hint.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        fallback.to_string()
    } else {
        out.to_string()
    }
}

fn stem_for(hint: Option<&str>, fallback: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}-{secs}", sanitize_filename(hint.unwrap_or(""), fallback))
}

/// Creates `<dir>/<stem>[-k].<ext>`, never overwriting.
fn create_unique(dir: &Path, stem: &str, ext: &str) -> Result<(PathBuf, std::fs::File)> {
    let io = |e: std::io::Error| {
        MediaError::Transport(format!("não consegui gravar em {}: {e}", dir.display()))
    };
    std::fs::create_dir_all(dir).map_err(io)?;
    for k in 0..1000 {
        let name = if k == 0 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{k}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(e)),
        }
    }
    Err(MediaError::Transport("nomes de arquivo esgotados".into()))
}

fn ext_for(media_type: &str) -> Option<&'static str> {
    match media_type.split(';').next().unwrap_or("").trim() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/svg+xml" => Some("svg"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        _ => None,
    }
}

/// Media type from the first bytes.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.len() > 12 && &b[4..8] == b"ftyp" {
        Some("video/mp4")
    } else if b.starts_with(b"<svg") || b.starts_with(b"<?xml") {
        Some("image/svg+xml")
    } else {
        None
    }
}

/// An http(s) or `data:image/` URL is passed through; anything else is a local
/// file, which must really be a png/jpeg/webp/gif (checked by content, so a
/// text file such as `.env` can never be uploaded this way) up to
/// [`MAX_REFERENCE_BYTES`].
pub fn reference_to_url(r: &str) -> Result<String> {
    let r = r.trim();
    let lower = r.to_ascii_lowercase();
    if lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("data:image/")
    {
        return Ok(r.to_string());
    }
    let bad = |why: String| MediaError::Invalid(format!("imagem de referência '{r}': {why}"));
    let meta = std::fs::metadata(r).map_err(|e| bad(format!("não consegui ler ({e})")))?;
    if !meta.is_file() || meta.len() > MAX_REFERENCE_BYTES {
        return Err(bad(format!(
            "precisa ser um arquivo de até {} MB",
            MAX_REFERENCE_BYTES >> 20
        )));
    }
    let bytes = std::fs::read(r).map_err(|e| bad(format!("não consegui ler ({e})")))?;
    match sniff(&bytes) {
        Some(mt @ ("image/png" | "image/jpeg" | "image/webp" | "image/gif")) => {
            Ok(format!("data:{mt};base64,{}", b64_encode(&bytes)))
        }
        _ => Err(bad("não é uma imagem png, jpeg, webp ou gif".into())),
    }
}

// --- base64 (RFC 4648; stdlib has none) -----------------------------------------------

/// Decodes standard or URL-safe base64, ignoring whitespace and padding. A
/// leading `data:...,` prefix is dropped. `None` on any other character.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let s = if s.starts_with("data:") {
        s.split_once(',')?.1
    } else {
        s
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn b64_encode(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_all_lengths_and_rejects_garbage() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data, "len {len}");
        }
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_decode("TWE").unwrap(), b"Ma");
        assert_eq!(b64_decode("data:image/png;base64,TWFu").unwrap(), b"Man");
        assert_eq!(b64_decode("TW\nFu").unwrap(), b"Man");
        assert!(b64_decode("TW*u").is_none());
    }

    #[test]
    fn filenames_cannot_escape_or_carry_an_extension() {
        assert_eq!(sanitize_filename("../../etc/passwd", "x"), "etc-passwd");
        assert_eq!(
            sanitize_filename("C:\\Windows\\a.exe", "x"),
            "c-windows-a-exe"
        );
        assert_eq!(sanitize_filename("Gato  Preto!!", "x"), "gato-preto");
        assert_eq!(sanitize_filename("  ...  ", "imagem"), "imagem");
        assert_eq!(sanitize_filename("ação", "x"), "a-o");
        assert!(sanitize_filename(&"a".repeat(200), "x").len() <= 40);
    }

    #[test]
    fn descriptors_become_param_specs() {
        let d = |v: Value| parse_descriptor("n", &v);
        assert_eq!(
            d(json!({"type":"enum","values":["1:1",5]})),
            ParamKind::Enum(vec!["1:1".into(), "5".into()])
        );
        assert_eq!(
            d(json!({"type":"range","min":1,"max":6})),
            ParamKind::Range { min: 1.0, max: 6.0 }
        );
        assert_eq!(d(json!({"type":"boolean"})), ParamKind::Flag);
        assert_eq!(
            parse_descriptor("seed", &json!({"type":"boolean"})),
            ParamKind::Text
        );
        assert_eq!(d(json!({"type":"weird"})), ParamKind::Other);
        assert_eq!(d(json!({})), ParamKind::Other);
    }

    #[test]
    fn usd_is_written_the_pt_br_way() {
        assert_eq!(fmt_usd(0.05), "US$ 0,05");
        assert_eq!(fmt_usd(0.0875), "US$ 0,0875");
        assert_eq!(fmt_usd(0.1), "US$ 0,10");
        assert_eq!(fmt_usd(1.2), "US$ 1,20");
        assert_eq!(fmt_usd(7.0), "US$ 7,00");
        assert_eq!(fmt_usd(0.023172), "US$ 0,0232");
    }

    /// Fallback from `/models`: only `image_output` (per token) is a price
    /// of the result; `image` there is an input price and is ignored.
    #[test]
    fn generic_listing_price_never_mistakes_the_input_price() {
        assert_eq!(
            image_price_hint(
                "bytedance-seed/seedream-5-5",
                &json!({"image":"0.008","image_output":"0.00000479041916167665"})
            ),
            "US$ 4,79 por 1M tokens de imagem gerada"
        );
        assert_eq!(image_price_hint("a", &json!({"image":"0.008"})), "");
        assert_eq!(image_price_hint("a:free", &json!({})), "grátis");
        assert_eq!(image_price_hint("a", &Value::Null), "");
    }

    /// Shapes of `/images/models/{id}/endpoints` seen on 2026-11-24.
    #[test]
    fn image_price_per_image_megapixel_and_token() {
        // Per image with variants and an input price (left out).
        let seedream = json!([{"pricing": [
            {"billable": "output_image", "unit": "image", "cost_usd": 0.02},
            {"billable": "output_image", "unit": "image", "cost_usd": 0.035, "variant": "high_resolution"},
            {"billable": "input_image", "unit": "image", "cost_usd": 0.008}]}]);
        assert_eq!(
            image_endpoint_price(&seedream),
            "US$ 0,02/imagem · US$ 0,035/imagem (high resolution)"
        );
        // Variants only, plus a per-request surcharge (left out).
        let krea = json!([{"pricing": [
            {"billable": "output_image", "unit": "image", "cost_usd": 0.03, "variant": "1k"},
            {"billable": "output_image", "unit": "image", "cost_usd": 0.06, "variant": "2k"},
            {"billable": "output_image", "unit": "request", "cost_usd": 0.02, "variant": "high_thinking_surcharge"}]}]);
        assert_eq!(
            image_endpoint_price(&krea),
            "US$ 0,03/imagem (1K) · US$ 0,06/imagem (2K)"
        );
        // One price for several variants is one entry.
        let qwen = json!([{"pricing": [
            {"billable": "output_image", "unit": "image", "cost_usd": 0.03, "variant": "1k"},
            {"billable": "output_image", "unit": "image", "cost_usd": 0.03, "variant": "2k"},
            {"billable": "output_image", "unit": "image", "cost_usd": 0.04, "variant": "low_2k"}]}]);
        assert_eq!(
            image_endpoint_price(&qwen),
            "US$ 0,03/imagem (1K / 2K) · US$ 0,04/imagem (low 2K)"
        );
        // Per megapixel, with the 1024x1024 equivalent.
        let flux = json!([{"pricing": [
            {"billable": "output_image", "unit": "megapixel", "cost_usd": 0.03}]}]);
        assert_eq!(
            image_endpoint_price(&flux),
            "US$ 0,03/megapixel (~US$ 0,0315 a 1024×1024)"
        );
        // Per token: raw, labelled.
        let gpt = json!([{"pricing": [
            {"billable": "input_text", "unit": "token", "cost_usd": 0.000005},
            {"billable": "output_image", "unit": "token", "cost_usd": 0.00003}]}]);
        assert_eq!(
            image_endpoint_price(&gpt),
            "US$ 30,00 por 1M tokens de imagem gerada"
        );
        // Free twin, empty list, odd shapes.
        let free =
            json!([{"pricing": [{"billable": "output_image", "unit": "image", "cost_usd": 0}]}]);
        assert_eq!(image_endpoint_price(&free), "grátis");
        assert_eq!(image_endpoint_price(&json!([])), "");
        assert_eq!(image_endpoint_price(&Value::Null), "");
        assert_eq!(image_endpoint_price(&json!([{"pricing": null}])), "");
        // More than three variants are cut to +N.
        let many = json!([{"pricing": (1..=5).map(|i| json!(
            {"billable": "output_image", "unit": "image", "cost_usd": i as f64 / 100.0, "variant": format!("v{i}")}
        )).collect::<Vec<_>>()}]);
        assert!(image_endpoint_price(&many).ends_with(" · +2"));
    }

    /// One case per SKU family of `/videos/models` seen on 2026-11-24.
    #[test]
    fn video_price_from_every_sku_family() {
        // USD per second by resolution, cheapest first.
        assert_eq!(
            video_price_hint(
                &json!({"duration_seconds_2k": "0.0875", "duration_seconds_1080p": "0.05"})
            ),
            "US$ 0,05/s (1080p) · US$ 0,0875/s (2K)"
        );
        // Cents per second; the input-image SKU is not a price of the video.
        assert_eq!(
            video_price_hint(&json!({"cents_per_image_input": "1",
                "cents_per_video_output_second_480p": "2",
                "cents_per_video_output_second_720p": "3",
                "cents_per_video_output_second_1080p": "14"})),
            "US$ 0,02/s (480p) · US$ 0,03/s (720p) · US$ 0,14/s (1080p)"
        );
        // Plain per-second price plus a minimum charge.
        assert_eq!(
            video_price_hint(
                &json!({"cents_per_second_output": "28", "minimum_cents_per_generation": "56"})
            ),
            "US$ 0,28/s · mínimo US$ 0,56 por vídeo"
        );
        // Per video token: raw value, clearly labelled; special cases hidden.
        assert_eq!(
            video_price_hint(&json!({"video_tokens": "0.000007",
                "video_tokens_without_audio": "0.000007",
                "video_tokens_with_video_input": "0.0000043"})),
            "US$ 7,00 por 1M tokens de vídeo"
        );
        // Audio and resolution in one key.
        assert_eq!(
            video_price_hint(&json!({"duration_seconds_with_audio": "0.40",
                "duration_seconds_with_audio_4k": "0.60",
                "duration_seconds_without_audio": "0.20"})),
            "US$ 0,20/s (sem áudio) · US$ 0,40/s (com áudio) · US$ 0,60/s (com áudio, 4K)"
        );
        // Per megapixel-second (upscaler): only variants exist, so they show.
        assert_eq!(
            video_price_hint(&json!({"cents_per_megapixel_second_precise": "3.5",
                "cents_per_megapixel_second_creative": "5"})),
            "US$ 0,035/megapixel·s (precise) · US$ 0,05/megapixel·s (creative)"
        );
        // Reference images are an input price.
        assert_eq!(
            video_price_hint(&json!({"duration_seconds": "0.13", "reference_images": "0.04"})),
            "US$ 0,13/s"
        );
        // Long/reference variants hide behind the plain prices.
        assert_eq!(
            video_price_hint(&json!({"duration_seconds_480p": "0.02",
                "long_duration_seconds_480p": "0.045",
                "reference_duration_seconds_480p": "0.07"})),
            "US$ 0,02/s (480p)"
        );
        assert_eq!(video_price_hint(&json!({"weird": "1"})), "");
        assert_eq!(video_price_hint(&json!({"duration_seconds": "0"})), "");
        assert_eq!(video_price_hint(&Value::Null), "");
    }

    #[test]
    fn debug_never_prints_the_key() {
        let o = OpenRouter::new(Some("sk-or-secret".into()), None);
        assert!(!format!("{o:?}").contains("secret"));
        let e = MediaError::Transport("falha em sk-or-secret".into()).scrub(Some("sk-or-secret"));
        assert!(!e.to_string().contains("secret"));
    }
}
