//! `generate_image` and `generate_video`: the user picks the model in the
//! settings; the chat model writes the prompt and picks the parameters the
//! request calls for (duration, aspect ratio, resolution...), within what
//! that model accepts. The schema lists exactly those parameters, with their
//! values, from the last OpenRouter list saved on disk; without one, a free
//! set is offered and the API validates. Values are checked against the live
//! descriptor before any paid request.
//!
//! The tools exist only when `media.image.model` / `media.video.model` is set,
//! so a model never sees a tool it cannot use. Both carry the policy action
//! `Network(..)`: they cost money, so the policy asks.
//!
//! Limits (see `zerith_providers::media`): image 330 s per call, video 15 min
//! per call (the provider is polled for at most 10 min, plus submit and
//! download), image reply 50 MB, video file 500 MB. Files are written only
//! under `<output_dir>/<session or date>/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zerith_core::{secrets, MediaSettings, Paths, Settings};
use zerith_providers::media::{
    cached_model, sanitize_filename, GeneratedFile, ImageRequest, Kind, MediaError, MediaModel,
    OpenRouter, ParamKind, ParamSpec, VideoRequest, INT_PARAMS, VIDEO_MAX_WAIT,
};
use zerith_security::{Action, Verdict};

use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

/// Shortest accepted prompt, in words. Raise or lower here; the refusal
/// message below is built from it.
pub const MIN_WORDS_IMAGE: usize = 12;
pub const MIN_WORDS_VIDEO: usize = 18;

/// Per-call budgets reported in the `ToolSpec` (the deadline the loop enforces).
const IMAGE_TIMEOUT_S: u64 = 330;
const VIDEO_TIMEOUT_S: u64 = 900;

/// The tools this configuration enables. Empty when no model is chosen.
pub fn tools(settings: &Settings, paths: &Paths) -> Vec<Box<dyn Tool>> {
    tools_inner(settings, paths, None)
}

/// Same, with a fixed key instead of env/keyring (tests, hosts with their own).
pub fn tools_with_key(settings: &Settings, paths: &Paths, key: String) -> Vec<Box<dyn Tool>> {
    tools_inner(settings, paths, Some(key))
}

/// What `Registry::new()` offers: the user layer of the settings plus env.
/// A broken settings file only means "no media tools", never a failed start.
pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    let paths = Paths::resolve();
    let loaded = Settings::load(&paths, None);
    tools(&loaded.settings, &paths)
}

fn tools_inner(settings: &Settings, paths: &Paths, key: Option<String>) -> Vec<Box<dyn Tool>> {
    let m = &settings.media;
    let root = m.resolve_output_dir(paths);
    let cache = MediaSettings::catalog_dir(paths);
    [Kind::Image, Kind::Video]
        .into_iter()
        .filter(|k| slot(m, *k).configured())
        .map(|kind| {
            let model = cached_model(&cache, kind, slot(m, kind).model.trim());
            let params = match &model {
                Some(d) => d.params.clone(),
                None => free_params(kind),
            };
            Box::new(MediaTool {
                spec: spec(kind, m, model.as_ref(), &params),
                kind,
                params,
                media: m.clone(),
                root: root.clone(),
                cache: cache.clone(),
                key: key.clone(),
            }) as Box<dyn Tool>
        })
        .collect()
}

/// Offered when no saved model list describes the chosen model: the common
/// names, loosely typed. The API validates the values.
fn free_params(kind: Kind) -> Vec<ParamSpec> {
    let names: &[&str] = match kind {
        Kind::Image => &[
            "aspect_ratio",
            "resolution",
            "size",
            "n",
            "quality",
            "background",
            "output_format",
            "seed",
        ],
        Kind::Video => &[
            "duration",
            "resolution",
            "aspect_ratio",
            "size",
            "generate_audio",
            "seed",
        ],
    };
    names
        .iter()
        .map(|n| ParamSpec {
            name: (*n).into(),
            kind: match *n {
                "generate_audio" => ParamKind::Flag,
                _ => ParamKind::Other,
            },
        })
        .collect()
}

/// What a parameter is for, in words the chat model maps a request onto.
fn param_doc(name: &str) -> &'static str {
    match name {
        "duration" => "duração do vídeo em segundos (\"vídeo de 8 segundos\" → 8)",
        "aspect_ratio" => {
            "proporção (\"16:9\" paisagem/YouTube, \"9:16\" vertical/Reels/TikTok, \"1:1\" quadrado)"
        }
        "resolution" => "resolução (ex.: 720p, 1080p, 2K, 4K); maior custa mais",
        "size" => "tamanho em pixels LARGURAxALTURA",
        "generate_audio" => "gerar áudio junto (sons, música, falas); pode custar mais",
        "seed" => "semente para repetir um resultado; só passe se o usuário pedir",
        "n" => "quantas imagens gerar; cada uma é cobrada",
        "quality" => "qualidade; maior custa mais",
        "background" => "fundo (transparent para recorte/logo, opaque)",
        "output_format" => "formato do arquivo (png, jpeg, webp)",
        "output_compression" => "compressão do arquivo (0-100)",
        _ => "parâmetro do modelo",
    }
}

/// JSON schema of one parameter, from its descriptor.
fn param_schema(p: &ParamSpec, default: Option<&Value>) -> Value {
    let int = INT_PARAMS.contains(&p.name.as_str());
    let mut s = match &p.kind {
        ParamKind::Enum(values) => {
            let ints: Option<Vec<i64>> = if int {
                values.iter().map(|v| v.parse().ok()).collect()
            } else {
                None
            };
            match ints {
                Some(i) => json!({"type": "integer", "enum": i}),
                None => json!({"type": "string", "enum": values}),
            }
        }
        ParamKind::Range { min, max } => json!({
            "type": if int { "integer" } else { "number" },
            "minimum": min,
            "maximum": max,
        }),
        ParamKind::Flag => json!({"type": "boolean"}),
        ParamKind::Text if p.name == "size" => {
            json!({"type": "string", "pattern": "^[0-9]+x[0-9]+$"})
        }
        _ if int => json!({"type": "integer"}),
        _ => json!({"type": "string"}),
    };
    let fallback = match default {
        Some(v) => format!("se omitido: {}", show(v)),
        None => "se omitido: padrão do modelo".into(),
    };
    s["description"] = json!(format!("opcional: {}; {fallback}", param_doc(&p.name)));
    s
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `duration (4, 6, 8)`, `n (1..4)`: repeated in the description because
/// some providers drop `enum` from tool schemas.
fn param_line(p: &ParamSpec) -> String {
    match &p.kind {
        ParamKind::Enum(v) => format!("{} ({})", p.name, v.join(", ")),
        ParamKind::Range { min, max } => format!("{} ({min}..{max})", p.name),
        ParamKind::Flag => format!("{} (true/false)", p.name),
        _ => p.name.clone(),
    }
}

fn slot(m: &MediaSettings, kind: Kind) -> &zerith_core::MediaSlot {
    match kind {
        Kind::Image => &m.image,
        Kind::Video => &m.video,
    }
}

fn spec(
    kind: Kind,
    m: &MediaSettings,
    known: Option<&MediaModel>,
    params: &[ParamSpec],
) -> ToolSpec {
    let s = slot(m, kind);
    let model = match known {
        Some(d) if d.name != d.id => format!("{}, {}", d.name, d.id),
        _ => s.model.trim().to_string(),
    };
    let accepted = if params.is_empty() {
        "este modelo não aceita parâmetros além do prompt".to_string()
    } else if known.is_some() {
        format!(
            "parâmetros aceitos por este modelo: {}",
            params.iter().map(param_line).collect::<Vec<_>>().join("; ")
        )
    } else {
        format!(
            "parâmetros comuns (a lista exata do modelo ainda não foi carregada; o OpenRouter valida): {}",
            params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let choose = format!(
        "O modelo é a escolha do usuário e não muda. VOCÊ escolhe os parâmetros conforme o pedido \
(ex.: \"vídeo de 8 segundos em 16:9\" → duration=8, aspect_ratio=\"16:9\"; \"para Reels\" → 9:16) e omite o que o usuário não pediu \
(vale o padrão). Use só valores da lista; {accepted}."
    );
    let (name, description, mut parameters, timeout_s, returns) = match kind {
        Kind::Image => (
            "generate_image",
            format!(
                "Gera uma imagem (generate image, text-to-image) com o modelo escolhido pelo usuário ({model}) e salva em arquivo. \
Use sempre que o usuário pedir uma imagem, ilustração, foto, logo, arte ou capa. COSTA DINHEIRO: chame uma vez por pedido. \
{choose} \
O `prompt` precisa ser ELABORADO, em inglês de preferência, com no mínimo {MIN_WORDS_IMAGE} palavras, cobrindo: \
subject (sujeito e detalhes), setting (cenário), composition/framing (enquadramento, ângulo), style/medium (estilo, técnica, mídia), \
lighting (iluminação), color palette (paleta de cores), mood (clima), lens/camera (lente, profundidade de campo), \
quality terms (alta resolução, detalhes nítidos) e o que evitar (negative: texto distorcido, mãos deformadas, marcas d'água). \
Prompts curtos como \"um gato\" são recusados. Para editar ou combinar imagens existentes, passe `reference_images` (URLs ou caminhos de arquivo). \
Depois de gerar, mostre a imagem ao usuário colando a linha Markdown devolvida."
            ),
            json!({"type": "object", "additionalProperties": false, "properties": {
                "prompt": {"type": "string", "description": "prompt detalhado: sujeito, cenário, enquadramento, estilo, iluminação, paleta, clima, lente, qualidade e o que evitar"},
                "reference_images": {"type": "array", "items": {"type": "string"}, "description": "opcional: URLs http(s) ou caminhos de imagens png/jpeg/webp/gif para editar ou combinar"},
                "filename_hint": {"type": "string", "description": "opcional: parte do nome do arquivo (letras e números)"}
            }, "required": ["prompt"]}),
            IMAGE_TIMEOUT_S,
            "o caminho de cada imagem gerada, o modelo, os parâmetros enviados, o custo (quando informado) e uma linha Markdown ![descrição](caminho) para exibir a imagem",
        ),
        Kind::Video => {
            let audio = params.iter().any(|p| p.name == "generate_audio");
            (
                "generate_video",
                format!(
                    "Gera um vídeo curto (generate video, text-to-video / image-to-video) com o modelo escolhido pelo usuário ({model}) e salva em arquivo. \
Use quando o usuário pedir um vídeo, animação ou clipe. É LENTO (até 10 minutos) e COSTA DINHEIRO: chame uma vez por pedido. \
{choose} \
O `prompt` precisa ser ELABORADO, com no mínimo {MIN_WORDS_VIDEO} palavras, cobrindo: subject e ação, setting (cenário), \
motion (o que se move e como), camera movement (pan, tilt, dolly, drone, câmera fixa), pacing (ritmo, câmera lenta), \
composition/framing, style/medium, lighting, color palette, mood, lens/camera, quality terms e o que evitar{}. \
Para image-to-video passe `first_frame` (e `last_frame` se o modelo aceitar): URL ou caminho de imagem.",
                    if audio { "; com generate_audio=true, descreva também sons, música e falas" } else { "" }
                ),
                json!({"type": "object", "additionalProperties": false, "properties": {
                    "prompt": {"type": "string", "description": "prompt detalhado de vídeo: sujeito, ação, movimento, câmera, ritmo, estilo, iluminação, paleta, clima, qualidade, o que evitar"},
                    "first_frame": {"type": "string", "description": "opcional: URL ou caminho da imagem do primeiro quadro"},
                    "last_frame": {"type": "string", "description": "opcional: URL ou caminho da imagem do último quadro"},
                    "filename_hint": {"type": "string", "description": "opcional: parte do nome do arquivo (letras e números)"}
                }, "required": ["prompt"]}),
                VIDEO_TIMEOUT_S,
                "o caminho do vídeo gerado, o modelo, os parâmetros enviados e o custo (quando informado)",
            )
        }
    };
    for p in params {
        parameters["properties"][p.name.as_str()] = param_schema(p, s.params.get(&p.name));
    }
    ToolSpec {
        name: name.into(),
        description,
        parameters,
        returns,
        permission: Permission::Network,
        timeout: Duration::from_secs(timeout_s),
        cancellable: true,
        local: false,
        source: "built-in".into(),
    }
}

struct MediaTool {
    spec: ToolSpec,
    kind: Kind,
    /// The generation parameters the schema offers.
    params: Vec<ParamSpec>,
    media: MediaSettings,
    root: PathBuf,
    /// Where the model lists are saved (see `cached_model`).
    cache: PathBuf,
    key: Option<String>,
}

impl MediaTool {
    fn base_args(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::Image => &["prompt", "reference_images", "filename_hint"],
            Kind::Video => &["prompt", "first_frame", "last_frame", "filename_hint"],
        }
    }

    /// Rejects anything the schema does not list: the model must not be able
    /// to swap the `model` or send a parameter this model does not take.
    fn check_args<'a>(&self, args: &'a Value) -> Result<&'a serde_json::Map<String, Value>> {
        let obj = args.as_object().ok_or_else(|| {
            ToolError::BadArguments("os argumentos precisam ser um objeto".into())
        })?;
        let known =
            |k: &str| self.base_args().contains(&k) || self.params.iter().any(|p| p.name == k);
        if let Some(k) = obj.keys().find(|k| !known(k)) {
            let mut all: Vec<&str> = self.base_args().to_vec();
            all.extend(self.params.iter().map(|p| p.name.as_str()));
            return Err(ToolError::BadArguments(format!(
                "argumento '{k}' não existe nesta ferramenta (o modelo é escolha do usuário e não muda); aceitos: {}",
                all.join(", ")
            )));
        }
        Ok(obj)
    }

    /// The user's optional defaults, overridden by what the chat model chose.
    fn params(
        &self,
        args: &serde_json::Map<String, Value>,
    ) -> std::collections::BTreeMap<String, Value> {
        let mut out = slot(&self.media, self.kind).params.clone();
        for p in &self.params {
            if let Some(v) = args.get(&p.name).filter(|v| !v.is_null()) {
                out.insert(p.name.clone(), v.clone());
            }
        }
        out
    }

    fn client(&self) -> Result<OpenRouter> {
        let key = match &self.key {
            Some(k) => Some(k.clone()),
            None => {
                secrets::resolve_openrouter_key().map_err(|e| ToolError::Failed(e.to_string()))?
            }
        };
        let base = Some(self.media.base_url.clone()).filter(|b| !b.trim().is_empty());
        Ok(OpenRouter::new(key, base).with_disk_cache(self.cache.clone()))
    }

    fn out_dir(&self, exec: &Exec<'_>) -> PathBuf {
        let day = today();
        let sub = match exec.host.session() {
            Some(s) => sanitize_filename(&s, &day),
            None => day,
        };
        self.root.join(sub)
    }

    /// A reference may be a URL, or a local file inside the workspace or the
    /// media folder (so a generated image can be edited again). Anything else
    /// would let the model upload arbitrary files to a third party.
    fn reference(&self, r: &str, ctx: &CallCtx<'_>) -> Result<String> {
        let lower = r.trim().to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("data:image/")
        {
            return Ok(r.trim().to_string());
        }
        let path = ctx.resolve(r.trim());
        let inside_media = match (path.canonicalize(), self.root.canonicalize()) {
            (Ok(p), Ok(root)) => p.starts_with(root),
            _ => false,
        };
        if !inside_media
            && !matches!(
                ctx.policy
                    .check_tool("read_file", &Action::Read(path.clone())),
                Verdict::Allow
            )
        {
            return Err(ToolError::Denied(format!(
                "a imagem de referência {} está fora do workspace e da pasta de mídia",
                path.display()
            )));
        }
        Ok(path.to_string_lossy().into_owned())
    }

    fn hint(args: &serde_json::Map<String, Value>) -> Option<String> {
        args.get("filename_hint")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

impl Tool for MediaTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn action(&self, args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
        self.check_args(args)?;
        let model = &slot(&self.media, self.kind).model;
        let base = Some(self.media.base_url.trim())
            .filter(|b| !b.is_empty())
            .unwrap_or(zerith_providers::media::DEFAULT_BASE_URL);
        Ok(Some(Action::Network(format!(
            "{base} — gerar {} com {model} (gera custo)",
            match self.kind {
                Kind::Image => "imagem",
                Kind::Video => "vídeo",
            }
        ))))
    }

    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        let obj = self.check_args(args)?;
        let prompt = obj
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ToolError::BadArguments("prompt precisa ser uma string".into()))?;
        let min = match self.kind {
            Kind::Image => MIN_WORDS_IMAGE,
            Kind::Video => MIN_WORDS_VIDEO,
        };
        quality_gate(prompt, min, self.kind)?;
        exec.check()?;

        let s = slot(&self.media, self.kind);
        let or = self.client()?;
        let out_dir = self.out_dir(exec);
        let (files, plan_model, sent, notes) = match self.kind {
            Kind::Image => {
                let refs = match obj.get("reference_images") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| {
                                    ToolError::BadArguments(
                                        "reference_images só aceita textos".into(),
                                    )
                                })
                                .and_then(|r| self.reference(r, ctx))
                        })
                        .collect::<Result<Vec<_>>>()?,
                    Some(_) => {
                        return Err(ToolError::BadArguments(
                            "reference_images precisa ser uma lista de textos".into(),
                        ))
                    }
                };
                let req = ImageRequest {
                    model: s.model.clone(),
                    prompt: prompt.to_string(),
                    params: self.params(obj),
                    reference_images: refs,
                    filename_hint: Self::hint(obj),
                };
                let plan = or.plan_image(&req).map_err(map_err)?;
                let files = or
                    .generate_image(&req, &out_dir, exec.cancel)
                    .map_err(map_err)?;
                (files, plan.model.id, plan.sent, plan.notes)
            }
            Kind::Video => {
                let frame = |k: &str| -> Result<Option<String>> {
                    match obj.get(k) {
                        None | Some(Value::Null) => Ok(None),
                        Some(Value::String(r)) => self.reference(r, ctx).map(Some),
                        Some(_) => {
                            Err(ToolError::BadArguments(format!("{k} precisa ser um texto")))
                        }
                    }
                };
                let req = VideoRequest {
                    model: s.model.clone(),
                    prompt: prompt.to_string(),
                    params: self.params(obj),
                    first_frame: frame("first_frame")?,
                    last_frame: frame("last_frame")?,
                    filename_hint: Self::hint(obj),
                    ..Default::default()
                };
                let plan = or.plan_video(&req).map_err(map_err)?;
                // Poll no longer than the call's own deadline allows.
                let left = exec
                    .deadline
                    .saturating_duration_since(std::time::Instant::now());
                let or = or.with_max_wait(
                    VIDEO_MAX_WAIT.min(left.saturating_sub(Duration::from_secs(120))),
                );
                let f = or
                    .generate_video(&req, &out_dir, exec.cancel, &mut |_| {})
                    .map_err(map_err)?;
                (vec![f], plan.model.id, plan.sent, plan.notes)
            }
        };
        Ok(report(
            self.kind,
            prompt,
            &plan_model,
            &sent,
            &notes,
            &files,
            &self.root,
        ))
    }
}

fn map_err(e: MediaError) -> ToolError {
    match e {
        MediaError::Cancelled => ToolError::Cancelled,
        // A value outside the model's list: the message names the accepted
        // ones, so the chat model can fix its own argument.
        MediaError::Invalid(m) if m.starts_with("parâmetro '") => ToolError::BadArguments(format!(
            "{m}. Chame de novo com um dos valores aceitos (o mais próximo do que o usuário pediu) e diga a ele o que mudou."
        )),
        MediaError::Invalid(m) => ToolError::Failed(format!(
            "{m} (o modelo é escolha do usuário; não tente outro modelo, avise o usuário)"
        )),
        MediaError::Timeout => ToolError::Failed(
            "o vídeo não ficou pronto a tempo (limite de 10 minutos); o provedor pode ter cobrado mesmo assim, avise o usuário".into(),
        ),
        other => ToolError::Failed(other.to_string()),
    }
}

/// Refuses a prompt that is too short and tells the model what to add. It
/// never loops: it returns the error and the turn loop decides how many
/// refusals to tolerate.
fn quality_gate(prompt: &str, min: usize, kind: Kind) -> Result<()> {
    let words = prompt.split_whitespace().count();
    if words >= min {
        return Ok(());
    }
    let extra = match kind {
        Kind::Image => "",
        Kind::Video => ", movimento, movimento de câmera, ritmo e áudio (se ligado)",
    };
    Err(ToolError::BadArguments(format!(
        "prompt curto demais ({words} palavras; mínimo {min}). Reescreva com um prompt elaborado e chame de novo, cobrindo: \
sujeito e detalhes, cenário, composição/enquadramento, estilo/mídia, iluminação, paleta de cores, clima, lente/câmera, \
termos de qualidade e o que evitar{extra}."
    )))
}

fn report(
    kind: Kind,
    prompt: &str,
    model: &str,
    sent: &std::collections::BTreeMap<String, Value>,
    notes: &[String],
    files: &[GeneratedFile],
    _root: &Path,
) -> String {
    let mut out = format!(
        "{} gerado com {model}.\n",
        if kind == Kind::Image {
            "Imagem"
        } else {
            "Vídeo"
        }
    );
    let params = if sent.is_empty() {
        "nenhum".to_string()
    } else {
        sent.iter()
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k}={s}"),
                other => format!("{k}={other}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    out.push_str(&format!("Parâmetros enviados: {params}\n"));
    for n in notes {
        out.push_str(&format!("Aviso: {n}\n"));
    }
    let cost: f64 = files.iter().filter_map(|f| f.cost_usd).sum();
    if files.iter().any(|f| f.cost_usd.is_some()) {
        out.push_str(&format!("Custo: US$ {cost:.4}\n"));
    } else {
        out.push_str("Custo: não informado pelo provedor\n");
    }
    for f in files {
        out.push_str(&format!(
            "Arquivo: {} ({}, {} KB)\n",
            f.path.display(),
            f.media_type,
            f.bytes.div_ceil(1024)
        ));
    }
    // Same `![](path)` line for video: the chat shows a player for .mp4.
    for f in files {
        out.push_str(&markdown_image(prompt, &f.path));
        out.push('\n');
    }
    out.push_str(&format!(
        "Cole a(s) linha(s) Markdown acima na resposta para o usuário ver {}.\n",
        if kind == Kind::Image {
            "a imagem"
        } else {
            "o vídeo"
        }
    ));
    out
}

/// `![descrição](caminho)`. Backslashes become `/`; a path with spaces or a
/// closing parenthesis is wrapped in `<...>` (CommonMark), which plain
/// Markdown would otherwise cut at the first space.
fn markdown_image(prompt: &str, path: &Path) -> String {
    let alt: String = prompt
        .chars()
        .map(|c| if "[]()\n\r".contains(c) { ' ' } else { c })
        .take(80)
        .collect::<String>()
        .trim()
        .to_string();
    let p = path.to_string_lossy().replace('\\', "/");
    if p.contains(char::is_whitespace) || p.contains(')') {
        format!("![{alt}](<{p}>)")
    } else {
        format!("![{alt}]({p})")
    }
}

/// `YYYY-MM-DD` (UTC) from the system clock, without a date crate
/// (Hinnant's civil-from-days).
fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests;
