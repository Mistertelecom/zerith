//! A provider error, said so a person can act on it.
//!
//! `ProviderError`'s `Display` is for logs: `http 401: {"error": {...}}` with
//! the server's whole JSON body. A window must not print that. This keeps the
//! short, useful part (what went wrong and what to do) and leaves the raw body
//! for the detail view.

use crate::ProviderError;

/// What to show a person: one sentence, in Portuguese, with the next step.
pub fn headline(e: &ProviderError) -> String {
    match e {
        ProviderError::NoEndpoint => {
            "Nenhum provedor configurado. Abra Provedores e informe a URL.".into()
        }
        ProviderError::NoKey => {
            "Falta a chave de API. Abra Provedores e salve a chave deste provedor.".into()
        }
        ProviderError::Http { status, body } => http(*status, body),
        ProviderError::RateLimited { retry_after_ms, .. } => match retry_after_ms {
            Some(ms) => format!(
                "Limite de requisições atingido. Tente de novo em {} s.",
                (ms / 1000).max(1)
            ),
            None => "Limite de requisições atingido. Espere um pouco e tente de novo.".into(),
        },
        ProviderError::Transport(m) => {
            format!(
                "Sem resposta do provedor ({}). Veja a URL e a conexão.",
                short(m)
            )
        }
        ProviderError::Json(_) | ProviderError::Malformed(_) => {
            "O provedor respondeu num formato inesperado. Confira se a URL termina em /v1.".into()
        }
        ProviderError::Cancelled => "Cancelado.".into(),
        ProviderError::Unsupported(what) => format!("Não suportado: {what}."),
    }
}

fn http(status: u16, body: &str) -> String {
    let detail = server_message(body);
    let why = match status {
        400 => "O provedor recusou o pedido",
        401 => "Chave de API ausente ou inválida. Abra Provedores e confira a chave",
        402 => "Sem crédito ou limite de gasto atingido no provedor",
        403 => "Acesso negado pelo provedor",
        404 => "Modelo ou endereço não encontrado. Confira o modelo e a URL",
        408 | 504 => "O provedor demorou demais para responder",
        413 => "O pedido ficou grande demais para o provedor",
        429 => "Limite de requisições atingido. Espere um pouco",
        500..=599 => "O provedor teve um erro interno. Tente de novo em instantes",
        _ => "O provedor devolveu um erro",
    };
    match detail {
        Some(d) => format!("{why} (HTTP {status}): {d}"),
        None => format!("{why} (HTTP {status})."),
    }
}

/// The human sentence inside an error body: `error.message`, or a plain-text
/// body when it is short. `None` when there is nothing readable.
pub fn server_message(body: &str) -> Option<String> {
    let t = body.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        let m = v
            .pointer("/error/message")
            .or_else(|| v.get("message"))
            .or_else(|| v.get("error"))
            .and_then(|x| x.as_str());
        return m.map(short);
    }
    // Plain text or HTML: keep it only when it is a short, tag-free line.
    (t.len() <= 160 && !t.contains('<')).then(|| short(t))
}

fn short(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 160 {
        format!("{}…", one.chars().take(160).collect::<String>())
    } else {
        one
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_401_with_a_json_body_becomes_one_sentence_with_the_next_step() {
        let e = ProviderError::Http {
            status: 401,
            body: r#"{"error":{"message":"Missing bearer authentication in header","type":"invalid_request_error","param":null,"code":null}}"#.into(),
        };
        let h = headline(&e);
        assert!(h.contains("Chave de API ausente ou inválida"), "{h}");
        assert!(h.contains("Missing bearer authentication in header"), "{h}");
        assert!(
            !h.contains('{') && !h.contains("param"),
            "raw JSON leaked: {h}"
        );
        assert!(!h.contains('\n'));
    }

    #[test]
    fn html_and_huge_bodies_are_not_printed() {
        let e = ProviderError::Http {
            status: 502,
            body: "<html><body>Bad gateway</body></html>".into(),
        };
        assert_eq!(
            headline(&e),
            "O provedor teve um erro interno. Tente de novo em instantes (HTTP 502)."
        );
        let long = ProviderError::Http {
            status: 400,
            body: "x".repeat(5000),
        };
        assert!(headline(&long).len() < 200);
    }

    #[test]
    fn the_other_errors_say_what_to_do() {
        assert!(headline(&ProviderError::NoKey).contains("Provedores"));
        assert!(headline(&ProviderError::NoEndpoint).contains("URL"));
        let rl = ProviderError::RateLimited {
            retry_after_ms: Some(12_000),
            body: String::new(),
        };
        assert!(headline(&rl).contains("12 s"));
    }
}
