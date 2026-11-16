//! One agent turn, from user text to a stored assistant message.
//!
//! The whole harness lives here rather than in the CLI, because the same turn
//! has to run from the shell, from the Rune window and from a future RPC host.
//!
//! Two rules, both from the spec:
//!
//! 1. **A message the user saw is committed.** The user message is appended
//!    before the request goes out, so a crash mid-turn loses the answer, never
//!    the question.
//! 2. **Streaming does not write per delta.** Tokens are shown as they arrive
//!    and the assistant message is written once, whole, when the turn closes.

use std::sync::mpsc;
use zerith_memory::{MemoryBackend, Scope};
use zerith_providers::{ChatMessage, ChatRequest, Client, Completion, ProviderError, StreamEvent};
use zerith_sessions::{NewMessage, Role, Store};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("no such session: {0}")]
    NoSession(String),
}

pub type Result<T> = std::result::Result<T, RunError>;

/// Where the turn's output goes while it is being produced.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Delta(String),
    Reasoning(String),
    Done { stored: Option<i64> },
}

/// How many past messages are replayed to the model. A cap, not a summary:
/// compaction arrives with the memory phase, and pretending it exists now
/// would be worse than a visible limit.
pub const HISTORY_LIMIT: usize = 40;

/// How much of the prompt may be spent on recalled facts. A rough 4 chars per
/// token, so the number stays predictable without a tokenizer dependency.
pub const MEMORY_BUDGET_CHARS: usize = 2000;

/// How many facts one turn asks the backend for. Recall is a ranking, not a
/// dump; the budget trims what the ranking offered.
pub const MEMORY_HITS: usize = 12;

/// The turn's outcome, with the numbers a UI needs to show it honestly.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnReport {
    pub session_id: String,
    pub message_id: i64,
    pub text: String,
    pub reasoning: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Runs one turn and appends the answer.
///
/// `on_event` receives deltas as they arrive; it is also the only chance to see
/// partial output, since nothing half-written is persisted.
pub fn run_turn(
    store: &Store,
    client: &Client,
    session_id: &str,
    model: &str,
    prompt: &str,
    on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    run_turn_with_memory(
        store, client, None, None, session_id, model, prompt, on_event,
    )
}

/// The same turn, with durable memory attached.
///
/// Recall happens once per turn, against the user's text, and the result lands
/// *after* the static system prompt: the provider's prefix cache stays warm
/// while the facts change.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with_memory(
    store: &Store,
    client: &Client,
    memory: Option<&dyn MemoryBackend>,
    scope: Option<&Scope>,
    session_id: &str,
    model: &str,
    prompt: &str,
    mut on_event: impl FnMut(RunEvent),
) -> Result<TurnReport> {
    let session = store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;

    store.append(session_id, &NewMessage::new(Role::User, prompt))?;

    let mut messages = Vec::new();
    let mut prompt_text = zerith_core::system_prompt(&session.title);
    if let (Some(mem), Some(scope)) = (memory, scope) {
        // A fact the user stated is stored before the answer is asked for, so
        // the same turn can already recall it — and a crash after this point
        // still keeps what the user said.
        if let Some(f) = zerith_memory::extract(prompt) {
            let _ = mem.remember(scope, f.kind, &f.text, f.confidence, Some(session_id));
        }
        let known = recall_block(mem, scope, prompt);
        if !known.is_empty() {
            prompt_text.push_str("\n\n");
            prompt_text.push_str(&known);
        }
    }
    messages.push(ChatMessage::system(prompt_text));

    let history = store.messages(session_id, HISTORY_LIMIT, 0)?;
    for m in &history {
        // Tool rows are not replayed: without the tool call, their answer reads
        // as an assistant claim nobody made.
        let role = match m.role {
            Role::User => zerith_providers::Role::User,
            Role::Assistant => zerith_providers::Role::Assistant,
            Role::System => zerith_providers::Role::System,
            Role::Tool => continue,
        };
        if m.content.trim().is_empty() {
            continue;
        }
        messages.push(ChatMessage {
            role,
            content: m.content.clone(),
        });
    }

    let req = ChatRequest::new(model, messages);
    let cancel = zerith_providers::Cancel::default();

    // Streaming first: the window is the main host, and a turn that only
    // paints when it finishes reads as a hang. If the endpoint rejects the
    // stream before sending anything, fall back once to a blocking request —
    // some OpenAI-compatible servers answer 400 to `stream: true`.
    let mut text = String::new();
    let mut reasoning: Option<String> = None;
    let mut usage = None;
    let streamed = client.stream(&req, &cancel, |event| match event {
        StreamEvent::Delta(t) => {
            text.push_str(&t);
            on_event(RunEvent::Delta(t));
        }
        StreamEvent::Reasoning(r) => {
            reasoning = Some(match reasoning.take() {
                Some(mut acc) => {
                    acc.push_str(&r);
                    acc
                }
                None => r.clone(),
            });
            on_event(RunEvent::Reasoning(r));
        }
        // A trailing `[DONE]` carries no usage; it must not erase the
        // counts an earlier chunk already reported.
        StreamEvent::Done { usage: u } => {
            if u.is_some() {
                usage = u;
            }
        }
    });

    if let Err(e) = streamed {
        // Only a stream that produced nothing is retried: repeating a stream
        // that already painted text would show the answer twice.
        if !text.is_empty() || matches!(e, ProviderError::Cancelled) {
            return Err(e.into());
        }
        let completion = client.complete(&req, &cancel)?;
        if let Some(r) = &completion.reasoning {
            on_event(RunEvent::Reasoning(r.clone()));
        }
        on_event(RunEvent::Delta(completion.text.clone()));
        text = completion.text;
        reasoning = completion.reasoning;
        usage = completion.usage;
    }

    let completion = Completion {
        text,
        reasoning,
        usage,
        finish_reason: None,
    };

    let message_id = store.append(
        session_id,
        &NewMessage {
            reasoning: completion.reasoning.clone(),
            token_count: completion
                .usage
                .as_ref()
                .map(|u| u.completion_tokens as i64),
            ..NewMessage::new(Role::Assistant, completion.text.clone())
        },
    )?;
    on_event(RunEvent::Done {
        stored: Some(message_id),
    });

    let (prompt_tokens, completion_tokens) = completion
        .usage
        .map(|u| (u.prompt_tokens, u.completion_tokens))
        .unwrap_or((0, 0));

    Ok(TurnReport {
        session_id: session_id.to_string(),
        message_id,
        text: completion.text,
        reasoning: completion.reasoning,
        prompt_tokens,
        completion_tokens,
    })
}

/// A helper for hosts that drive a turn on a worker thread and read events on
/// the UI thread: a turn must never block a frame.
/// Renders the recalled facts as a prompt section, trimmed to the budget.
///
/// A failed recall is not a failed turn: memory is an aid, and losing it must
/// degrade the answer, not block it.
pub(crate) fn recall_block(memory: &dyn MemoryBackend, scope: &Scope, query: &str) -> String {
    let Ok(hits) = memory.recall(scope, query, MEMORY_HITS) else {
        return String::new();
    };
    let mut block = String::from("# O que você já sabe sobre o usuário\n");
    let mut used = block.len();
    let mut any = false;
    for hit in hits {
        if hit.fact.superseded_by.is_some() {
            continue;
        }
        let line = format!(
            "- [{}] {}\n",
            hit.fact.confidence.as_str(),
            hit.fact.text.trim()
        );
        if used + line.len() > MEMORY_BUDGET_CHARS {
            break;
        }
        used += line.len();
        block.push_str(&line);
        any = true;
    }
    if any {
        block
    } else {
        String::new()
    }
}

pub fn run_turn_async(
    store: Store,
    client: Client,
    session_id: String,
    model: String,
    prompt: String,
) -> (
    mpsc::Receiver<RunEvent>,
    std::thread::JoinHandle<Result<TurnReport>>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        run_turn(&store, &client, &session_id, &model, &prompt, |e| {
            let _ = tx.send(e);
        })
    });
    (rx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_memory::backend::{Fact, FactKind, MemoryError, RecallHit, Result as MemResult};

    /// A backend that answers from a fixed list, so the prompt rendering can be
    /// tested without a database or a network.
    struct Fixed(Vec<String>);

    impl MemoryBackend for Fixed {
        fn remember(
            &self,
            _scope: &Scope,
            _kind: FactKind,
            _text: &str,
            _confidence: zerith_memory::Confidence,
            _source: Option<&str>,
        ) -> MemResult<i64> {
            Ok(1)
        }

        fn recall(&self, _s: &Scope, _q: &str, limit: usize) -> MemResult<Vec<RecallHit>> {
            Ok(self
                .0
                .iter()
                .take(limit)
                .enumerate()
                .map(|(i, text)| RecallHit {
                    fact: Fact {
                        id: i as i64 + 1,
                        scope: "user:test".into(),
                        kind: FactKind::Fact,
                        text: text.clone(),
                        source: None,
                        session_id: None,
                        confidence: zerith_memory::Confidence::Confirmed,
                        created_at: 0,
                        updated_at: 0,
                        superseded_by: None,
                    },
                    score: 0.0,
                })
                .collect())
        }

        fn list(&self, _s: &Scope, _l: usize) -> MemResult<Vec<Fact>> {
            Err(MemoryError::Empty)
        }
        fn get(&self, id: i64) -> MemResult<Fact> {
            Err(MemoryError::NotFound(id))
        }
        fn update(&self, _id: i64, _t: &str) -> MemResult<()> {
            Ok(())
        }
        fn forget(&self, _id: i64) -> MemResult<()> {
            Ok(())
        }
        fn supersede(&self, _o: i64, _b: i64) -> MemResult<()> {
            Ok(())
        }
        fn export(&self, _s: &Scope) -> MemResult<Vec<Fact>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn facts_are_rendered_with_their_confidence() {
        let mem = Fixed(vec!["o projeto usa Rust".into()]);
        let block = recall_block(&mem, &Scope::user("t"), "x");
        assert!(block.contains("[confirmed] o projeto usa Rust"), "{block}");
    }

    #[test]
    fn an_empty_memory_adds_no_section_at_all() {
        let mem = Fixed(vec![]);
        assert_eq!(recall_block(&mem, &Scope::user("t"), "x"), "");
    }

    #[test]
    fn the_block_stops_at_the_budget_instead_of_growing_forever() {
        let big = "x".repeat(500);
        let mem = Fixed(vec![big; 20]);
        let block = recall_block(&mem, &Scope::user("t"), "x");
        assert!(block.len() <= MEMORY_BUDGET_CHARS + 128, "{}", block.len());
    }
}
