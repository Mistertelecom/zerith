//! Gateway local nativo multi-provedores e multi-contas com resolução de
//! destino, suporte a combos e failover automático antes do primeiro token.

use crate::config::{ProviderConfig, ProviderKind};
use crate::retry::RetryPolicy;
use crate::{Cancel, ChatRequest, Client, Completion, ProviderError, Result, StreamEvent};
use std::time::Instant;

/// Especificação de modelo resolvida a partir de strings como:
/// - `"gpt-4o"`
/// - `"anthropic/claude-sonnet-4"`
/// - `"claude/claude-3-5-sonnet"`
/// - `"acc-work:anthropic/claude-sonnet-4"`
/// - `"openrouter/meta-llama/llama-3.3-70b"`
/// - `"qwen3:8b"` (tag Ollama preservada quando `"qwen3"` não é conta)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub account_id: Option<String>,
    pub provider: Option<String>,
    pub model: String,
}

impl ModelSpec {
    /// Analisa `raw` sem lista de contas conhecidas (não interpreta prefixos
    /// `lhs:` como `account_id`).
    pub fn parse(raw: &str) -> Self {
        Self::parse_with_accounts(raw, &[])
    }

    /// Analisa `raw` reconhecendo `<account_id>:<rest>` quando `account_id`
    /// está em `accounts`, e `<provider>/<model>` para provedores conhecidos.
    pub fn parse_with_accounts(raw: &str, accounts: &[&str]) -> Self {
        let trimmed = raw.trim();
        let (account_id, rest) = match trimmed.split_once(':') {
            Some((lhs, rhs)) if accounts.contains(&lhs) => (Some(lhs.to_string()), rhs),
            _ => (None, trimmed),
        };
        let (provider, model) = match rest.split_once('/') {
            Some((lhs, rhs)) => match canonical_provider_name(lhs) {
                Some(canon) => (Some(canon.to_string()), rhs.to_string()),
                None => (None, rest.to_string()),
            },
            None => (None, rest.to_string()),
        };
        Self {
            account_id,
            provider,
            model,
        }
    }
}

fn canonical_provider_name(lhs: &str) -> Option<&'static str> {
    match lhs.to_ascii_lowercase().as_str() {
        "openai" => Some("openai"),
        "anthropic" | "claude" => Some("anthropic"),
        "deepseek" => Some("deepseek"),
        "gemini" => Some("gemini"),
        "openrouter" => Some("openrouter"),
        "ollama" => Some("ollama"),
        "lmstudio" => Some("lmstudio"),
        "custom" => Some("custom"),
        _ => None,
    }
}

/// Infere o [`ProviderKind`] nativo a partir do nome nu do modelo, quando o
/// prefixo identifica inequivocamente um provedor conhecido.
pub fn infer_provider(model: &str) -> Option<ProviderKind> {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("claude-") {
        Some(ProviderKind::Anthropic)
    } else if m.starts_with("deepseek-") {
        Some(ProviderKind::DeepSeek)
    } else if m.starts_with("gemini-") {
        Some(ProviderKind::Gemini)
    } else if m.starts_with("gpt-")
        || m == "o1"
        || m.starts_with("o1-")
        || m == "o3"
        || m.starts_with("o3-")
        || m == "o4"
        || m.starts_with("o4-")
        || m.starts_with("chatgpt-")
    {
        Some(ProviderKind::OpenAi)
    } else {
        None
    }
}

/// Um candidato concreto de roteamento (conta + provedor + modelo + config).
#[derive(Debug, Clone)]
pub struct RouteCandidate {
    pub account_id: String,
    pub provider: String,
    pub model: String,
    pub config: ProviderConfig,
    pub priority: i32,
    pub quota_tokens: Option<u64>,
    pub quota_requests: Option<u64>,
    pub window_hours: u32,
}

/// Resultado de uma chamada executada através do [`Gateway`].
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayOutcome<T> {
    pub value: T,
    pub account_id: String,
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
    pub attempts: usize,
}

/// Indica se o erro deve acionar failover automático para a próxima conta/alvo
/// da cadeia (cota esgotada, credencial ausente/inválida, pagamento, modelo que
/// a conta não tem mais (404), timeout, rede ou 5xx).
pub fn is_failover_error(err: &ProviderError) -> bool {
    match err {
        ProviderError::RateLimited { .. } | ProviderError::NoKey | ProviderError::Transport(_) => {
            true
        }
        ProviderError::Http { status, .. } => {
            matches!(*status, 401..=404 | 408 | 429 | 500..=599)
        }
        ProviderError::Cancelled
        | ProviderError::NoEndpoint
        | ProviderError::Malformed(_)
        | ProviderError::Json(_)
        | ProviderError::Unsupported(_) => false,
    }
}

/// Tempo de cooldown sugerido (em ms) para colocar uma conta em espera após
/// um erro de failover.
pub fn suggested_cooldown_ms(err: &ProviderError) -> u64 {
    match err {
        ProviderError::RateLimited {
            retry_after_ms: Some(ms),
            ..
        } => (*ms).clamp(5_000, 3_600_000),
        ProviderError::RateLimited {
            retry_after_ms: None,
            ..
        } => 60_000,
        ProviderError::Http {
            status: 401..=403, ..
        } => 300_000,
        ProviderError::Http { status: 429, .. } => 60_000,
        // the model is gone, not the account: its other models keep working
        ProviderError::Http { status: 404, .. } => 0,
        ProviderError::Http {
            status: 500..=599, ..
        }
        | ProviderError::Transport(_) => 15_000,
        _ => 10_000,
    }
}

/// Gateway local multi-contas com suporte a failover automático e skip de
/// contas em cooldown ou sem cota.
#[derive(Debug, Clone)]
pub struct Gateway {
    pub candidates: Vec<RouteCandidate>,
    /// Política de retry opcional para cada tentativa intermediária antes do
    /// failover (padrão: `RetryPolicy::none()` quando há mais contas).
    pub intermediate_retry: RetryPolicy,
    /// Política de retry opcional para a última conta da cadeia (`None` =
    /// padrão do `Client`).
    pub final_retry: Option<RetryPolicy>,
}

impl Gateway {
    pub fn new(candidates: Vec<RouteCandidate>) -> Self {
        Self {
            candidates,
            intermediate_retry: RetryPolicy::none(),
            final_retry: None,
        }
    }

    fn build_client(&self, cand: &RouteCandidate, has_more: bool) -> Result<Client> {
        let mut cfg = cand.config.clone();
        cfg.account_id = Some(cand.account_id.clone());
        let mut client = Client::new(cfg)?;
        if has_more {
            client = client.with_retry(self.intermediate_retry);
        } else if let Some(retry) = self.final_retry {
            client = client.with_retry(retry);
        }
        Ok(client)
    }

    /// Executa uma requisição em streaming tentando cada candidato permitido por
    /// `can_use`. Faz failover em erros elegíveis apenas enquanto nenhum delta
    /// ou raciocínio tiver sido emitido para o chamador.
    pub fn stream_with<FAllow, FFail, FEv>(
        &self,
        req: &ChatRequest,
        cancel: &Cancel,
        mut can_use: FAllow,
        mut on_failover: FFail,
        mut on_event: FEv,
    ) -> Result<GatewayOutcome<()>>
    where
        FAllow: FnMut(&RouteCandidate) -> bool,
        FFail: FnMut(&RouteCandidate, &ProviderError, u64),
        FEv: FnMut(StreamEvent),
    {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if self.candidates.is_empty() {
            return Err(ProviderError::NoEndpoint);
        }

        let started = Instant::now();
        let mut attempts = 0usize;
        let mut last_err: Option<ProviderError> = None;

        for (idx, cand) in self.candidates.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            if !can_use(cand) {
                continue;
            }
            attempts += 1;

            let client = match self.build_client(cand, idx + 1 < self.candidates.len()) {
                Ok(c) => c,
                Err(err) => {
                    if is_failover_error(&err) {
                        on_failover(cand, &err, suggested_cooldown_ms(&err));
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let mut cand_req = req.clone();
            cand_req.model = cand.model.clone();

            let mut painted = false;
            let streamed = client.stream(&cand_req, cancel, |ev| {
                if matches!(ev, StreamEvent::Delta(_) | StreamEvent::Reasoning(_)) {
                    painted = true;
                }
                on_event(ev);
            });

            match streamed {
                Ok(()) => return Ok(build_outcome((), cand, started, attempts)),
                Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
                Err(err) if painted => return Err(err),
                Err(err) if is_failover_error(&err) => {
                    on_failover(cand, &err, suggested_cooldown_ms(&err));
                    last_err = Some(err);
                }
                Err(ProviderError::Http { status: 400, .. }) => {
                    // Alguns servidores OpenAI-compatíveis rejeitam `stream: true`
                    // com HTTP 400; tenta uma vez em modo bloqueante antes de falhar.
                    match client.complete(&cand_req, cancel) {
                        Ok(completion) => {
                            emit_completion_events(completion, &mut on_event);
                            return Ok(build_outcome((), cand, started, attempts));
                        }
                        Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
                        Err(err2) if is_failover_error(&err2) => {
                            on_failover(cand, &err2, suggested_cooldown_ms(&err2));
                            last_err = Some(err2);
                        }
                        Err(err2) => return Err(err2),
                    }
                }
                Err(err) => return Err(err),
            }
        }

        Err(last_err.unwrap_or_else(all_accounts_exhausted_error))
    }

    /// Executa uma requisição bloqueante tentando cada candidato permitido por
    /// `can_use`, com a mesma política de skip e failover de [`Self::stream_with`].
    pub fn complete_with<FAllow, FFail>(
        &self,
        req: &ChatRequest,
        cancel: &Cancel,
        mut can_use: FAllow,
        mut on_failover: FFail,
    ) -> Result<GatewayOutcome<Completion>>
    where
        FAllow: FnMut(&RouteCandidate) -> bool,
        FFail: FnMut(&RouteCandidate, &ProviderError, u64),
    {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if self.candidates.is_empty() {
            return Err(ProviderError::NoEndpoint);
        }

        let started = Instant::now();
        let mut attempts = 0usize;
        let mut last_err: Option<ProviderError> = None;

        for (idx, cand) in self.candidates.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            if !can_use(cand) {
                continue;
            }
            attempts += 1;

            let client = match self.build_client(cand, idx + 1 < self.candidates.len()) {
                Ok(c) => c,
                Err(err) => {
                    if is_failover_error(&err) {
                        on_failover(cand, &err, suggested_cooldown_ms(&err));
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let mut cand_req = req.clone();
            cand_req.model = cand.model.clone();

            match client.complete(&cand_req, cancel) {
                Ok(completion) => return Ok(build_outcome(completion, cand, started, attempts)),
                Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
                Err(err) if is_failover_error(&err) => {
                    on_failover(cand, &err, suggested_cooldown_ms(&err));
                    last_err = Some(err);
                }
                Err(err) => return Err(err),
            }
        }

        Err(last_err.unwrap_or_else(all_accounts_exhausted_error))
    }
}

fn build_outcome<T>(
    value: T,
    cand: &RouteCandidate,
    started: Instant,
    attempts: usize,
) -> GatewayOutcome<T> {
    GatewayOutcome {
        value,
        account_id: cand.account_id.clone(),
        provider: cand.provider.clone(),
        model: cand.model.clone(),
        latency_ms: started.elapsed().as_millis() as u64,
        attempts,
    }
}

fn emit_completion_events(completion: Completion, on_event: &mut impl FnMut(StreamEvent)) {
    if let Some(reasoning) = completion.reasoning.filter(|r| !r.is_empty()) {
        on_event(StreamEvent::Reasoning(reasoning));
    }
    if !completion.text.is_empty() {
        on_event(StreamEvent::Delta(completion.text));
    }
    if !completion.tool_calls.is_empty() {
        on_event(StreamEvent::ToolCalls(completion.tool_calls));
    }
    on_event(StreamEvent::Done {
        usage: completion.usage,
        stop_reason: completion.finish_reason,
    });
}

fn all_accounts_exhausted_error() -> ProviderError {
    ProviderError::RateLimited {
        retry_after_ms: None,
        body: "todas as contas configuradas atingiram o limite ou estão em cooldown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatMessage, Protocol, ToolCall, Usage};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    enum MockResponse {
        Raw(String),
        ChunkedAbrupt(Vec<String>),
    }

    struct MockServer {
        url: String,
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<(String, Value)>>>,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn start(responses: Vec<MockResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let url = format!("http://{addr}/v1");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&seen);
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);

            let handle = std::thread::spawn(move || {
                for resp in responses {
                    let Ok((mut sock, _)) = listener.accept() else {
                        break;
                    };
                    if stop_flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut r = BufReader::new(sock.try_clone().unwrap());
                    let mut first = String::new();
                    if r.read_line(&mut first).is_err() || first.is_empty() {
                        break;
                    }
                    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                    let mut len = 0usize;
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; len];
                    if len > 0 && r.read_exact(&mut body).is_err() {
                        break;
                    }
                    log.lock()
                        .unwrap()
                        .push((path, serde_json::from_slice(&body).unwrap_or(Value::Null)));

                    match resp {
                        MockResponse::Raw(raw) => {
                            let _ = sock.write_all(raw.as_bytes());
                            let _ = sock.flush();
                        }
                        MockResponse::ChunkedAbrupt(chunks) => {
                            let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                            let _ = sock.write_all(hdr.as_bytes());
                            for chunk in chunks {
                                let frame = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                                let _ = sock.write_all(frame.as_bytes());
                            }
                            let _ = sock.flush();
                        }
                    }
                }
            });

            Self {
                url,
                addr,
                seen,
                stop,
                handle: Some(handle),
            }
        }

        fn requests(&self) -> Vec<(String, Value)> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.addr);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn sse_text(text: &str) -> MockResponse {
        let mut s = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        );
        let chunk = json!({"choices":[{"delta":{"content":text}}]});
        let usage = json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}});
        s.push_str(&format!("data: {chunk}\n\n"));
        s.push_str(&format!("data: {usage}\n\n"));
        s.push_str("data: [DONE]\n\n");
        MockResponse::Raw(s)
    }

    fn http_status(code: u16, extra_headers: &str, body: &str) -> MockResponse {
        MockResponse::Raw(format!(
            "HTTP/1.1 {code} Err\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ))
    }

    fn http_json(body: &Value) -> MockResponse {
        let raw = body.to_string();
        MockResponse::Raw(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",
            raw.len()
        ))
    }

    fn candidate(account_id: &str, provider: &str, model: &str, url: &str) -> RouteCandidate {
        let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("test-key");
        cfg.base_url = url.to_string();
        cfg.protocol = Protocol::OpenAi;
        cfg.timeout_ms = 5_000;
        RouteCandidate {
            account_id: account_id.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            config: cfg,
            priority: 0,
            quota_tokens: None,
            quota_requests: None,
            window_hours: 24,
        }
    }

    #[test]
    fn an_error_inside_a_200_stream_fails_over_before_the_first_token() {
        // overloaded, said inside the stream instead of as an HTTP status
        let s1 = MockServer::start(vec![MockResponse::Raw(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
             data: {\"error\":{\"code\":529,\"message\":\"Overloaded\"}}\n\n"
                .into(),
        )]);
        let s2 = MockServer::start(vec![sse_text("ok da conta 2")]);
        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "m", &s1.url),
            candidate("acc-2", "openai", "m", &s2.url),
        ]);
        let req = ChatRequest::new("m", vec![ChatMessage::user("oi")]);
        let mut failovers = Vec::new();
        let outcome = gw
            .stream_with(
                &req,
                &Cancel::default(),
                |_| true,
                |c, e, cd| failovers.push((c.account_id.clone(), e.to_string(), cd)),
                |_| {},
            )
            .unwrap();
        assert_eq!(outcome.account_id, "acc-2");
        assert_eq!(failovers.len(), 1, "{failovers:?}");
        assert!(failovers[0].1.contains("529"));
        assert_eq!(s2.requests().len(), 1);
    }

    #[test]
    fn model_spec_parses_provider_account_and_ollama_tags() {
        assert_eq!(
            ModelSpec::parse("gpt-4o"),
            ModelSpec {
                account_id: None,
                provider: None,
                model: "gpt-4o".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse("anthropic/claude-sonnet-4"),
            ModelSpec {
                account_id: None,
                provider: Some("anthropic".into()),
                model: "claude-sonnet-4".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse("Claude/claude-3-5-sonnet"),
            ModelSpec {
                account_id: None,
                provider: Some("anthropic".into()),
                model: "claude-3-5-sonnet".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse("meta-llama/llama-3.3-70b"),
            ModelSpec {
                account_id: None,
                provider: None,
                model: "meta-llama/llama-3.3-70b".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse("openrouter/meta-llama/llama-3.3-70b"),
            ModelSpec {
                account_id: None,
                provider: Some("openrouter".into()),
                model: "meta-llama/llama-3.3-70b".into(),
            }
        );

        let accounts = ["work", "personal"];
        assert_eq!(
            ModelSpec::parse_with_accounts("qwen3:8b", &accounts),
            ModelSpec {
                account_id: None,
                provider: None,
                model: "qwen3:8b".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse_with_accounts("ollama/qwen3:8b", &accounts),
            ModelSpec {
                account_id: None,
                provider: Some("ollama".into()),
                model: "qwen3:8b".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse_with_accounts("work:ollama/qwen3:8b", &accounts),
            ModelSpec {
                account_id: Some("work".into()),
                provider: Some("ollama".into()),
                model: "qwen3:8b".into(),
            }
        );

        assert_eq!(
            ModelSpec::parse_with_accounts("personal:meta-llama/llama-3.3-70b", &accounts),
            ModelSpec {
                account_id: Some("personal".into()),
                provider: None,
                model: "meta-llama/llama-3.3-70b".into(),
            }
        );

        assert_eq!(
            infer_provider("claude-3-7-sonnet"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(
            infer_provider("deepseek-chat"),
            Some(ProviderKind::DeepSeek)
        );
        assert_eq!(infer_provider("gemini-2.5-pro"), Some(ProviderKind::Gemini));
        assert_eq!(infer_provider("gpt-4o"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o1"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o1-mini"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o3"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o3-mini"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o4"), Some(ProviderKind::OpenAi));
        assert_eq!(infer_provider("o4-mini"), Some(ProviderKind::OpenAi));
        assert_eq!(
            infer_provider("chatgpt-4o-latest"),
            Some(ProviderKind::OpenAi)
        );
        assert_eq!(infer_provider("qwen3:8b"), None);
    }

    #[test]
    fn failover_switches_from_exhausted_429_account_to_second_account() {
        let s1 = MockServer::start(vec![http_status(
            429,
            "Retry-After: 12\r\n",
            "rate limit exceeded",
        )]);
        let s2 = MockServer::start(vec![sse_text("olá da conta 2")]);

        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "gpt-4o", &s1.url),
            candidate("acc-2", "openai", "gpt-4o-mini", &s2.url),
        ]);
        let req = ChatRequest::new("ignored-combo", vec![ChatMessage::user("oi")]);
        let cancel = Cancel::default();

        let mut failovers = Vec::new();
        let mut text = String::new();
        let mut saw_done = false;

        let outcome = gw
            .stream_with(
                &req,
                &cancel,
                |_| true,
                |cand, err, cooldown| {
                    failovers.push((cand.account_id.clone(), err.to_string(), cooldown));
                },
                |ev| match ev {
                    StreamEvent::Delta(d) => text.push_str(&d),
                    StreamEvent::Done { .. } => saw_done = true,
                    _ => {}
                },
            )
            .unwrap();

        assert_eq!(outcome.account_id, "acc-2");
        assert_eq!(outcome.provider, "openai");
        assert_eq!(outcome.model, "gpt-4o-mini");
        assert_eq!(outcome.attempts, 2);
        assert_eq!(text, "olá da conta 2");
        assert!(saw_done);
        assert_eq!(failovers.len(), 1);
        assert_eq!(failovers[0].0, "acc-1");
        assert_eq!(failovers[0].2, 12_000);
        assert_eq!(s1.requests().len(), 1);
        assert_eq!(s1.requests()[0].1["model"], "gpt-4o");
        assert_eq!(s2.requests().len(), 1);
        assert_eq!(s2.requests()[0].1["model"], "gpt-4o-mini");
    }

    #[test]
    fn a_model_the_account_no_longer_has_fails_over_without_cooling_the_account() {
        // Antigravity answers 404 "Requested entity was not found" for a
        // retired model: the next target serves the turn, the account stays up
        let s1 = MockServer::start(vec![http_status(
            404,
            "",
            "Requested entity was not found.",
        )]);
        let s2 = MockServer::start(vec![sse_text("ok")]);
        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "retired-model", &s1.url),
            candidate("acc-2", "openai", "gpt-4o", &s2.url),
        ]);
        let req = ChatRequest::new("x", vec![ChatMessage::user("oi")]);
        let mut failovers = Vec::new();
        let outcome = gw
            .stream_with(
                &req,
                &Cancel::default(),
                |_| true,
                |c, _, cooldown| failovers.push((c.account_id.clone(), cooldown)),
                |_| {},
            )
            .unwrap();
        assert_eq!(outcome.account_id, "acc-2");
        assert_eq!(failovers, vec![("acc-1".to_string(), 0)]);
    }

    #[test]
    fn unavailable_accounts_by_can_use_are_skipped_without_network_call() {
        let s1 = MockServer::start(vec![sse_text("não deveria chamar")]);
        let s2 = MockServer::start(vec![sse_text("ok da conta 2")]);

        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "gpt-4o", &s1.url),
            candidate("acc-2", "openai", "gpt-4o", &s2.url),
        ]);
        let req = ChatRequest::new("gpt-4o", vec![ChatMessage::user("oi")]);
        let cancel = Cancel::default();

        let mut failovers = 0usize;
        let mut text = String::new();
        let outcome = gw
            .stream_with(
                &req,
                &cancel,
                |cand| cand.account_id != "acc-1",
                |_, _, _| failovers += 1,
                |ev| {
                    if let StreamEvent::Delta(d) = ev {
                        text.push_str(&d);
                    }
                },
            )
            .unwrap();

        assert_eq!(outcome.account_id, "acc-2");
        assert_eq!(outcome.attempts, 1);
        assert_eq!(text, "ok da conta 2");
        assert_eq!(failovers, 0);
        assert!(s1.requests().is_empty());
        assert_eq!(s2.requests().len(), 1);

        let all_blocked =
            gw.stream_with(&req, &cancel, |_| false, |_, _, _| failovers += 1, |_| {});
        assert!(matches!(
            all_blocked,
            Err(ProviderError::RateLimited {
                retry_after_ms: None,
                ref body
            }) if body.contains("todas as contas configuradas")
        ));
    }

    #[test]
    fn no_failover_once_stream_already_painted_a_delta() {
        let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"parcial\"}}]}\n\n".to_string();
        let s1 = MockServer::start(vec![MockResponse::ChunkedAbrupt(vec![chunk])]);
        let s2 = MockServer::start(vec![sse_text("conta 2 não deve rodar")]);

        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "gpt-4o", &s1.url),
            candidate("acc-2", "openai", "gpt-4o", &s2.url),
        ]);
        let req = ChatRequest::new("gpt-4o", vec![ChatMessage::user("oi")]);
        let cancel = Cancel::default();

        let mut failovers = 0usize;
        let mut events = Vec::new();
        let res = gw.stream_with(
            &req,
            &cancel,
            |_| true,
            |_, _, _| failovers += 1,
            |ev| events.push(ev),
        );

        assert!(matches!(res, Err(ProviderError::Transport(_))));
        assert_eq!(events, vec![StreamEvent::Delta("parcial".into())]);
        assert_eq!(failovers, 0);
        assert_eq!(s1.requests().len(), 1);
        assert!(s2.requests().is_empty());
    }

    #[test]
    fn http_400_falls_back_to_blocking_complete_and_emits_events() {
        let s1 = MockServer::start(vec![
            http_status(400, "", r#"{"error":"streaming disabled"}"#),
            http_json(&json!({
                "choices": [{
                    "message": {
                        "content": "resposta final",
                        "reasoning_content": "raciocínio",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
            })),
        ]);
        let gw = Gateway::new(vec![candidate("acc-1", "openai", "gpt-4o", &s1.url)]);
        let req = ChatRequest::new("gpt-4o", vec![ChatMessage::user("oi")]);
        let cancel = Cancel::default();

        let mut events = Vec::new();
        let outcome = gw
            .stream_with(&req, &cancel, |_| true, |_, _, _| {}, |ev| events.push(ev))
            .unwrap();

        assert_eq!(outcome.account_id, "acc-1");
        assert_eq!(outcome.attempts, 1);
        assert_eq!(
            events,
            vec![
                StreamEvent::Reasoning("raciocínio".into()),
                StreamEvent::Delta("resposta final".into()),
                StreamEvent::ToolCalls(vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"a\"}".into(),
                }]),
                StreamEvent::Done {
                    usage: Some(Usage {
                        prompt_tokens: 3,
                        completion_tokens: 4,
                        total_tokens: 7,
                        ..Default::default()
                    }),
                    stop_reason: Some("tool_calls".into()),
                }
            ]
        );
        assert_eq!(s1.requests().len(), 2);
        assert_eq!(s1.requests()[0].1["stream"], true);
        assert_eq!(s1.requests()[1].1["stream"], false);
    }

    #[test]
    fn complete_with_skips_unavailable_and_fails_over_on_402() {
        let s1 = MockServer::start(vec![http_json(&json!({}))]);
        let s2 = MockServer::start(vec![http_status(402, "", "payment required")]);
        let s3 = MockServer::start(vec![http_json(&json!({
            "choices": [{"message": {"content": "sucesso conta 3"}, "finish_reason": "stop"}]
        }))]);

        let gw = Gateway::new(vec![
            candidate("acc-1", "openai", "gpt-4o", &s1.url),
            candidate("acc-2", "openai", "gpt-4o", &s2.url),
            candidate("acc-3", "deepseek", "deepseek-chat", &s3.url),
        ]);
        let req = ChatRequest::new("combo", vec![ChatMessage::user("oi")]);
        let cancel = Cancel::default();

        let mut failovers = Vec::new();
        let outcome = gw
            .complete_with(
                &req,
                &cancel,
                |c| c.account_id != "acc-1",
                |c, _, cd| failovers.push((c.account_id.clone(), cd)),
            )
            .unwrap();

        assert_eq!(outcome.account_id, "acc-3");
        assert_eq!(outcome.provider, "deepseek");
        assert_eq!(outcome.model, "deepseek-chat");
        assert_eq!(outcome.attempts, 2);
        assert_eq!(outcome.value.text, "sucesso conta 3");
        assert_eq!(failovers, vec![("acc-2".to_string(), 300_000)]);
        assert!(s1.requests().is_empty());
        assert_eq!(s2.requests().len(), 1);
        assert_eq!(s3.requests().len(), 1);
    }

    #[test]
    fn error_classification_and_cooldowns_match_policy() {
        assert!(is_failover_error(&ProviderError::RateLimited {
            retry_after_ms: None,
            body: String::new(),
        }));
        assert!(is_failover_error(&ProviderError::NoKey));
        assert!(is_failover_error(&ProviderError::Transport("eof".into())));
        for code in [401, 402, 403, 408, 429, 500, 502, 503] {
            assert!(
                is_failover_error(&ProviderError::Http {
                    status: code,
                    body: String::new(),
                }),
                "status {code}"
            );
        }

        assert!(!is_failover_error(&ProviderError::Cancelled));
        assert!(!is_failover_error(&ProviderError::NoEndpoint));
        assert!(!is_failover_error(&ProviderError::Malformed("x".into())));
        let json_err = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(!is_failover_error(&ProviderError::Json(json_err)));
        assert!(!is_failover_error(&ProviderError::Http {
            status: 400,
            body: String::new(),
        }));

        assert_eq!(
            suggested_cooldown_ms(&ProviderError::RateLimited {
                retry_after_ms: Some(100),
                body: String::new(),
            }),
            5_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::RateLimited {
                retry_after_ms: Some(9_999_999),
                body: String::new(),
            }),
            3_600_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::RateLimited {
                retry_after_ms: None,
                body: String::new(),
            }),
            60_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::Http {
                status: 402,
                body: String::new(),
            }),
            300_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::Http {
                status: 429,
                body: String::new(),
            }),
            60_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::Http {
                status: 503,
                body: String::new(),
            }),
            15_000
        );
        assert_eq!(
            suggested_cooldown_ms(&ProviderError::Transport("x".into())),
            15_000
        );
        assert_eq!(suggested_cooldown_ms(&ProviderError::NoKey), 10_000);
    }
}
