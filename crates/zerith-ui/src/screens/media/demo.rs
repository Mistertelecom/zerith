//! Fake media data: a deterministic catalogue of a few hundred models and a
//! backend that serves it, so snapshots and tests run offline. Nothing here
//! touches the network, the keyring or the disk.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use zerith_providers::media::{KeyInfo, Kind, MediaModel, ModelPage, ParamKind, ParamSpec};

use super::state::{Backend, KeySource};

fn spec(name: &str, kind: ParamKind) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        kind,
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// One fake model. Even indexes take an aspect ratio and a count, multiples of
/// three also take a quality, so switching models drops different parameters.
pub fn model(kind: Kind, i: usize) -> MediaModel {
    let (vendor, family) = match kind {
        Kind::Image => (
            [
                "bytedance-seed",
                "black-forest-labs",
                "openai",
                "google",
                "stability",
            ][i % 5],
            ["seedream", "flux", "gpt-image", "imagen", "sd"][i % 5],
        ),
        Kind::Video => (
            ["google", "kwaivgi", "alibaba", "minimax", "openai"][i % 5],
            ["veo", "kling", "wan", "hailuo", "sora"][i % 5],
        ),
    };
    let ver = format!("{}.{}", 1 + i / 5 % 4, i % 3);
    let mut params = Vec::new();
    match kind {
        Kind::Image => {
            if i.is_multiple_of(2) {
                params.push(spec(
                    "aspect_ratio",
                    ParamKind::Enum(strings(&["1:1", "16:9", "9:16", "4:3"])),
                ));
                params.push(spec("n", ParamKind::Range { min: 1.0, max: 4.0 }));
            }
            if i.is_multiple_of(3) {
                params.push(spec(
                    "quality",
                    ParamKind::Enum(strings(&["low", "medium", "high"])),
                ));
            }
            if i.is_multiple_of(4) {
                params.push(spec("seed", ParamKind::Text));
            }
        }
        Kind::Video => {
            params.push(spec(
                "duration",
                ParamKind::Enum(strings(&["4", "6", "8", "10"])),
            ));
            params.push(spec(
                "resolution",
                ParamKind::Enum(strings(&["480p", "720p", "1080p"])),
            ));
            if i.is_multiple_of(2) {
                params.push(spec("generate_audio", ParamKind::Flag));
            }
        }
    }
    MediaModel {
        id: format!("{vendor}/{family}-{ver}-{i}"),
        name: format!("{} {ver}", family[..1].to_uppercase() + &family[1..]),
        kind,
        description: match kind {
            Kind::Image => {
                "Gera imagens a partir de texto, com bom acabamento em retratos e cenas de produto."
                    .into()
            }
            Kind::Video => "Gera clipes curtos a partir de texto ou de uma imagem inicial.".into(),
        },
        price_hint: match kind {
            Kind::Image => format!("US$ 0,0{}/imagem", 2 + i % 7),
            Kind::Video => format!("~US$ 0,{}/s", 10 + i % 40),
        },
        params,
        created: 1_760_000_000 + i as i64,
        max_references: (i % 3) as u32,
        frame_images: Vec::new(),
    }
}

/// The catalogue: 240 images, 120 videos.
pub fn catalogue(kind: Kind) -> Vec<MediaModel> {
    let n = if kind == Kind::Image { 240 } else { 120 };
    (0..n).map(|i| model(kind, i)).collect()
}

/// A backend over the fake catalogue. `requests` counts calls to `models`.
pub fn backend(key: Option<KeySource>) -> (Backend, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let b = Backend {
        models: Arc::new(move |kind, q, off, lim| {
            c.fetch_add(1, Ordering::SeqCst);
            let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
            let hits: Vec<MediaModel> = catalogue(kind)
                .into_iter()
                .filter(|m| {
                    let hay = format!("{} {}", m.id, m.name).to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                })
                .collect();
            Ok(ModelPage {
                total: hits.len(),
                models: hits.into_iter().skip(off).take(lim).collect(),
            })
        }),
        resolve_key: Arc::new(move || Ok(key)),
        save_key: Arc::new(|_| Ok(())),
        check_key: Arc::new(|| {
            Ok(KeyInfo {
                label: "zerith-local".into(),
                usage_usd: Some(1.42),
                limit_usd: Some(20.0),
            })
        }),
    };
    (b, calls)
}
