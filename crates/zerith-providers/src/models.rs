//! Model capabilities and context windows: backed by a locally cached
//! `models.dev` catalog (`https://models.dev/api.json`, 24h TTL, fail-open
//! background refresh) with conservative offline name heuristics as fallback.

use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MODELS_DEV_TIMEOUT: Duration = Duration::from_secs(12);
const MODELS_DEV_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const MODELS_DEV_MAX_BYTES: u64 = 16 << 20;

/// Metadata extracted from `models.dev` for one model id.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelMeta {
    pub context_window: Option<u32>,
    pub supports_vision: Option<bool>,
    pub reasoning: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    by_id: HashMap<String, ModelMeta>,
}

static GLOBAL_CATALOG: OnceLock<Mutex<ModelCatalog>> = OnceLock::new();
static WARM_STARTED: OnceLock<()> = OnceLock::new();

fn global_catalog() -> &'static Mutex<ModelCatalog> {
    GLOBAL_CATALOG.get_or_init(|| Mutex::new(ModelCatalog::default()))
}

impl ModelCatalog {
    /// Parses the `models.dev/api.json` structure:
    /// `{ "<provider>": { "models": { "<model_id>": { "limit": { "context": N }, "modalities": { "input": [...] }, "reasoning": bool } } } }`.
    pub fn from_models_dev_json(v: &Value) -> Self {
        let mut by_id = HashMap::new();
        let Some(providers) = v.as_object() else {
            return Self::default();
        };
        for (prov_id, prov) in providers {
            let Some(models) = prov.get("models").and_then(Value::as_object) else {
                continue;
            };
            let prov_l = prov_id.trim().to_ascii_lowercase();
            for (model_id, entry) in models {
                let id_l = model_id.trim().to_ascii_lowercase();
                if id_l.is_empty() {
                    continue;
                }
                let ctx = entry
                    .get("limit")
                    .and_then(|l| l.get("context"))
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|&n| n >= 4_096);
                let supports_vision = entry
                    .get("modalities")
                    .and_then(|m| m.get("input"))
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .any(|x| x.as_str().is_some_and(|s| s.eq_ignore_ascii_case("image")))
                    });
                let reasoning = entry.get("reasoning").and_then(Value::as_bool);
                let meta = ModelMeta {
                    context_window: ctx,
                    supports_vision,
                    reasoning,
                };
                if meta.context_window.is_none()
                    && meta.supports_vision.is_none()
                    && meta.reasoning.is_none()
                {
                    continue;
                }
                let full_key = format!("{prov_l}/{id_l}");
                by_id.entry(full_key).or_insert(meta);
                // Bare model id: keep the conservative (smaller) context window on collision.
                by_id
                    .entry(id_l.clone())
                    .and_modify(|cur| {
                        cur.context_window = match (cur.context_window, meta.context_window) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        };
                        if cur.supports_vision.is_none() {
                            cur.supports_vision = meta.supports_vision;
                        }
                        if cur.reasoning.is_none() {
                            cur.reasoning = meta.reasoning;
                        }
                    })
                    .or_insert(meta);
            }
        }
        Self { by_id }
    }

    pub fn lookup(&self, model: &str) -> Option<ModelMeta> {
        if self.by_id.is_empty() {
            return None;
        }
        let raw = model.trim().to_ascii_lowercase();
        if raw.is_empty() {
            return None;
        }
        // Strip account prefix (`conta:modelo`) while keeping `provider/model`.
        let without_acct = raw.split_once(':').map_or(raw.as_str(), |(_, m)| m);
        let bare = without_acct.rsplit('/').next().unwrap_or(without_acct);
        let candidates = [
            without_acct.to_string(),
            bare.to_string(),
            bare.replace('.', "-"),
            bare.replace('-', "."),
        ];
        for key in candidates {
            if let Some(&meta) = self.by_id.get(&key) {
                return Some(meta);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

fn cache_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("models-dev.json")
}

fn is_fresh(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age < MODELS_DEV_MAX_AGE)
}

fn load_from_disk(path: &Path) -> Option<ModelCatalog> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let cat = ModelCatalog::from_models_dev_json(&v);
    (!cat.is_empty()).then_some(cat)
}

fn fetch_models_dev() -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(MODELS_DEV_TIMEOUT)
        .build();
    let resp = agent
        .get(MODELS_DEV_URL)
        .set("User-Agent", "Zerith")
        .call()
        .ok()?;
    let mut buf = String::new();
    resp.into_reader()
        .take(MODELS_DEV_MAX_BYTES)
        .read_to_string(&mut buf)
        .ok()?;
    Some(buf)
}

/// Loads the cached `models.dev` catalog from `cache_dir` immediately (if
/// present) and spawns a single non-blocking background refresh when missing
/// or older than 24 hours. Safe to call repeatedly; never fails or blocks.
pub fn warm_catalog(cache_dir: PathBuf) {
    if WARM_STARTED.set(()).is_err() {
        return;
    }
    let path = cache_file(&cache_dir);
    if let Some(cat) = load_from_disk(&path) {
        if let Ok(mut g) = global_catalog().lock() {
            *g = cat;
        }
        if is_fresh(&path) {
            return;
        }
    }
    std::thread::spawn(move || {
        let Some(body) = fetch_models_dev() else {
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(&body) else {
            return;
        };
        let cat = ModelCatalog::from_models_dev_json(&v);
        if cat.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::write(&path, body.as_bytes());
        if let Ok(mut g) = global_catalog().lock() {
            *g = cat;
        }
    });
}

/// Looks up cached `models.dev` metadata for `model`, if the catalog has been
/// loaded.
pub fn catalog_meta(model: &str) -> Option<ModelMeta> {
    global_catalog().lock().ok()?.lookup(model)
}

/// Returns `Some(true/false)` from the cached `models.dev` catalog when known.
pub fn catalog_supports_vision(model: &str) -> Option<bool> {
    catalog_meta(model).and_then(|m| m.supports_vision)
}

/// Context window of `model`, in tokens. Consults the cached `models.dev`
/// catalog first (when warmed), then falls back to conservative family
/// heuristics. Accepts `provider/model` and `conta:modelo`.
pub fn context_window(model: &str) -> Option<u32> {
    if let Some(ctx) = catalog_meta(model).and_then(|m| m.context_window) {
        return Some(ctx);
    }
    heuristic_context_window(model)
}

fn heuristic_context_window(model: &str) -> Option<u32> {
    let m = model.to_ascii_lowercase();
    let m = m.rsplit([':', '/']).next().unwrap_or(&m);
    let has = |p: &[&str]| p.iter().any(|x| m.contains(x));
    const K200: u32 = 200_000;
    const M1: u32 = 1_000_000;
    const M1B: u32 = 1_048_576;

    if m.starts_with("claude") {
        // 4.6 and later (and every 5.x) hold 1M; 4.5 and older 200k.
        let old = has(&[
            "-3",
            "-2",
            "instant",
            "4-5",
            "4.5",
            "4-1",
            "4.1",
            "-4-2",
            "opus-4-0",
            "sonnet-4-0",
        ]) || m == "claude-opus-4"
            || m == "claude-sonnet-4"
            || m.contains("-4-20");
        return Some(if old { K200 } else { M1 });
    }
    if m.starts_with("gemini") {
        if has(&[
            "image",
            "tts",
            "live",
            "embedding",
            "nano-banana",
            "omni",
            "computer-use",
        ]) {
            return None;
        }
        return has(&["-1.5", "-2.", "-3", "-4", "latest"]).then_some(M1B);
    }
    if m.starts_with("gpt-4.1") {
        return Some(1_047_576);
    }
    if let Some(rest) = m.strip_prefix("gpt-") {
        if has(&["image", "audio", "realtime", "spark"]) {
            return None;
        }
        let v = rest
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .next()
            .and_then(|s| s.parse::<f32>().ok())?;
        return match v {
            v if v >= 5.4 && !has(&["mini", "nano"]) => Some(1_050_000),
            v if v >= 5.0 => Some(400_000),
            _ => None,
        };
    }
    if m.starts_with("o3") || m.starts_with("o4") {
        return Some(K200);
    }
    if m.starts_with("grok") {
        return Some(if has(&["4.20", "4.3"]) {
            M1
        } else if has(&["4.5", "4.6", "4.7", "latest"]) {
            500_000
        } else {
            return None;
        });
    }
    if m.starts_with("deepseek") {
        return Some(if has(&["v4", "flash", "pro-latest"]) {
            M1B
        } else {
            163_840
        });
    }
    if m.starts_with("kimi") {
        return Some(if has(&["k3", "latest"]) { M1B } else { 262_144 });
    }
    if m.starts_with("glm") {
        let long = has(&["5.2", "5-2", "5.3", "latest"]);
        return Some(if long { M1 } else { K200 });
    }
    if m.starts_with("minimax") {
        return Some(if has(&["m3"]) { M1 } else { K200 });
    }
    if m.starts_with("qwen") {
        let long = has(&["-plus", "-flash", "3.7-max"]) && !has(&["omni", "asr", "livetranslate"]);
        return long.then_some(M1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::context_window as w;

    #[test]
    fn the_long_context_families_are_known_and_the_old_ones_stay_small() {
        // 1M
        for m in [
            "claude-sonnet-4-6",
            "claude-opus-4.8",
            "claude-sonnet-5",
            "claude-haiku-5-5",
            "anthropic/claude-opus-4.7",
            "claude:claude-sonnet-4-6",
            "kimi-k3",
            "glm-5.3",
            "minimax-m3",
            "qwen3.6-plus",
        ] {
            assert!(w(m).is_some_and(|n| n >= 1_000_000), "{m}: {:?}", w(m));
        }
        assert_eq!(w("gemini-3.1-pro-preview"), Some(1_048_576));
        assert_eq!(w("gemini-4-hydrogen"), Some(1_048_576));
        assert_eq!(w("gpt-5.6-sol"), Some(1_050_000));
        assert_eq!(w("gpt-6.2-astra-pro"), Some(1_050_000));
        assert_eq!(w("gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(w("deepseek-v4-pro"), Some(1_048_576));
        assert_eq!(w("grok-4.20"), Some(1_000_000));
        // smaller, on purpose
        for (m, n) in [
            ("claude-haiku-4.5", 200_000),
            ("claude-sonnet-4-5-20250929", 200_000),
            ("claude-opus-4.1", 200_000),
            ("claude-3-7-sonnet", 200_000),
            ("claude-sonnet-4-20250514", 200_000),
            ("gpt-5.3-codex", 400_000),
            ("gpt-5.4-mini", 400_000),
            ("o3", 200_000),
            ("deepseek-chat", 163_840),
            ("kimi-k2.6", 262_144),
            ("grok-4.7", 500_000),
        ] {
            assert_eq!(w(m), Some(n), "{m}");
        }
        // unknown or not a chat window
        for m in [
            "llama3",
            "gemini-2.5-flash-image",
            "gpt-5.4-image-2",
            "mistral-large",
        ] {
            assert_eq!(w(m), None, "{m}");
        }
    }

    #[test]
    fn models_dev_catalog_parses_vision_reasoning_and_context_limits() {
        let sample = serde_json::json!({
            "deepseek": {
                "models": {
                    "deepseek-chat": {
                        "limit": { "context": 131072 },
                        "modalities": { "input": ["text"], "output": ["text"] },
                        "reasoning": false
                    }
                }
            },
            "google": {
                "models": {
                    "gemini-2.5-flash": {
                        "limit": { "context": 1048576 },
                        "modalities": { "input": ["text", "image", "audio"], "output": ["text"] },
                        "reasoning": true
                    }
                }
            }
        });
        let cat = super::ModelCatalog::from_models_dev_json(&sample);
        assert_eq!(cat.len(), 4);
        let ds = cat.lookup("minha-conta:deepseek/deepseek-chat").unwrap();
        assert_eq!(ds.context_window, Some(131_072));
        assert_eq!(ds.supports_vision, Some(false));
        assert_eq!(ds.reasoning, Some(false));

        let gem = cat.lookup("gemini-2.5-flash").unwrap();
        assert_eq!(gem.context_window, Some(1_048_576));
        assert_eq!(gem.supports_vision, Some(true));
        assert_eq!(gem.reasoning, Some(true));
    }
}
