//! What a model can hold, by name, when neither the account nor the provider
//! says. The numbers are the smallest window any provider lists for the name
//! (models.dev, 2026-11): guessing high overflows the request, guessing low
//! only summarizes a little early.

/// Context window of `model`, in tokens. `None` = unknown name (the caller
/// keeps its own default). Accepts `provider/model` and `conta:modelo`.
// ponytail: name heuristic over the families that matter; a models.dev
// snapshot per release if a provider starts aliasing names.
pub fn context_window(model: &str) -> Option<u32> {
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
}
