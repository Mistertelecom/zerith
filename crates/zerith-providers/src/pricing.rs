//! Preço por token dos modelos de texto, tirado do catálogo público do
//! OpenRouter (`GET /api/v1/models`, sem chave). Não há tabela fixa aqui: sem
//! catálogo (nem cópia em disco), o custo fica desconhecido.
//!
//! Ideia da precedência e do cálculo com cache emprestada do Goose
//! (`providers/canonical_cost.rs`, aaif-goose/goose, Apache-2.0) e do
//! `opencode stats` (anomalyco/opencode, MIT).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CATALOG_URL: &str = "https://openrouter.ai/api/v1/models";
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Depois disso o catálogo é buscado de novo; sem rede, a cópia velha serve.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// US$ por token. Cache sem preço próprio cobra como entrada.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// Tokens de uma ou mais chamadas. `prompt` já inclui os de cache (é assim
/// que todos os provedores do Zerith contam).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub prompt: u64,
    pub completion: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Price {
    /// Dólares de `t` a este preço.
    pub fn cost(&self, t: &Tokens) -> f64 {
        let fresh = t
            .prompt
            .saturating_sub(t.cache_read)
            .saturating_sub(t.cache_write);
        fresh as f64 * self.input
            + t.cache_read as f64 * self.cache_read.unwrap_or(self.input)
            + t.cache_write as f64 * self.cache_write.unwrap_or(self.input)
            + t.completion as f64 * self.output
    }
}

#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    by_id: HashMap<String, Price>,
}

impl PriceTable {
    /// Lê `{"data": [{"id", "canonical_slug", "pricing": {...}}]}`. Entradas
    /// sem preço numérico ou com preço negativo (roteadores dinâmicos) ficam
    /// de fora.
    pub fn from_json(v: &Value) -> Self {
        let mut by_id = HashMap::new();
        for m in v["data"].as_array().into_iter().flatten() {
            let p = &m["pricing"];
            let num = |k: &str| p[k].as_str().and_then(|s| s.trim().parse::<f64>().ok());
            let (Some(input), Some(output)) = (num("prompt"), num("completion")) else {
                continue;
            };
            if input < 0.0 || output < 0.0 {
                continue;
            }
            let price = Price {
                input,
                output,
                cache_read: num("input_cache_read").filter(|x| *x >= 0.0),
                cache_write: num("input_cache_write").filter(|x| *x >= 0.0),
            };
            for key in [&m["id"], &m["canonical_slug"]] {
                if let Some(id) = key.as_str() {
                    by_id.entry(id.to_ascii_lowercase()).or_insert(price);
                }
            }
        }
        Self { by_id }
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Id do OpenRouter e preço de `model`, como o Zerith o grava
    /// (`claude-opus-5-5`, `anthropic/claude-opus-5.5`, `conta:modelo`…).
    pub fn lookup(&self, model: &str) -> Option<(String, Price)> {
        let m = model.trim().to_ascii_lowercase();
        if m.is_empty() {
            return None;
        }
        if let Some(p) = self.by_id.get(&m) {
            return Some((m, *p));
        }
        // `[conta:][vendor/]modelo[:variante]`: o nome nu decide daqui em diante.
        let rest = match m.split_once(':') {
            Some((acc, rest)) if !acc.contains('/') && !rest.is_empty() => rest,
            _ => &m,
        };
        if let Some(p) = self.by_id.get(rest) {
            return Some((rest.to_string(), *p));
        }
        let bare = rest.rsplit('/').next().unwrap_or(rest);
        let bare = bare.split(':').next().unwrap_or(bare);
        let mut names = vec![bare.to_string()];
        let undated = strip_date(bare);
        if undated != bare {
            names.push(undated.to_string());
        }
        for n in names.clone() {
            let dotted = dot_versions(&n);
            if dotted != n {
                names.push(dotted);
            }
        }
        let vendors: Vec<&str> = match rest.split_once('/') {
            Some((v, _)) => vec![openrouter_vendor(v), vendor_of(bare).unwrap_or("")],
            None => vec![vendor_of(bare).unwrap_or("")],
        };
        for n in &names {
            for v in vendors.iter().filter(|v| !v.is_empty()) {
                let id = format!("{v}/{n}");
                if let Some(p) = self.by_id.get(&id) {
                    return Some((id, *p));
                }
            }
        }
        // Sem fornecedor conhecido: um id cujo nome bate, se for um só.
        for n in &names {
            let suffix = format!("/{n}");
            let mut hits = self
                .by_id
                .iter()
                .filter(|(id, _)| id.ends_with(&suffix) && !id.starts_with('~'));
            if let (Some((id, p)), None) = (hits.next(), hits.next()) {
                return Some((id.clone(), *p));
            }
        }
        None
    }
}

/// Prefixo de provedor do Zerith → fornecedor no OpenRouter.
fn openrouter_vendor(v: &str) -> &str {
    match v {
        "claude" => "anthropic",
        "gemini" => "google",
        "codex" => "openai",
        other => other,
    }
}

/// Fornecedor do OpenRouter pelo nome do modelo.
fn vendor_of(name: &str) -> Option<&'static str> {
    let starts = |p: &[&str]| p.iter().any(|x| name.starts_with(x));
    Some(if starts(&["claude"]) {
        "anthropic"
    } else if starts(&["gpt-", "chatgpt-", "codex-", "o1", "o3", "o4"]) {
        "openai"
    } else if starts(&["gemini", "gemma"]) {
        "google"
    } else if starts(&["deepseek"]) {
        "deepseek"
    } else if starts(&["grok"]) {
        "x-ai"
    } else if starts(&["glm"]) {
        "z-ai"
    } else if starts(&["kimi"]) {
        "moonshotai"
    } else if starts(&["qwen", "qwq"]) {
        "qwen"
    } else if starts(&["mistral", "codestral", "devstral", "magistral", "ministral"]) {
        "mistralai"
    } else if starts(&["llama"]) {
        "meta-llama"
    } else {
        return None;
    })
}

/// `-20250929` / `-2025-09-29` / `@20250929` no fim saem.
fn strip_date(n: &str) -> &str {
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if let Some((head, tail)) = n.rsplit_once(['-', '@']) {
        if tail.len() == 8 && digits(tail) {
            return head;
        }
    }
    if n.len() > 11 {
        let (head, tail) = n.split_at(n.len() - 11);
        let b = tail.as_bytes();
        if b[0] == b'-'
            && b[5] == b'-'
            && b[8] == b'-'
            && digits(&tail[1..5])
            && digits(&tail[6..8])
            && digits(&tail[9..])
        {
            return head;
        }
    }
    n
}

/// `claude-opus-5-5` → `claude-opus-5.5`: traço entre dois números curtos
/// (1–2 dígitos) vira ponto. `qwen3-235b` fica como está.
fn dot_versions(n: &str) -> String {
    let b = n.as_bytes();
    let run_left = |i: usize| {
        b[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let run_right = |i: usize| b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut out = String::with_capacity(n.len());
    for (i, c) in n.char_indices() {
        let short = |r: usize| (1..=2).contains(&r);
        if c == '-' && short(run_left(i)) && short(run_right(i)) {
            let after = i + 1 + run_right(i);
            if after == b.len() || !b[after].is_ascii_digit() {
                out.push('.');
                continue;
            }
        }
        out.push(c);
    }
    out
}

pub fn cache_file(dir: &Path) -> PathBuf {
    dir.join("openrouter-models-pricing.json")
}

/// A tabela de preços: a cópia em disco se tiver menos de 24 h; senão busca
/// o catálogo (e regrava a cópia); sem rede, a cópia velha; sem nada, vazia.
pub fn load(cache_dir: &Path) -> PriceTable {
    let file = cache_file(cache_dir);
    let saved = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < MAX_AGE);
    if let (true, Some(v)) = (fresh, &saved) {
        return PriceTable::from_json(v);
    }
    match fetch() {
        Ok(v) => {
            let slim = slim(&v);
            let _ = std::fs::create_dir_all(cache_dir);
            let _ = std::fs::write(&file, slim.to_string());
            PriceTable::from_json(&slim)
        }
        Err(_) => saved.map(|v| PriceTable::from_json(&v)).unwrap_or_default(),
    }
}

/// Só a cópia em disco, sem rede (vazia se não houver).
pub fn load_cached(cache_dir: &Path) -> PriceTable {
    std::fs::read_to_string(cache_file(cache_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|v| PriceTable::from_json(&v))
        .unwrap_or_default()
}

fn fetch() -> Result<Value, String> {
    // Sem redirecionamento, como o cliente de mídia.
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(0)
        .user_agent("zerith")
        .build();
    let resp = agent.get(CATALOG_URL).call().map_err(|e| e.to_string())?;
    let mut text = String::new();
    resp.into_reader()
        .take(32 << 20)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Só o que o preço precisa (o catálogo inteiro passa de 900 KB).
fn slim(v: &Value) -> Value {
    let keys = [
        "prompt",
        "completion",
        "input_cache_read",
        "input_cache_write",
    ];
    let data: Vec<Value> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let mut pricing = serde_json::Map::new();
            for k in keys {
                if let Some(x) = m["pricing"].get(k) {
                    pricing.insert(k.into(), x.clone());
                }
            }
            json!({"id": m["id"], "canonical_slug": m["canonical_slug"], "pricing": pricing})
        })
        .collect();
    json!({ "data": data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PriceTable {
        PriceTable::from_json(&json!({"data": [
            {"id": "anthropic/claude-opus-5.5", "canonical_slug": "anthropic/claude-opus-5.5-20260921",
             "pricing": {"prompt": "0.000004", "completion": "0.00002",
                         "input_cache_read": "0.0000002", "input_cache_write": "0.000005"}},
            {"id": "anthropic/claude-sonnet-4.5", "pricing": {"prompt": "0.000003", "completion": "0.000015"}},
            {"id": "anthropic/claude-3.5-haiku", "pricing": {"prompt": "0.0000008", "completion": "0.000004"}},
            {"id": "openai/gpt-5.5", "pricing": {"prompt": "0.000005", "completion": "0.00003",
                                                 "input_cache_read": "0.0000005"}},
            {"id": "openai/gpt-4.1-mini", "pricing": {"prompt": "0.0000004", "completion": "0.0000016"}},
            {"id": "deepseek/deepseek-chat", "pricing": {"prompt": "0.0000003", "completion": "0.0000012"}},
            {"id": "z-ai/glm-4.6", "pricing": {"prompt": "0.0000005", "completion": "0.000002"}},
            {"id": "qwen/qwen3-235b-a22b", "pricing": {"prompt": "0.0000001", "completion": "0.0000006"}},
            {"id": "openrouter/auto", "pricing": {"prompt": "-1", "completion": "-1"}},
            {"id": "x/sem-preco", "pricing": {}}
        ]}))
    }

    fn id(model: &str) -> Option<String> {
        table().lookup(model).map(|(id, _)| id)
    }

    #[test]
    fn maps_native_and_routed_names_to_openrouter_ids() {
        let opus = Some("anthropic/claude-opus-5.5".to_string());
        assert_eq!(id("anthropic/claude-opus-5.5"), opus);
        assert_eq!(id("claude-opus-5-5"), opus);
        assert_eq!(id("Claude-Opus-5.5"), opus);
        assert_eq!(id("claude-assinatura:claude-opus-5-5"), opus);
        assert_eq!(id("anthropic/claude-opus-5-5"), opus);
        assert_eq!(id("claude/claude-opus-5-5"), opus);
        assert_eq!(id("conta:anthropic/claude-opus-5.5:thinking"), opus);
        // a data some ou casa com o canonical_slug
        assert_eq!(
            id("claude-sonnet-4-5-20250929").as_deref(),
            Some("anthropic/claude-sonnet-4.5")
        );
        assert_eq!(
            id("claude-opus-5-5-20260921").as_deref(),
            Some("anthropic/claude-opus-5.5-20260921")
        );
        assert_eq!(
            id("claude-3-5-haiku-2024-10-22").as_deref(),
            Some("anthropic/claude-3.5-haiku")
        );
        assert_eq!(id("gpt-5.5").as_deref(), Some("openai/gpt-5.5"));
        assert_eq!(id("codex/gpt-5-5").as_deref(), Some("openai/gpt-5.5"));
        assert_eq!(id("gpt-4-1-mini").as_deref(), Some("openai/gpt-4.1-mini"));
        assert_eq!(
            id("deepseek-chat").as_deref(),
            Some("deepseek/deepseek-chat")
        );
        assert_eq!(id("glm-4.6").as_deref(), Some("z-ai/glm-4.6"));
        // fornecedor desconhecido, nome único no catálogo
        assert_eq!(
            id("ollama/qwen3-235b-a22b").as_deref(),
            Some("qwen/qwen3-235b-a22b")
        );
        assert_eq!(id("modelo-local"), None);
        assert_eq!(id("gpt-5-codex"), None);
        assert_eq!(id("openrouter/auto"), None, "preço -1 não é preço");
        assert_eq!(id("x/sem-preco"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn version_dots_and_dates() {
        assert_eq!(dot_versions("claude-3-5-sonnet"), "claude-3.5-sonnet");
        assert_eq!(dot_versions("qwen3-235b-a22b"), "qwen3-235b-a22b");
        assert_eq!(dot_versions("gpt-4o-mini"), "gpt-4o-mini");
        assert_eq!(strip_date("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(strip_date("claude-x@20250101"), "claude-x");
        assert_eq!(strip_date("deepseek-chat-v3-0324"), "deepseek-chat-v3-0324");
    }

    #[test]
    fn cost_splits_fresh_cached_and_written_prompt_tokens() {
        let (_, opus) = table().lookup("claude-opus-5-5").unwrap();
        // 100k de prompt = 10k novos + 80k lidos do cache + 10k gravados.
        let t = Tokens {
            prompt: 100_000,
            completion: 5_000,
            cache_read: 80_000,
            cache_write: 10_000,
        };
        let want = 10_000.0 * 4e-6 + 80_000.0 * 2e-7 + 10_000.0 * 5e-6 + 5_000.0 * 2e-5;
        assert!((opus.cost(&t) - want).abs() < 1e-12, "{}", opus.cost(&t));
        assert!((want - 0.206).abs() < 1e-9);
        // Sem preço de cache, o cache cobra como entrada.
        let (_, ds) = table().lookup("deepseek-chat").unwrap();
        let plain = Tokens {
            prompt: 1_000_000,
            completion: 0,
            cache_read: 400_000,
            cache_write: 0,
        };
        assert!((ds.cost(&plain) - 0.3).abs() < 1e-9);
        // Contagem inconsistente (cache > prompt) não vira custo negativo.
        let odd = Tokens {
            prompt: 10,
            cache_read: 50,
            ..Tokens::default()
        };
        assert!(ds.cost(&odd) >= 0.0);
    }

    #[test]
    fn cache_on_disk_is_read_and_slimmed() {
        let dir = std::env::temp_dir().join(format!("zerith-pricing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_cached(&dir).is_empty());
        let full = json!({"data": [{"id": "a/b", "canonical_slug": "a/b-1", "name": "B",
            "description": "longa", "pricing": {"prompt": "0.000001", "completion": "0.000002",
            "web_search": "0.01"}}]});
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_file(&dir), slim(&full).to_string()).unwrap();
        let t = load_cached(&dir);
        assert_eq!(t.lookup("a/b").map(|(_, p)| p.output), Some(0.000002));
        // fresca: `load` não toca a rede
        assert!(load(&dir).lookup("a/b-1").is_some());
        let text = std::fs::read_to_string(cache_file(&dir)).unwrap();
        assert!(!text.contains("longa") && !text.contains("web_search"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
