//! The model's view of the conversation, and how it shrinks.
//!
//! `messages` stays the replay source. A compaction never deletes rows: it
//! appends one user row whose `raw` says `{"summary": {start_id, end_id,
//! supersedes}}`, and replay then hides the rows in `start_id..=end_id` and
//! shows the summary in their place.
//!
//! Shrinking goes in three phases, cheapest first:
//! 1. prune: old tool results lose their body (in memory only, repeatable);
//! 2. summarize: the oldest span becomes one summary row (one model call);
//! 3. drop: if the summary failed or was not enough, the oldest turns go
//!    (in memory only). A compaction failure never fails the turn.

use std::collections::HashSet;

use serde_json::json;
use zerith_providers::ToolCall;
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderError, Role as PRole};
use zerith_sessions::{Message, NewMessage, Role, Store};

use crate::events::TurnLog;
use crate::{Result, RunError};

/// Replaces a tool result dropped to fit the context window.
pub const DROPPED: &str = "[saída removida para caber no contexto]";
/// Stands in for a result that was never recorded (crash or cancel mid-step):
/// the API rejects a tool call without an answer.
pub(crate) const INTERRUPTED: &str = "[sem resultado: o turno foi interrompido]";
/// First line of a summary message, so the model knows what it is reading.
pub const SUMMARY_HEADER: &str = "[resumo da conversa anterior, gerado automaticamente]";

/// The fixed instruction of the summary call.
pub const SUMMARY_PROMPT: &str =
    "Você resume o início de uma conversa entre um usuário e um agente de código, \
para que o agente continue o trabalho sem reler tudo. Escreva em português, em tópicos curtos: \
(1) o que o usuário pediu, com as palavras dele quando forem curtas; (2) decisões tomadas; \
(3) arquivos lidos ou alterados e comandos relevantes; (4) estado atual e pendências. \
Não invente nada que não esteja no texto. No máximo 400 palavras.";

/// How many rows `compact_now` reads.
const COMPACT_WINDOW: usize = 500;
/// Characters of the span sent to the summarizer, at most.
const TRANSCRIPT_CHARS: usize = 60_000;

/// Writes the summary of a span. A trait so tests (and hosts that prefer a
/// cheaper model) can replace the model call.
pub trait Summarizer {
    fn summarize(&self, transcript: &str, cancel: &Cancel) -> std::result::Result<String, String>;
}

/// The default: the turn's own client and model, one blocking request.
pub struct ClientSummarizer<'a> {
    pub client: &'a Client,
    pub model: &'a str,
}

impl Summarizer for ClientSummarizer<'_> {
    fn summarize(&self, transcript: &str, cancel: &Cancel) -> std::result::Result<String, String> {
        let req = ChatRequest::new(
            self.model,
            vec![
                ChatMessage::system(SUMMARY_PROMPT),
                ChatMessage::user(transcript),
            ],
        )
        .max_tokens(1500);
        self.client
            .complete(&req, cancel)
            .map(|c| c.text)
            .map_err(|e| e.to_string())
    }
}

/// One message of the model's view, with the rows it stands for.
#[derive(Debug, Clone)]
pub struct Item {
    pub msg: ChatMessage,
    /// First and last `messages.id` this item stands for; 0 for an item with
    /// no row (a synthetic "interrupted" result).
    pub lo: i64,
    pub hi: i64,
    /// Set when this item is a summary: the id of its row.
    pub summary_row: Option<i64>,
}

impl Item {
    pub fn plain(msg: ChatMessage) -> Self {
        Self {
            msg,
            lo: 0,
            hi: 0,
            summary_row: None,
        }
    }

    fn row(msg: ChatMessage, id: i64) -> Self {
        Self {
            msg,
            lo: id,
            hi: id,
            summary_row: None,
        }
    }
}

fn cost(m: &ChatMessage) -> usize {
    let calls: usize = m
        .tool_calls
        .iter()
        .map(|c| c.name.len() + c.arguments.len())
        .sum();
    (m.content.len() + calls) / 4 + 4
}

/// A rough 4 chars per token, plus a little per message for the framing.
pub fn estimate_tokens(msgs: &[ChatMessage]) -> usize {
    msgs.iter().map(cost).sum()
}

/// What the next request will carry: the system prompt, then `items`.
#[derive(Debug, Clone)]
pub struct History {
    pub system: Vec<ChatMessage>,
    pub items: Vec<Item>,
}

impl History {
    pub fn new(system: Vec<ChatMessage>, items: Vec<Item>) -> Self {
        Self { system, items }
    }

    pub fn chat(&self) -> Vec<ChatMessage> {
        self.system
            .iter()
            .cloned()
            .chain(self.items.iter().map(|i| i.msg.clone()))
            .collect()
    }

    pub fn tokens(&self) -> usize {
        self.system.iter().map(cost).sum::<usize>()
            + self.items.iter().map(|i| cost(&i.msg)).sum::<usize>()
    }

    /// Adds a message that was just stored as row `id`.
    pub fn push(&mut self, msg: ChatMessage, id: i64) {
        self.items.push(Item::row(msg, id));
    }

    /// Phase 1: oldest tool results lose their body until `budget` fits.
    pub fn prune_tools(&mut self, budget: usize) {
        let mut total = self.tokens();
        for it in &mut self.items {
            if total <= budget {
                return;
            }
            if it.msg.role == PRole::Tool && it.msg.content != DROPPED {
                let before = cost(&it.msg);
                it.msg.content = DROPPED.into();
                total -= before.saturating_sub(cost(&it.msg));
            }
        }
    }

    /// Phase 3: whole oldest turns go until `budget` fits. Everything from the
    /// latest user message on is kept.
    /// Returns how many items went.
    pub fn drop_oldest(&mut self, budget: usize) -> usize {
        let start_len = self.items.len();
        let mut last_user = self
            .items
            .iter()
            .rposition(|i| i.msg.role == PRole::User)
            .unwrap_or(0);
        while self.tokens() > budget {
            if last_user == 0 {
                break;
            }
            // A turn runs to the next user message, so a tool call never
            // loses its result to the cut.
            let end = self.items[1..]
                .iter()
                .position(|i| i.msg.role == PRole::User)
                .map(|p| 1 + p)
                .unwrap_or(self.items.len())
                .min(last_user);
            self.items.drain(..end);
            last_user -= end;
        }
        start_len - self.items.len()
    }

    /// Where phase 2 cuts: `items[..b]` is the span to summarize, the rest is
    /// kept. The kept tail fits `keep` tokens when possible, starts at a
    /// message that is not a tool result (a call never loses its answer), and
    /// never swallows the latest user message. `None`: nothing worth summarizing.
    pub fn plan_span(&self, keep: usize) -> Option<usize> {
        let last_user = self.items.iter().rposition(|i| i.msg.role == PRole::User)?;
        let mut tail = 0;
        let mut b0 = self.items.len();
        for i in (0..self.items.len()).rev() {
            tail += cost(&self.items[i].msg);
            if tail > keep {
                break;
            }
            b0 = i;
        }
        let b = (b0.max(1)..=last_user)
            .find(|&i| self.items[i].msg.role != PRole::Tool)
            .unwrap_or(last_user);
        // A lone summary is already as small as it gets.
        if b == 0 || (b == 1 && self.items[0].summary_row.is_some()) {
            return None;
        }
        Some(b)
    }

    /// `items[..b]` become one summary item.
    fn replace_span(&mut self, b: usize, content: String, lo: i64, hi: i64, row: i64) {
        self.items.splice(
            ..b,
            [Item {
                msg: ChatMessage::user(content),
                lo,
                hi,
                summary_row: Some(row),
            }],
        );
    }
}

/// Shrinks `msgs` to `budget` tokens without a model call: old tool results
/// lose their body, then whole oldest turns go. The leading system prompt and
/// everything from the latest user message on are kept.
pub fn fit_context(msgs: &mut Vec<ChatMessage>, budget: usize) {
    let lead = msgs.iter().take_while(|m| m.role == PRole::System).count();
    let items = msgs.split_off(lead).into_iter().map(Item::plain).collect();
    let mut h = History::new(std::mem::take(msgs), items);
    h.prune_tools(budget);
    let _ = h.drop_oldest(budget);
    *msgs = h.chat();
}

/// What a finished compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactReport {
    /// Messages replaced by the summary.
    pub summarized: usize,
    /// The row range now hidden from replay.
    pub start_id: i64,
    pub end_id: i64,
    /// The row holding the summary.
    pub summary_id: i64,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

fn transcript(items: &[Item]) -> String {
    let per = (TRANSCRIPT_CHARS / items.len().max(1)).clamp(200, 4000);
    let mut out = String::new();
    for it in items {
        let m = &it.msg;
        let who = match m.role {
            PRole::System => "sistema",
            PRole::User => "usuário",
            PRole::Assistant => "agente",
            PRole::Tool => "resultado de ferramenta",
        };
        let body: String = m.content.chars().take(per).collect();
        out.push_str(&format!("[{who}] {body}\n"));
        for c in &m.tool_calls {
            let args: String = c.arguments.chars().take(300).collect();
            out.push_str(&format!("[chamou {}] {args}\n", c.name));
        }
    }
    out
}

/// Phase 2 on `hist`: summarize the oldest span, store the summary row, swap
/// the span for it. Brackets the work with `compaction/start` ..
/// `compaction/end`; a start without an end is a crash. `Ok(None)`: nothing
/// to summarize. `Err`: the summary failed; `hist` is untouched and the end
/// event says so.
#[allow(clippy::too_many_arguments)]
pub(crate) fn summarize_into(
    store: &Store,
    session_id: &str,
    hist: &mut History,
    keep: usize,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
    log: &TurnLog<'_>,
    trigger: &str,
) -> std::result::Result<Option<CompactReport>, String> {
    let Some(b) = hist.plan_span(keep) else {
        return Ok(None);
    };
    let span = &hist.items[..b];
    let ids: Vec<&Item> = span.iter().filter(|i| i.lo > 0).collect();
    let (Some(lo), Some(hi)) = (
        ids.iter().map(|i| i.lo).min(),
        ids.iter().map(|i| i.hi).max(),
    ) else {
        return Ok(None);
    };
    let supersedes: Vec<i64> = span.iter().filter_map(|i| i.summary_row).collect();
    let tokens_before = hist.tokens();
    log.emit(
        "compaction/start",
        json!({"trigger": trigger, "items": b, "start_id": lo, "end_id": hi,
               "tokens_before": tokens_before}),
    );
    let fail = |e: String| {
        log.emit("compaction/end", json!({"ok": false, "error": e}));
        Err(e)
    };
    let text = match summarizer.summarize(&transcript(span), cancel) {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        Ok(_) => return fail("resumo vazio".into()),
        Err(e) => return fail(e),
    };
    let content = format!("{SUMMARY_HEADER}\n{text}");
    let row = match store.append(
        session_id,
        &NewMessage {
            raw: Some(json!({"summary": {"start_id": lo, "end_id": hi, "supersedes": supersedes}})),
            ..NewMessage::new(Role::User, content.clone())
        },
    ) {
        Ok(id) => id,
        Err(e) => return fail(e.to_string()),
    };
    log.emit(
        "compaction/summary",
        json!({"summary": text, "start_id": lo, "end_id": hi, "summary_id": row,
               "supersedes": supersedes, "items": b}),
    );
    hist.replace_span(b, content, lo, hi, row);
    let tokens_after = hist.tokens();
    log.emit(
        "compaction/end",
        json!({"ok": true, "tokens_before": tokens_before, "tokens_after": tokens_after}),
    );
    Ok(Some(CompactReport {
        summarized: b,
        start_id: lo,
        end_id: hi,
        summary_id: row,
        tokens_before,
        tokens_after,
    }))
}

/// Manual compaction (a future `/compact`): everything before the latest user
/// message becomes one summary row. `Ok(None)` when there is nothing to
/// summarize. Unlike the automatic path, a failed summary is an error here:
/// the person asked for it.
pub fn compact_now(
    store: &Store,
    client: &Client,
    session_id: &str,
    model: &str,
    cancel: &Cancel,
    summarizer: Option<&dyn Summarizer>,
) -> Result<Option<CompactReport>> {
    store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;
    let rows = store.recent_messages(session_id, COMPACT_WINDOW)?;
    let mut hist = History::new(Vec::new(), replay_items(&rows));
    let log = TurnLog::outside_turn(store, session_id);
    let default = ClientSummarizer { client, model };
    let s = summarizer.unwrap_or(&default);
    summarize_into(store, session_id, &mut hist, 0, s, cancel, &log, "manual")
        .map_err(|e| RunError::Provider(ProviderError::Transport(e)))
}

/// Stored rows as the model's view, with tool calls and results paired and
/// compacted ranges replaced by their summary.
pub fn replay_items(rows: &[Message]) -> Vec<Item> {
    let summary = |m: &Message| -> Option<(i64, i64, Vec<i64>)> {
        let s = m.raw.as_ref()?.get("summary")?;
        let ids = s["supersedes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();
        Some((s["start_id"].as_i64()?, s["end_id"].as_i64()?, ids))
    };
    let mut superseded: HashSet<i64> = HashSet::new();
    for m in rows {
        if let Some((_, _, ids)) = summary(m) {
            superseded.extend(ids);
        }
    }
    let ranges: Vec<(i64, i64)> = rows
        .iter()
        .filter(|m| !superseded.contains(&m.id))
        .filter_map(|m| summary(m).map(|(lo, hi, _)| (lo, hi)))
        .collect();

    // Sort key: a summary sits just before the first row it replaced.
    let mut out: Vec<(i64, Item)> = Vec::new();
    for m in rows {
        if let Some((lo, hi, _)) = summary(m) {
            if superseded.contains(&m.id) {
                continue;
            }
            out.push((
                lo * 2 - 1,
                Item {
                    msg: ChatMessage::user(m.content.clone()),
                    lo,
                    hi,
                    summary_row: Some(m.id),
                },
            ));
            continue;
        }
        if superseded.contains(&m.id) || ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&m.id)) {
            continue;
        }
        if let Some(msg) = row_to_chat(m) {
            out.push((m.id * 2, Item::row(msg, m.id)));
        }
    }
    out.sort_by_key(|(k, _)| *k);
    pair(out.into_iter().map(|(_, i)| i).collect())
}

fn row_to_chat(m: &Message) -> Option<ChatMessage> {
    match m.role {
        Role::User | Role::System => {
            if m.content.trim().is_empty() {
                return None;
            }
            let role = if m.role == Role::User {
                PRole::User
            } else {
                PRole::System
            };
            Some(ChatMessage::new(role, m.content.clone()))
        }
        Role::Assistant => {
            let calls: Vec<ToolCall> = m
                .tool_calls
                .clone()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            if m.content.trim().is_empty() && calls.is_empty() {
                return None;
            }
            Some(ChatMessage::assistant(m.content.clone(), calls))
        }
        // A tool row without its call id (imported, or pre-tool-loop) cannot
        // be paired, and unpaired it reads as a claim nobody made.
        Role::Tool => m
            .tool_calls
            .as_ref()
            .and_then(|v| v["tool_call_id"].as_str())
            .map(|id| ChatMessage::tool(id, m.content.clone())),
    }
}

/// Stored rows as provider messages. See [`replay_items`].
pub fn replay(rows: &[Message]) -> Vec<ChatMessage> {
    replay_items(rows).into_iter().map(|i| i.msg).collect()
}

/// Every assistant tool call gets exactly one result right after it; results
/// without a call (the call fell outside the history window) are dropped.
pub(crate) fn pair(items: Vec<Item>) -> Vec<Item> {
    fn close(out: &mut Vec<Item>, open: &mut Vec<String>) {
        for id in open.drain(..) {
            out.push(Item::plain(ChatMessage::tool(id, INTERRUPTED)));
        }
    }
    let mut out = Vec::with_capacity(items.len());
    let mut open: Vec<String> = Vec::new();
    for it in items {
        if it.msg.role == PRole::Tool {
            let id = it.msg.tool_call_id.as_deref().unwrap_or("");
            if let Some(pos) = open.iter().position(|o| o == id) {
                open.remove(pos);
                out.push(it);
            }
            continue;
        }
        close(&mut out, &mut open);
        if it.msg.role == PRole::Assistant {
            open = it.msg.tool_calls.iter().map(|c| c.id.clone()).collect();
        }
        out.push(it);
    }
    close(&mut out, &mut open);
    out
}
