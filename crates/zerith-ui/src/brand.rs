//! Brand marks: the Zerith logo and the provider logos, embedded in the binary.
//!
//! The provider PNGs come from 9Router (`public/providers`, MIT; see
//! `assets/providers/SOURCE.md`). They identify whose service a row talks to,
//! nothing more. A name with no logo gets a monogram tile, so every row keeps
//! the same shape.

use rune::{el, text, El, TextStyle};

use crate::mediaout::Images;
use crate::ui::Ui;

macro_rules! logos {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/providers/", $name, ".png")) as &[u8])),*]
    };
}

const LOGOS: &[(&str, &[u8])] = logos![
    "anthropic",
    "claude",
    "openai",
    "codex",
    "gemini",
    "deepseek",
    "openrouter",
    "ollama",
    "antigravity",
    "xai",
    "groq",
    "mistral",
    "qwen",
    "kimi",
    "glm",
    "minimax",
    "github",
    "cursor",
    "kiro",
    "black-forest-labs",
    "recraft",
];

const ZERITH: &[u8] = include_bytes!("../assets/zerith.svg");

/// The logo file for a free-form provider, account or model name. Matches by
/// substring so `anthropic-trabalho`, `claude-sonnet` and `openai/gpt-5` all
/// find their mark.
pub fn key_for(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    const ALIASES: &[(&str, &str)] = &[
        ("claude-code", "claude"),
        ("claude", "claude"),
        ("anthropic", "anthropic"),
        ("codex", "codex"),
        ("openrouter", "openrouter"),
        ("gpt", "openai"),
        ("openai", "openai"),
        ("gemini", "gemini"),
        ("google", "gemini"),
        ("antigravity", "antigravity"),
        ("deepseek", "deepseek"),
        ("ollama", "ollama"),
        ("grok", "xai"),
        ("x-ai", "xai"),
        ("xai", "xai"),
        ("groq", "groq"),
        ("mistral", "mistral"),
        ("qwen", "qwen"),
        ("kimi", "kimi"),
        ("moonshot", "kimi"),
        ("glm", "glm"),
        ("minimax", "minimax"),
        ("github", "github"),
        ("copilot", "github"),
        ("cursor", "cursor"),
        ("kiro", "kiro"),
        ("flux", "black-forest-labs"),
        ("black-forest", "black-forest-labs"),
        ("recraft", "recraft"),
    ];
    ALIASES.iter().find(|(k, _)| n.contains(k)).map(|(_, v)| *v)
}

fn tile(size: f32) -> El {
    let r = (size * 0.24).round().clamp(4.0, 9.0);
    el().size(size, size).radius(r)
}

fn ready(image: &rune::Image) -> bool {
    // A snapshot has one frame: wait for the decode instead of drawing a blank.
    if rune::is_headless() {
        let _ = image.wait();
    }
    image.is_ready() && image.error().is_none()
}

/// A square logo for `name`, or a monogram tile when there is none.
pub fn logo(name: &str, size: f32, images: &Images, u: &Ui) -> El {
    if let Some(key) = key_for(name) {
        if let Some((_, bytes)) = LOGOS.iter().find(|(k, _)| *k == key) {
            let image = images.embedded(key, bytes);
            if ready(&image) {
                let r = (size * 0.24).round().clamp(4.0, 9.0);
                // `ollama.png` is black ink on transparent; give it a soft light
                // plate in both themes so it remains crisp against dark fills.
                let needs_light_plate = key == "ollama";
                let transparent_mark = matches!(
                    key,
                    "ollama"
                        | "deepseek"
                        | "gemini"
                        | "antigravity"
                        | "openrouter"
                        | "github"
                        | "xai"
                );
                let pad = if transparent_mark {
                    (size * 0.12).round().clamp(1.5, 3.5)
                } else {
                    0.0
                };
                let inner = (size - pad * 2.0).max(8.0);
                let bg = if needs_light_plate {
                    rune::color::hex("#f1f5f9").unwrap_or(rune::Color::WHITE)
                } else {
                    u.layer2
                };
                return tile(size)
                    .clip_rounded(r)
                    .fill(bg)
                    .border(1.0, u.line)
                    .center()
                    .a11y_hidden()
                    .child(
                        el().size(inner, inner)
                            .radius((r - pad).max(2.0))
                            .pixels(image.pixels()),
                    );
            }
        }
    }
    let letter: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into());
    tile(size)
        .fill(u.layer2)
        .border(1.0, u.line)
        .center()
        .a11y_hidden()
        .child(
            text(letter)
                .font(TextStyle::new(size * 0.46, 600.0).lh(size * 0.6))
                .color(u.secondary),
        )
}

/// The Zerith mark.
pub fn zerith(size: f32, images: &Images) -> El {
    let image = images.embedded_svg("zerith", ZERITH, size);
    if ready(&image) {
        tile(size).pixels(image.pixels()).a11y_hidden()
    } else {
        tile(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_people_actually_type_find_a_logo() {
        for (name, want) in [
            ("anthropic-trabalho", "anthropic"),
            ("Claude Code", "claude"),
            ("claude-sonnet-5-5", "claude"),
            ("openai/gpt-5", "openai"),
            ("codex", "codex"),
            ("9router", ""),
            ("black-forest-labs/flux-2.0", "black-forest-labs"),
        ] {
            assert_eq!(key_for(name).unwrap_or(""), want, "{name}");
        }
    }

    #[test]
    fn every_alias_points_at_an_embedded_png() {
        for name in ["claude", "gpt", "gemini", "grok", "kimi", "flux", "copilot"] {
            let key = key_for(name).unwrap();
            assert!(
                LOGOS
                    .iter()
                    .any(|(k, b)| *k == key && b.starts_with(b"\x89PNG")),
                "{key}"
            );
        }
    }
}
