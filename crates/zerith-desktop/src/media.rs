//! Images and video from OpenRouter: catalogue, key, and generation.
//!
//! The key is read from the OS keyring at the moment of a request and never
//! kept in the window; the window only learns whether one exists.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use zerith_core::Paths;
use zerith_providers::media::{ImageRequest, Kind, OpenRouter, VideoRequest};
use zerith_providers::Cancel;

use crate::state::AppState;

/// One model as the window sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaModelView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price_hint: String,
    pub params: Vec<ParamView>,
    pub max_references: u32,
    pub frame_images: Vec<String>,
}

/// One parameter a model accepts. The window renders a control from `kind`:
/// `enum` picks from `options`, `range` bounds a number, `flag` is a switch,
/// `text` is free (a number when the descriptor bounds nothing).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamView {
    pub name: String,
    pub kind: String,
    pub options: Vec<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl ParamView {
    fn of(p: &zerith_providers::media::ParamSpec) -> Self {
        use zerith_providers::media::ParamKind;
        let (kind, options, min, max) = match &p.kind {
            ParamKind::Enum(v) => ("enum", v.clone(), None, None),
            ParamKind::Range { min, max } => ("range", Vec::new(), Some(*min), Some(*max)),
            ParamKind::Flag => ("flag", Vec::new(), None, None),
            ParamKind::Text => ("text", Vec::new(), None, None),
            ParamKind::Other => ("other", Vec::new(), None, None),
        };
        Self {
            name: p.name.clone(),
            kind: kind.into(),
            options,
            min,
            max,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPage {
    pub total: usize,
    pub models: Vec<MediaModelView>,
}

/// What the window knows about the key: whether one is configured, and where.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub source: Option<String>,
    pub label: String,
    pub usage_usd: Option<f64>,
    pub limit_usd: Option<f64>,
}

/// A request from the composer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRequest {
    pub kind: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    #[serde(default)]
    pub references: Vec<String>,
    pub first_frame: Option<String>,
    pub last_frame: Option<String>,
}

/// What one file became.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub path: String,
    pub media_type: String,
    pub bytes: u64,
    pub cost_usd: Option<f64>,
}

/// The saved slot for each kind.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotView {
    pub image: String,
    pub image_params: BTreeMap<String, Value>,
    pub video: String,
    pub video_params: BTreeMap<String, Value>,
    pub output_dir: String,
}

fn kind_of(s: &str) -> Result<Kind, String> {
    Kind::parse(s).ok_or_else(|| format!("tipo de mídia desconhecido: {s}"))
}

/// The catalogue needs no key, so it works before one is configured.
#[tauri::command]
pub async fn media_models(
    state: State<'_, AppState>,
    kind: String,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<MediaPage, String> {
    let base = crate::wire::media_load(state.root()).base_url;
    tauri::async_runtime::spawn_blocking(move || {
        let page = OpenRouter::new(None, Some(base))
            .models(kind_of(&kind)?, &query, offset, limit)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(MediaPage {
            total: page.total,
            models: page
                .models
                .into_iter()
                .map(|m| MediaModelView {
                    id: m.id,
                    name: m.name,
                    description: m.description,
                    price_hint: m.price_hint,
                    params: m.params.iter().map(ParamView::of).collect(),
                    max_references: m.max_references,
                    frame_images: m.frame_images,
                })
                .collect(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether a key exists, and what it says about itself. Never returns the key.
#[tauri::command]
pub async fn media_key(state: State<'_, AppState>, check: bool) -> Result<KeyView, String> {
    let base = crate::wire::media_load(state.root()).base_url;
    tauri::async_runtime::spawn_blocking(move || {
        let source = match zerith_core::secrets::resolve_openrouter_key() {
            Ok(Some(_)) => {
                if std::env::var(zerith_core::secrets::OPENROUTER_ENV)
                    .is_ok_and(|v| !v.trim().is_empty())
                {
                    Some("env".to_string())
                } else {
                    Some("keyring".to_string())
                }
            }
            Ok(None) => None,
            Err(e) => return Err(e.to_string()),
        };
        let mut view = KeyView {
            source,
            label: String::new(),
            usage_usd: None,
            limit_usd: None,
        };
        if check && view.source.is_some() {
            let key = zerith_core::secrets::resolve_openrouter_key()
                .map_err(|e| e.to_string())?;
            let info = OpenRouter::new(key, Some(base))
                .check_key()
                .map_err(|e| e.to_string())?;
            view.label = info.label;
            view.usage_usd = info.usage_usd;
            view.limit_usd = info.limit_usd;
        }
        Ok::<_, String>(view)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stores the key in the OS keyring. An empty string removes it.
#[tauri::command]
pub async fn media_save_key(key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let k = key.trim();
        if k.is_empty() {
            return zerith_core::secrets::set_secret(zerith_core::secrets::OPENROUTER_ENTRY, "")
                .map_err(|e| e.to_string());
        }
        zerith_core::secrets::set_secret(zerith_core::secrets::OPENROUTER_ENTRY, k)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The model and fixed parameters saved for each kind.
#[tauri::command]
pub async fn media_settings(state: State<'_, AppState>) -> Result<SlotView, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let m = crate::wire::media_load(&root);
        Ok::<_, String>(SlotView {
            image: m.image.model.clone(),
            image_params: m.image.params.clone(),
            video: m.video.model.clone(),
            video_params: m.video.params.clone(),
            output_dir: crate::wire::media_dir(&root).display().to_string(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotEdit {
    pub kind: String,
    pub model: String,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
}

/// Fixes one kind`s model and parameters in the user settings layer.
#[tauri::command]
pub async fn media_save_slot(state: State<'_, AppState>, edit: SlotEdit) -> Result<(), String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        crate::wire::media_save(&root, kind_of(&edit.kind)?, &edit.model, &edit.params)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Generates one image or one video and returns the files written.
#[tauri::command]
pub async fn media_generate(
    state: State<'_, AppState>,
    req: MediaRequest,
) -> Result<Vec<FileView>, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let kind = kind_of(&req.kind)?;
        let base = crate::wire::media_load(&root).base_url;
        let key = zerith_core::secrets::resolve_openrouter_key().map_err(|e| e.to_string())?;
        let client = OpenRouter::new(key, Some(base));
        let out = crate::wire::media_dir(&root);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        let cancel = Cancel::default();
        let files = match kind {
            Kind::Image => client
                .generate_image(
                    &ImageRequest {
                        model: req.model,
                        prompt: req.prompt,
                        params: req.params,
                        reference_images: req.references,
                        filename_hint: None,
                    },
                    &out,
                    &cancel,
                )
                .map_err(|e| e.to_string())?,
            Kind::Video => vec![client
                .generate_video(
                    &VideoRequest {
                        model: req.model,
                        prompt: req.prompt,
                        params: req.params,
                        reference_images: req.references,
                        first_frame: req.first_frame,
                        last_frame: req.last_frame,
                        filename_hint: None,
                    },
                    &out,
                    &cancel,
                    &mut |_| {},
                )
                .map_err(|e| e.to_string())?],
        };
        Ok::<_, String>(
            files
                .into_iter()
                .map(|f| FileView {
                    path: f.path.display().to_string(),
                    media_type: f.media_type,
                    bytes: f.bytes,
                    cost_usd: f.cost_usd,
                })
                .collect(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Opens one generated file, refusing anything outside the media folder.
#[tauri::command]
pub async fn media_open(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let dir = crate::wire::media_dir(state.root());
    tauri::async_runtime::spawn_blocking(move || {
        crate::wire::open_media(&dir, &PathBuf::from(path))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Opens the folder generated files land in.
#[tauri::command]
pub async fn media_open_dir(state: State<'_, AppState>) -> Result<(), String> {
    let dir = crate::wire::media_dir(state.root());
    let _ = std::fs::create_dir_all(&dir);
    crate::wire::reveal(&dir)
}

/// Kept so the settings screen can show where files will land without a
/// second round trip after a save.
pub fn resolved_dir(root: &std::path::Path) -> PathBuf {
    Paths::with_root(root).root().join("media").to_path_buf()
}
