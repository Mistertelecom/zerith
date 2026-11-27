//! Auxiliary fallback vision client bound to `settings.routing.vision_model`.
//!
//! When the active chat model lacks native vision (for example DeepSeek text
//! models or pure code models), user-attached screenshots and tool captures
//! (`view_image`, `browser{action: "screenshot"}`, `desktop_inspect{action: "screenshot"}`)
//! are routed through `vision_model` to extract a faithful, technical textual
//! description (exact error strings, window titles, UI layout issues) instead
//! of crashing with HTTP 400 or leaving the main model blind.

use zerith_core::Settings;
use zerith_providers::{
    Cancel, ChatMessage, ChatRequest, Client, ProviderConfig, ProviderKind, RetryPolicy,
};

const VISION_TIMEOUT_MS: u64 = 45_000;

const VISION_SYSTEM_PROMPT: &str = "\
Você é o módulo auxiliar de visão do Zerith. Analise a(s) imagem(ns) anexada(s) em português do Brasil \
de forma objetiva, fiel e técnica para o modelo principal que não possui visão nativa:\n\
- Transcreva integralmente textos visíveis relevantes: mensagens de erro, stack traces, títulos de janelas/caixas de diálogo, badges e campos.\n\
- Descreva o estado visual da interface (botões, cortes, sobreposições, contraste, caixas modais abertas).\n\
- Nunca invente texto que não esteja claramente legível na imagem.";

/// Returns `false` for known text-only models/providers (e.g. DeepSeek text,
/// pure code models, Llama 3 text) and `true` for multimodal models or unknown
/// custom endpoints (fail-open so native vision models keep receiving images
/// directly without extra latency).
pub fn model_supports_vision(provider: ProviderKind, model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    let base = m.rsplit('/').next().unwrap_or(&m);

    // Explicit vision/multimodal tokens always win.
    if base.contains("-vl")
        || base.contains("_vl")
        || base.contains("vision")
        || base.contains("pixtral")
        || base.contains("llava")
        || base.contains("minicpm-v")
        || base.contains("internvl")
        || base.contains("glm-4v")
        || base.contains("glm-4.5v")
        || base.contains("llama-4")
        || base.contains("llama4")
    {
        return true;
    }

    // Known text-only models across providers (including OpenRouter/Ollama).
    if matches!(provider, ProviderKind::DeepSeek)
        || base.starts_with("deepseek-")
        || base.contains("deepseek-chat")
        || base.contains("deepseek-reasoner")
        || base.contains("deepseek-r1")
        || base.contains("deepseek-v3")
        || base.contains("deepseek-coder")
        || base.contains("-coder")
        || base.starts_with("codestral")
        || base.starts_with("devstral")
        || base.starts_with("starcoder")
        || base == "o1-mini"
        || base == "o3-mini"
        || base.starts_with("qwq")
        || base.starts_with("kimi-k2")
        || ((base.starts_with("llama-3") || base.starts_with("llama3")) && !base.contains("vision"))
    {
        return false;
    }

    // Multimodal first-party providers.
    if matches!(
        provider,
        ProviderKind::Anthropic
            | ProviderKind::Gemini
            | ProviderKind::Codex
            | ProviderKind::Antigravity
    ) {
        return true;
    }

    true
}

/// Quick check for markdown image syntax referencing a local file (`![...](path)`).
pub fn has_local_image_ref(text: &str) -> bool {
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("](") else { break };
        if rest[..j].contains('\n') {
            continue;
        }
        let after = &rest[j + 2..];
        let (raw, next) = match after.strip_prefix('<') {
            Some(inner) => match inner.find(">)") {
                Some(k) => (&inner[..k], &inner[k + 2..]),
                None => break,
            },
            None => match after.find(')') {
                Some(k) => (&after[..k], &after[k + 1..]),
                None => break,
            },
        };
        let target = raw.trim();
        let lower = target.to_ascii_lowercase();
        let is_remote = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("data:");
        if !is_remote
            && [".png", ".jpg", ".jpeg", ".webp", ".gif"]
                .iter()
                .any(|ext| lower.ends_with(ext))
        {
            return true;
        }
        rest = next;
    }
    false
}

/// Fail-open auxiliary client bound to `settings.routing.vision_model`.
pub struct VisionClient {
    client: Client,
    model: String,
}

impl VisionClient {
    /// Builds a vision client when `settings.routing.vision_model` is set and
    /// non-empty. Performs zero network I/O.
    pub fn from_settings(
        settings: Option<&Settings>,
        fallback_cfg: &ProviderConfig,
    ) -> Option<Self> {
        let settings = settings?;
        let spec = settings
            .routing
            .vision_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let (gw, _) = crate::resolve_gateway(settings, spec, fallback_cfg);
        let mut cand = gw.candidates.into_iter().next()?;
        if cand.model.trim().is_empty() {
            return None;
        }
        cand.config.timeout_ms = if cand.config.timeout_ms == 0 {
            VISION_TIMEOUT_MS
        } else {
            cand.config.timeout_ms.min(VISION_TIMEOUT_MS)
        };
        let client = Client::new(cand.config)
            .ok()?
            .with_retry(RetryPolicy::none());
        Some(Self {
            client,
            model: cand.model,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Calls `vision_model` with a non-synthetic user message containing the
    /// markdown image reference(s) so `zerith-providers` attaches the actual
    /// image bytes automatically.
    pub fn describe_images(
        &self,
        context_hint: &str,
        markdown_with_images: &str,
        cancel: &Cancel,
    ) -> Option<String> {
        if cancel.is_cancelled() || !has_local_image_ref(markdown_with_images) {
            return None;
        }
        let hint: String = context_hint.trim().chars().take(600).collect();
        let user_text = if hint.is_empty() || hint == markdown_with_images.trim() {
            format!(
                "Analise a(s) imagem(ns) abaixo e extraia todos os textos, erros e detalhes visuais:\n\n{markdown_with_images}"
            )
        } else {
            format!(
                "Contexto do pedido: {hint}\n\nAnalise a(s) imagem(ns) abaixo e extraia todos os textos, erros e detalhes visuais:\n\n{markdown_with_images}"
            )
        };
        let req = ChatRequest::new(
            &self.model,
            vec![
                ChatMessage::system(VISION_SYSTEM_PROMPT),
                ChatMessage::user(user_text),
            ],
        )
        .max_tokens(1024)
        .temperature(0.1);
        let comp = self.client.complete(&req, cancel).ok()?;
        let out = comp.text.trim();
        (!out.is_empty()).then(|| out.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_capability_classifier_flags_deepseek_and_coder_models_as_text_only() {
        assert!(!model_supports_vision(
            ProviderKind::DeepSeek,
            "deepseek-chat"
        ));
        assert!(!model_supports_vision(
            ProviderKind::DeepSeek,
            "deepseek-reasoner"
        ));
        assert!(!model_supports_vision(
            ProviderKind::OpenRouter,
            "deepseek/deepseek-chat-v3-0324"
        ));
        assert!(!model_supports_vision(
            ProviderKind::OpenRouter,
            "deepseek/deepseek-r1"
        ));
        assert!(!model_supports_vision(
            ProviderKind::OpenRouter,
            "qwen/qwen3-coder"
        ));

        assert!(model_supports_vision(
            ProviderKind::Anthropic,
            "claude-sonnet-4-6"
        ));
        assert!(model_supports_vision(
            ProviderKind::Gemini,
            "gemini-2.5-flash"
        ));
        assert!(model_supports_vision(
            ProviderKind::OpenRouter,
            "google/gemini-2.5-flash"
        ));
        assert!(model_supports_vision(
            ProviderKind::OpenRouter,
            "qwen/qwen2.5-vl-72b-instruct"
        ));
        assert!(model_supports_vision(ProviderKind::OpenAi, "gpt-4o"));
    }

    #[test]
    fn detects_local_image_refs_and_ignores_remote_urls() {
        assert!(has_local_image_ref(
            "olha isso ![print](<C:/Users/Joao/shot 1.png>)"
        ));
        assert!(has_local_image_ref("![x](/tmp/a.webp)"));
        assert!(!has_local_image_ref("![x](https://example.com/a.png)"));
        assert!(!has_local_image_ref("sem imagem aqui"));
    }
}
