//! `/btw <pergunta>`: uma pergunta paralela sobre a conversa, sem sujá-la.
//!
//! Uma chamada ao modelo da conversa com o mesmo prefixo que o turno manda
//! (o mesmo prompt de sistema, o mesmo replay do histórico, a mesma chave de
//! cache), mais a pergunta no fim, sem ferramentas. Nada é gravado em
//! `messages`, nada entra na caixa de entrada de um turno em andamento; só o
//! custo vai para `usage_records` (`role_kind = 'btw'`). Pode rodar enquanto
//! um turno roda: lê o histórico, não pega o slot do turno.
//!
//! Ideia e instrução da pergunta do `/btw` do Hermes
//! (`agent/side_question.py`: fork com paridade de cache, ferramentas
//! negadas, nada persistido; MIT). O código é do Zerith.

use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, Gateway, GatewayOutcome};
use zerith_sessions::{NewUsageRecord, Store};
use zerith_tools::Registry;

use crate::compact::{fit_context, replay_items, DEFAULT_CONTEXT};
use crate::{Result, RunError, TurnContext};

/// Quanto a resposta pode ter.
const MAX_TOKENS: u32 = 2048;

/// O que vai antes da pergunta (do `_FORK_PROMPT` do Hermes, em português).
pub const INSTRUCTION: &str = "[pergunta paralela (/btw) do usuário, enquanto o trabalho principal continua na conversa]
Regras:
- Responda SÓ a pergunta paralela, usando a conversa acima como contexto. Não continue, não refaça e não critique a tarefa principal.
- NÃO chame ferramentas: estão desligadas nesta pergunta. Responda direto, em texto.
- Se a conversa não tiver informação suficiente, diga isso em vez de adivinhar.
- Seja curto e direto.";

/// A resposta e o que ela custou.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    pub text: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
}

/// As mensagens da pergunta: o prefixo do turno (sistema + replay) e a
/// pergunta. `tools` é o registro que o turno usaria: só a orientação de uso
/// entra no prompt de sistema, para o prefixo ficar igual ao do turno.
#[allow(clippy::too_many_arguments)]
fn messages(
    store: &Store,
    ctx: TurnContext<'_>,
    tools: Option<&Registry>,
    history: usize,
    window: usize,
    folded: bool,
    session_id: &str,
    question: &str,
) -> Result<Vec<ChatMessage>> {
    let session = store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;
    // A mesma janela de replay do turno (`agent::turn_body`).
    let replay = if folded {
        2000
    } else {
        history.max(1) * (window / DEFAULT_CONTEXT).clamp(1, 8)
    };
    let rows = store.history_rows(session_id, replay)?;
    let items = replay_items(&rows);
    // O prompt de sistema montado com o último pedido real, como o turno o
    // montou: a lista de skills depende do pedido, e a pergunta não pode
    // mudar o prefixo. Sem memória: ela vai no lembrete do pedido (fora do
    // prefixo), e a montagem ainda gravaria fatos tirados da pergunta.
    let last_prompt = items
        .iter()
        .rev()
        .find(|i| i.msg.role == zerith_providers::Role::User && !i.msg.synthetic)
        .map_or("", |i| i.msg.content.as_str());
    let ctx = TurnContext {
        memory: None,
        ..ctx
    };
    let skill_tool = tools.is_some_and(|r| r.get("skill").is_some());
    let mut system =
        crate::build_prompt_context(&session.title, ctx, skill_tool, session_id, last_prompt)
            .static_system;
    if let Some(r) = tools {
        system.push_str(&crate::tool_guidance(r));
    }
    let mut msgs = vec![ChatMessage::system(system)];
    msgs.extend(items.into_iter().map(|i| i.msg));
    msgs.push(ChatMessage::user(format!("{INSTRUCTION}\n\n{question}")));
    if !folded {
        fit_context(&mut msgs, window * 8 / 10);
    }
    Ok(msgs)
}

/// Faz a pergunta paralela e devolve a resposta. Não grava nada na sessão.
#[allow(clippy::too_many_arguments)]
pub fn ask(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    tools: Option<&Registry>,
    history: usize,
    session_id: &str,
    model: &str,
    question: &str,
    cancel: &Cancel,
) -> Result<Answer> {
    let question = question.trim();
    if question.is_empty() {
        return Err(RunError::Provider(
            zerith_providers::ProviderError::Malformed("pergunta vazia".into()),
        ));
    }
    let _conversation = zerith_providers::bili::conversation(session_id);
    let mut gateway = match ctx.settings {
        Some(st) => crate::resolve_gateway(st, model, client.config()).0,
        None => Gateway::new(Vec::new()),
    };
    if gateway.candidates.is_empty() {
        gateway = crate::agent::default_gateway(client, model);
    }
    gateway.final_retry = Some(client.retry_policy());
    let first = &gateway.candidates[0];
    let window = first
        .config
        .context_limit
        .or(client.config().context_limit)
        .or_else(|| zerith_providers::models::context_window(&first.model))
        .filter(|&l| l > 0)
        .map_or(DEFAULT_CONTEXT, |l| l as usize);
    let folded = zerith_providers::bili::routes(&first.config);
    let msgs = messages(
        store, ctx, tools, history, window, folded, session_id, question,
    )?;
    // Sem `tools`: o modelo não tem como agir. Mesma chave de cache do turno.
    let req = ChatRequest::new(model, msgs)
        .prompt_cache_key(session_id)
        .max_tokens(MAX_TOKENS);
    let res = gateway.complete_with(
        &req,
        cancel,
        |c| {
            store
                .is_account_available(
                    &c.account_id,
                    c.window_hours,
                    c.quota_tokens,
                    c.quota_requests,
                )
                .unwrap_or(true)
        },
        |c, e, cooldown| {
            if cooldown > 0 {
                let until = zerith_sessions::migrations::now_ms()
                    .saturating_add(i64::try_from(cooldown).unwrap_or(i64::MAX));
                let _ = store.set_account_cooldown(&c.account_id, until, Some(&e.to_string()));
            }
            record(
                store,
                session_id,
                &c.account_id,
                &c.provider,
                &c.model,
                None,
                "error",
                0,
            );
        },
    );
    let out: GatewayOutcome<_> = res?;
    let usage = out.value.usage.clone().unwrap_or_default();
    record(
        store,
        session_id,
        &out.account_id,
        &out.provider,
        &out.model,
        Some(&usage),
        "ok",
        out.latency_ms,
    );
    Ok(Answer {
        text: out.value.text.trim().to_string(),
        model: out.model,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cache_read_tokens: usage.cache_read_tokens,
    })
}

#[allow(clippy::too_many_arguments)]
fn record(
    store: &Store,
    session_id: &str,
    account: &str,
    provider: &str,
    model: &str,
    usage: Option<&zerith_providers::Usage>,
    status: &str,
    latency_ms: u64,
) {
    let u = usage.cloned().unwrap_or_default();
    let _ = store.record_usage(&NewUsageRecord {
        session_id: Some(session_id.to_string()),
        account_id: account.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        role_kind: "btw".into(),
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        cache_read_tokens: u.cache_read_tokens,
        cache_write_tokens: u.cache_write_tokens,
        status: status.into(),
        latency_ms,
        ..NewUsageRecord::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use zerith_providers::{Protocol, ProviderConfig, ProviderKind, RetryPolicy};
    use zerith_sessions::SessionSource;

    /// Servidor que responde `responses` em ordem e guarda os corpos pedidos.
    fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut sock, _)) = listener.accept() else {
                    return;
                };
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                log.lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap_or_default());
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        (url, seen)
    }

    fn client(url: &str) -> Client {
        let mut cfg = ProviderConfig::new(ProviderKind::Custom).with_key("k");
        cfg.base_url = url.to_string();
        cfg.protocol = Protocol::OpenAi;
        cfg.timeout_ms = 5_000;
        Client::new(cfg).unwrap().with_retry(RetryPolicy {
            max_retries: 0,
            base_ms: 1,
            max_ms: 5,
            max_retry_after_ms: 1_000,
        })
    }

    #[test]
    fn btw_reuses_the_turn_prefix_without_tools_and_leaves_history_alone() {
        let sse = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"choices":[{"delta":{"content":"feito"}}]})
        );
        let body = serde_json::json!({
            "choices":[{"message":{"content":" use `route-map NOME permit 10` "},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":120,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":100}}
        })
        .to_string();
        let json = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, seen) = serve(vec![sse, json]);
        let client = client(&url);
        let store = Store::open_in_memory().unwrap();
        let sid = store
            .create_session("rede", SessionSource::Native)
            .unwrap()
            .id;
        let ctx = TurnContext {
            no_project_instructions: true,
            ..Default::default()
        };
        crate::run_agent_turn(
            &store,
            &client,
            ctx,
            crate::AgentOptions::default(),
            &sid,
            "m",
            "configure o BGP",
            &Cancel::default(),
            |_| {},
        )
        .unwrap();
        let before = store.messages(&sid, 100, 0).unwrap();

        let a = ask(
            &store,
            &client,
            ctx,
            None,
            crate::HISTORY_LIMIT,
            &sid,
            "m",
            "qual a sintaxe de route-map no NX-OS?",
            &Cancel::default(),
        )
        .unwrap();
        assert_eq!(a.text, "use `route-map NOME permit 10`");
        assert_eq!((a.prompt_tokens, a.cache_read_tokens), (120, 100));

        // Histórico intacto.
        assert_eq!(store.messages(&sid, 100, 0).unwrap(), before);

        let reqs = seen.lock().unwrap();
        let (turn, side) = (&reqs[0], &reqs[1]);
        // Sem ferramentas na pergunta.
        assert!(side.get("tools").is_none() && side.get("tool_choice").is_none());
        // O prefixo é o do turno: o mesmo sistema e o mesmo pedido, depois a
        // resposta, depois a pergunta.
        let t = turn["messages"].as_array().unwrap();
        let s = side["messages"].as_array().unwrap();
        assert_eq!(&s[..t.len()], &t[..]);
        assert_eq!(s.len(), t.len() + 2);
        assert_eq!(s[t.len()]["role"], "assistant");
        let q = s.last().unwrap()["content"].as_str().unwrap();
        assert!(q.starts_with("[pergunta paralela (/btw)") && q.ends_with("NX-OS?"));
        assert_eq!(side["prompt_cache_key"], turn["prompt_cache_key"]);

        // Custo registrado como btw.
        let n: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM usage_records WHERE role_kind = 'btw' AND prompt_tokens = 120",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn empty_question_or_unknown_session_fails() {
        let store = Store::open_in_memory().unwrap();
        let client = client("http://127.0.0.1:9/v1");
        let ctx = TurnContext::default();
        let c = Cancel::default();
        assert!(ask(&store, &client, ctx, None, 40, "x", "m", "  ", &c).is_err());
        assert!(matches!(
            ask(&store, &client, ctx, None, 40, "nada", "m", "oi", &c),
            Err(RunError::NoSession(_))
        ));
    }
}
