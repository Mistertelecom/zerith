//! Custo em dólares do uso gravado, com os preços públicos do OpenRouter
//! (`zerith_providers::pricing`). Conta de assinatura (OAuth) não é cobrada
//! por token: o valor dela sai à parte, como "equivalente na API".

use zerith_core::settings::AuthMode;
use zerith_core::Settings;
use zerith_providers::pricing::{PriceTable, Tokens};
use zerith_sessions::UsageCostRow;

/// Soma de custo de um grupo de linhas.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost {
    /// Cobrado por token (chave de API).
    pub billed_usd: f64,
    /// Quanto o uso das assinaturas custaria na API. Não é cobrado.
    pub subscription_usd: f64,
    /// Requisições de modelos sem preço no catálogo: o total fica por baixo.
    pub unknown_requests: u64,
    pub requests: u64,
}

impl Cost {
    pub fn add(&mut self, o: &Cost) {
        self.billed_usd += o.billed_usd;
        self.subscription_usd += o.subscription_usd;
        self.unknown_requests += o.unknown_requests;
        self.requests += o.requests;
    }

    /// Nada com preço conhecido.
    pub fn is_unknown(&self) -> bool {
        self.unknown_requests > 0 && self.unknown_requests == self.requests
    }
}

/// A conta (ou o provedor, quando a rota não usa conta) entra por OAuth: é
/// assinatura. Codex e Antigravity só existem assim.
pub fn is_subscription(settings: &Settings, account_id: &str, provider: &str) -> bool {
    if let Some(acc) = settings.account(account_id) {
        return acc.auth == AuthMode::Oauth;
    }
    let entry = settings
        .provider_entry(account_id)
        .or_else(|| settings.provider_entry(provider));
    entry.is_some_and(|e| e.auth == AuthMode::Oauth)
        || matches!(
            provider.to_ascii_lowercase().as_str(),
            "codex" | "antigravity"
        )
}

/// Custo de uma linha agrupada.
pub fn row_cost(table: &PriceTable, settings: &Settings, r: &UsageCostRow) -> Cost {
    let mut c = Cost {
        requests: r.requests,
        ..Cost::default()
    };
    // Uma linha só com falhas (0 tokens) não pede preço.
    if r.prompt_tokens == 0 && r.completion_tokens == 0 {
        return c;
    }
    let Some((_, price)) = table.lookup(&r.model) else {
        c.unknown_requests = r.requests;
        return c;
    };
    let usd = price.cost(&Tokens {
        prompt: r.prompt_tokens,
        completion: r.completion_tokens,
        cache_read: r.cache_read_tokens,
        cache_write: r.cache_write_tokens,
    });
    if is_subscription(settings, &r.account_id, &r.provider) {
        c.subscription_usd = usd;
    } else {
        c.billed_usd = usd;
    }
    c
}

/// Soma das linhas.
pub fn total(table: &PriceTable, settings: &Settings, rows: &[UsageCostRow]) -> Cost {
    let mut t = Cost::default();
    for r in rows {
        t.add(&row_cost(table, settings, r));
    }
    t
}

/// Modelos sem preço no catálogo, sem repetir.
pub fn unpriced_models(table: &PriceTable, rows: &[UsageCostRow]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in rows {
        let used = r.prompt_tokens > 0 || r.completion_tokens > 0;
        if used && table.lookup(&r.model).is_none() && !out.contains(&r.model) {
            out.push(r.model.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PriceTable {
        PriceTable::from_json(&serde_json::json!({"data": [
            {"id": "anthropic/claude-opus-5.5", "pricing": {"prompt": "0.000004",
             "completion": "0.00002", "input_cache_read": "0.0000002",
             "input_cache_write": "0.000005"}},
            {"id": "deepseek/deepseek-chat", "pricing": {"prompt": "0.0000003",
             "completion": "0.0000012"}}
        ]}))
    }

    fn row(account: &str, provider: &str, model: &str) -> UsageCostRow {
        UsageCostRow {
            account_id: account.into(),
            provider: provider.into(),
            model: model.into(),
            prompt_tokens: 1_000_000,
            completion_tokens: 100_000,
            cache_read_tokens: 500_000,
            requests: 3,
            ..UsageCostRow::default()
        }
    }

    #[test]
    fn subscription_goes_apart_and_unknown_is_never_priced() {
        let settings: Settings = toml::from_str(
            r#"
[[accounts]]
id = "claude-assinatura"
provider = "anthropic"
auth = "oauth"

[[accounts]]
id = "deepseek-trabalho"
provider = "deepseek"
"#,
        )
        .unwrap();
        let t = table();
        let sub = row_cost(
            &t,
            &settings,
            &row("claude-assinatura", "anthropic", "claude-opus-5-5"),
        );
        // 500k novos * 4e-6 + 500k do cache * 2e-7 + 100k * 2e-5
        assert!((sub.subscription_usd - 4.1).abs() < 1e-9, "{sub:?}");
        assert_eq!(sub.billed_usd, 0.0);

        let api = row_cost(
            &t,
            &settings,
            &row("deepseek-trabalho", "deepseek", "deepseek-chat"),
        );
        assert!((api.billed_usd - 0.42).abs() < 1e-9, "{api:?}");

        let none = row_cost(&t, &settings, &row("x", "ollama", "llama-local"));
        assert!(none.is_unknown());
        assert_eq!(none.billed_usd + none.subscription_usd, 0.0);

        let all = total(
            &t,
            &settings,
            &[
                row("claude-assinatura", "anthropic", "claude-opus-5-5"),
                row("x", "ollama", "llama-local"),
            ],
        );
        assert_eq!((all.requests, all.unknown_requests), (6, 3));
        assert!(!all.is_unknown());
        assert_eq!(
            unpriced_models(&t, &[row("x", "ollama", "llama-local")]),
            ["llama-local"]
        );
        // Codex sem conta declarada também é assinatura.
        assert!(is_subscription(&settings, "codex", "codex"));
        assert!(!is_subscription(&settings, "openai", "openai"));
        assert!(!is_subscription(&Settings::default(), "", ""));
        // Uma linha de falha (0 tokens) não é "sem preço".
        let failed = UsageCostRow {
            model: "llama-local".into(),
            requests: 1,
            ..UsageCostRow::default()
        };
        assert!(!row_cost(&t, &settings, &failed).is_unknown());
    }
}
