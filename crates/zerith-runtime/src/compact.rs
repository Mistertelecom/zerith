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
use zerith_sessions::{Message, Role, Store};

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
    "Você resume o início de uma conversa entre um usuário e um agente técnico, \
para que o agente continue o trabalho sem reler tudo. Escreva em português, em tópicos objetivos: \
(1) pedido do usuário; (2) decisões tomadas; (3) entidades exatas (preserve literalmente IPs, CIDRs, \
ASNs, VLANs, VRFs, nomes de interfaces, hostnames, caminhos de arquivo, símbolos de código e mensagens de erro); \
(4) arquivos/equipamentos lidos ou alterados; (5) estado atual e pendências. \
Não invente nada que não esteja no texto. No máximo 500 palavras.";

/// How many rows `compact_now` reads.
const COMPACT_WINDOW: usize = 500;
/// Context window assumed when neither the provider nor the account says.
// ponytail: one number for every model (Claude 200k, Gemini 1M, GPT 400k);
// a per-model table belongs in the provider catalogue if early summaries hurt.
pub const DEFAULT_CONTEXT: usize = 128_000;
/// History share of the window that triggers a summary before the turn.
pub const AUTO_COMPACT_PCT: usize = 75;
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
                ChatMessage::synthetic_user(transcript),
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

/// Tokens a provider bills for one attached image, about (Claude ~1.6k for a
/// screenshot); the `![..](path)` text itself is a few chars.
const IMAGE_TOKENS: usize = 1600;

fn cost(m: &ChatMessage) -> usize {
    let calls: usize = m
        .tool_calls
        .iter()
        .map(|c| c.name.len() + c.arguments.len())
        .sum();
    // ponytail: counts every `![` in a user message, file or not; local_images
    // reads the file, too slow to run per estimate.
    let images = match m.role {
        PRole::User => m.content.matches("![").count(),
        _ => 0,
    };
    (m.content.len() + calls) / 4 + 4 + images * IMAGE_TOKENS
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
                msg: ChatMessage::synthetic_user(content),
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
    let snapshot_max_id = hist
        .items
        .iter()
        .map(|i| i.hi.max(i.summary_row.unwrap_or(0)))
        .max()
        .unwrap_or(hi);
    let expected_ids: Vec<i64> = ids
        .iter()
        .map(|it| it.summary_row.unwrap_or(it.lo))
        .collect();
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
    // Checked before the (paid, slow) summary too: a doomed span must not
    // call the summarizer on every step only to throw the text away.
    match skip_reason(
        store,
        session_id,
        snapshot_max_id,
        &expected_ids,
        &supersedes,
        lo,
        hi,
    ) {
        Ok(Some(reason)) => {
            log.emit(
                "compaction/end",
                json!({"ok": false, "skipped": true, "reason": reason}),
            );
            return Ok(None);
        }
        Ok(None) => {}
        Err(e) => return fail(e),
    }
    let text = match summarizer.summarize(&transcript(span), cancel) {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        Ok(_) => return fail("resumo vazio".into()),
        Err(e) => return fail(e),
    };
    if cancel.is_cancelled() {
        log.emit(
            "compaction/end",
            json!({"ok": false, "skipped": true, "reason": "cancelled"}),
        );
        return Ok(None);
    }
    let content = format!("{SUMMARY_HEADER}\n{text}");
    let raw_json =
        json!({"summary": {"start_id": lo, "end_id": hi, "supersedes": supersedes}}).to_string();

    struct ImmediateTx<'a>(&'a Store, bool);
    impl<'a> ImmediateTx<'a> {
        fn begin(store: &'a Store) -> std::result::Result<Self, String> {
            store
                .conn()
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| e.to_string())?;
            Ok(Self(store, false))
        }
        fn commit(mut self) -> std::result::Result<(), String> {
            self.0
                .conn()
                .execute_batch("COMMIT")
                .map_err(|e| e.to_string())?;
            self.1 = true;
            Ok(())
        }
    }
    impl Drop for ImmediateTx<'_> {
        fn drop(&mut self) {
            if !self.1 {
                let _ = self.0.conn().execute_batch("ROLLBACK");
            }
        }
    }

    enum TxOutcome {
        Inserted(i64),
        Skipped(&'static str),
    }

    let tx_outcome = {
        let tx = match ImmediateTx::begin(store) {
            Ok(t) => t,
            Err(e) => return fail(e),
        };
        let check = (|| -> std::result::Result<TxOutcome, String> {
            if let Some(reason) = skip_reason(
                store,
                session_id,
                snapshot_max_id,
                &expected_ids,
                &supersedes,
                lo,
                hi,
            )? {
                return Ok(TxOutcome::Skipped(reason));
            }
            let conn = store.conn();
            let ts = zerith_sessions::migrations::now_ms();
            conn.execute(
                "INSERT INTO messages(session_id, role, content, tool_name, tool_calls, reasoning, token_count, created_at, raw)
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, ?4, ?5)",
                (session_id, "user", &content, ts, &raw_json),
            )
            .map_err(|e| e.to_string())?;
            let id = conn.last_insert_rowid();
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                (ts, session_id),
            )
            .map_err(|e| e.to_string())?;
            Ok(TxOutcome::Inserted(id))
        })();
        match check {
            Ok(TxOutcome::Inserted(id)) => {
                if let Err(e) = tx.commit() {
                    return fail(e);
                }
                TxOutcome::Inserted(id)
            }
            Ok(TxOutcome::Skipped(reason)) => {
                drop(tx);
                TxOutcome::Skipped(reason)
            }
            Err(e) => {
                drop(tx);
                return fail(e);
            }
        }
    };
    let row = match tx_outcome {
        TxOutcome::Inserted(id) => id,
        TxOutcome::Skipped(reason) => {
            log.emit(
                "compaction/end",
                json!({"ok": false, "skipped": true, "reason": reason}),
            );
            return Ok(None);
        }
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

/// Why storing a summary of `lo..=hi` would be wrong now, if it would: the
/// session is gone, rows arrived after the snapshot, a row of the span was
/// rewound, or a live summary already covers part of it. A summary named in
/// any live summary's `supersedes` is dead, whatever its flag says (the same
/// rule as [`replay_items`]).
#[allow(clippy::too_many_arguments)]
fn skip_reason(
    store: &Store,
    session_id: &str,
    snapshot_max_id: i64,
    expected_ids: &[i64],
    supersedes: &[i64],
    lo: i64,
    hi: i64,
) -> std::result::Result<Option<&'static str>, String> {
    let conn = store.conn();
    let session_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            (session_id,),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if session_exists == 0 {
        return Ok(Some("session_deleted"));
    }
    let db_max_id: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(id), 0) FROM messages WHERE session_id = ?1",
            (session_id,),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if db_max_id > snapshot_max_id {
        return Ok(Some("stale_snapshot"));
    }
    let mut sup_stmt = conn
        .prepare("SELECT superseded FROM messages WHERE id = ?1 AND session_id = ?2")
        .map_err(|e| e.to_string())?;
    for &row_id in expected_ids {
        let sup: std::result::Result<i64, _> =
            sup_stmt.query_row((row_id, session_id), |r| r.get(0));
        if sup.ok() != Some(0) {
            return Ok(Some("superseded_or_missing"));
        }
    }
    let mut sum_stmt = conn
        .prepare(
            "SELECT id, raw FROM messages WHERE session_id = ?1 AND superseded = 0
             AND CASE WHEN raw IS NULL OR NOT json_valid(raw) THEN 0
                 ELSE json_type(raw, '$.summary.start_id') IS NOT NULL END",
        )
        .map_err(|e| e.to_string())?;
    let summaries: Vec<(i64, serde_json::Value)> = sum_stmt
        .query_map((session_id,), |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|(id, raw)| Some((id, serde_json::from_str(&raw).ok()?)))
        .collect();
    let dead: HashSet<i64> = summaries
        .iter()
        .filter_map(|(_, v)| v["summary"]["supersedes"].as_array())
        .flatten()
        .filter_map(|v| v.as_i64())
        .chain(supersedes.iter().copied())
        .collect();
    let covered = summaries.iter().any(|(id, v)| {
        let s = &v["summary"];
        !dead.contains(id)
            && matches!((s["start_id"].as_i64(), s["end_id"].as_i64()),
                        (Some(s_lo), Some(s_hi)) if s_lo <= hi && s_hi >= lo)
    });
    Ok(covered.then_some("already_summarized"))
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
    let default = ClientSummarizer { client, model };
    compact_with(
        store,
        session_id,
        summarizer.unwrap_or(&default),
        cancel,
        "manual",
    )
}

fn compact_with(
    store: &Store,
    session_id: &str,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
    trigger: &str,
) -> Result<Option<CompactReport>> {
    store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;
    let rows = store.recent_messages(session_id, COMPACT_WINDOW)?;
    let mut hist = History::new(Vec::new(), replay_items(&rows));
    let log = TurnLog::outside_turn(store, session_id);
    summarize_into(
        store, session_id, &mut hist, 0, summarizer, cancel, &log, trigger,
    )
    .map_err(|e| RunError::Provider(ProviderError::Transport(e)))
}

/// Whether a history of `tokens` should be summarized before the next turn:
/// past `pct`% of the window (`context_limit`, else [`DEFAULT_CONTEXT`]).
pub fn needs_compaction_at(tokens: usize, context_limit: Option<u32>, pct: usize) -> bool {
    let window = context_limit
        .filter(|&l| l > 0)
        .map_or(DEFAULT_CONTEXT, |l| l as usize);
    tokens * 100 > window * pct
}

/// Whether a history of `tokens` should be summarized before the next turn:
/// past [`AUTO_COMPACT_PCT`] of the window (`context_limit`, else
/// [`DEFAULT_CONTEXT`]).
pub fn needs_compaction(tokens: usize, context_limit: Option<u32>) -> bool {
    needs_compaction_at(tokens, context_limit, AUTO_COMPACT_PCT)
}

/// Picks the configured summarization model (`compact_model` -> `explore_model`
/// -> active conversation model).
pub(crate) fn compact_model_spec<'a>(
    settings: &'a zerith_core::Settings,
    fallback: &'a str,
) -> &'a str {
    settings
        .routing
        .compact_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            settings
                .routing
                .explore_model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(fallback)
}

/// `auto_compact` for the account `model` is routed to: its window (the
/// account's, else the model table), its client as the summarizer, and no
/// summary at all when billion-context folds that account's history. The
/// one every host calls before a turn.
pub fn auto_compact_routed(
    store: &Store,
    settings: &zerith_core::Settings,
    client: &Client,
    session_id: &str,
    model: &str,
    history: usize,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    auto_compact_routed_at(
        store,
        settings,
        client,
        session_id,
        model,
        history,
        AUTO_COMPACT_PCT,
        cancel,
    )
}

/// Background pre-compaction trigger at 70% of the active model's context window.
pub fn pre_compact_routed(
    store: &Store,
    settings: &zerith_core::Settings,
    client: &Client,
    session_id: &str,
    model: &str,
    history: usize,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    auto_compact_routed_at(
        store, settings, client, session_id, model, history, 70, cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn auto_compact_routed_at(
    store: &Store,
    settings: &zerith_core::Settings,
    client: &Client,
    session_id: &str,
    model: &str,
    history: usize,
    threshold_pct: usize,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    let first = crate::resolve_gateway(settings, model, client.config())
        .0
        .candidates
        .into_iter()
        .next();
    // billion-context folds on the wire; a summary here would be a second
    // compressor rewriting the prefix it caches.
    if zerith_providers::bili::routes(first.as_ref().map_or(client.config(), |c| &c.config)) {
        return Ok(None);
    }
    let limit = first
        .as_ref()
        .and_then(|c| c.config.context_limit)
        .or(client.config().context_limit)
        .or_else(|| {
            zerith_providers::models::context_window(first.as_ref().map_or(model, |c| &c.model))
        });

    let sum_spec = compact_model_spec(settings, model);
    let sum_cand = if sum_spec == model {
        first
    } else {
        crate::resolve_gateway(settings, sum_spec, client.config())
            .0
            .candidates
            .into_iter()
            .next()
    };
    let routed = sum_cand.and_then(|c| Client::new(c.config).ok().map(|cl| (cl, c.model)));
    let (client, sum_model) = match &routed {
        Some((cl, m)) => (cl, m.as_str()),
        None => (client, model),
    };
    auto_compact_at(
        store,
        session_id,
        history,
        limit,
        threshold_pct,
        &ClientSummarizer {
            client,
            model: sum_model,
        },
        cancel,
    )
}

/// Before a turn: when the `history` rows the turn will replay pass the
/// threshold, the same summary as `/compact` replaces the older ones.
/// `Ok(None)`: below the threshold, or nothing to summarize.
pub fn auto_compact(
    store: &Store,
    session_id: &str,
    history: usize,
    context_limit: Option<u32>,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    auto_compact_at(
        store,
        session_id,
        history,
        context_limit,
        AUTO_COMPACT_PCT,
        summarizer,
        cancel,
    )
}

pub fn auto_compact_at(
    store: &Store,
    session_id: &str,
    history: usize,
    context_limit: Option<u32>,
    threshold_pct: usize,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    // ponytail: counts the replayed rows only; the system prompt and tool
    // specs (a few k tokens) ride on the margin.
    let rows = store.history_rows(session_id, history.max(1))?;
    let tokens: usize = replay_items(&rows).iter().map(|i| cost(&i.msg)).sum();
    if !needs_compaction_at(tokens, context_limit, threshold_pct) {
        return Ok(None);
    }
    compact_with(store, session_id, summarizer, cancel, "auto")
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
                    msg: ChatMessage::synthetic_user(m.content.clone()),
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

fn is_synthetic_raw(raw: Option<&serde_json::Value>) -> bool {
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return false;
    };
    obj.contains_key("tool_context")
        || obj.contains_key("hook_context")
        || obj.contains_key("hook_stop")
        || obj.contains_key("subagents_joined")
        || obj.contains_key("monitor")
        || obj.contains_key("summary")
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
            let mut msg = ChatMessage::new(role, m.content.clone());
            if m.role == Role::User && is_synthetic_raw(m.raw.as_ref()) {
                msg.synthetic = true;
            }
            Some(msg)
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

#[cfg(test)]
mod routing_tests {
    use super::*;

    #[test]
    fn compact_model_prefers_compact_then_explore_then_fallback() {
        let mut s = zerith_core::Settings::default();
        assert_eq!(compact_model_spec(&s, "main-m"), "main-m");
        s.routing.explore_model = Some("exp-m".into());
        assert_eq!(compact_model_spec(&s, "main-m"), "exp-m");
        s.routing.compact_model = Some("cmp-m".into());
        assert_eq!(compact_model_spec(&s, "main-m"), "cmp-m");
        assert!(SUMMARY_PROMPT.contains("CIDR"));
        assert!(needs_compaction_at(71, Some(100), 70));
        assert!(!needs_compaction_at(70, Some(100), 70));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerith_sessions::{NewMessage, SessionSource};

    struct Canned;
    impl Summarizer for Canned {
        fn summarize(&self, t: &str, _: &Cancel) -> std::result::Result<String, String> {
            assert!(
                t.contains("pedido 0"),
                "the oldest turn goes to the summary"
            );
            Ok("o usuário pediu 30 coisas".into())
        }
    }

    #[test]
    fn an_image_in_history_costs_like_an_image_not_like_its_path() {
        let plain = ChatMessage::user("veja");
        let shot = ChatMessage::user("veja\n![tela](C:/a/b.png)");
        assert!(cost(&shot) - cost(&plain) >= IMAGE_TOKENS);
        // a path the assistant merely mentions is text
        let said = ChatMessage::assistant("![tela](C:/a/b.png)", vec![]);
        assert!(cost(&said) < 20);
    }

    #[test]
    fn the_threshold_is_three_quarters_of_the_window() {
        assert!(!needs_compaction(75_000, Some(100_000)));
        assert!(needs_compaction(75_001, Some(100_000)));
        // no window known: 128k
        assert!(!needs_compaction(96_000, None));
        assert!(needs_compaction(96_001, None));
        assert!(needs_compaction(96_001, Some(0)), "0 means unknown");
    }

    /// A long history crosses the line: one summary row replaces every turn
    /// but the last, and the next replay shows it in their place.
    #[test]
    fn a_long_history_is_summarized_before_the_turn() {
        let store = Store::open_in_memory().unwrap();
        let sid = store
            .create_session("longa", SessionSource::Native)
            .unwrap()
            .id;
        let filler = "x".repeat(4_000); // ~1000 tokens per message
        for i in 0..30 {
            store
                .append(
                    &sid,
                    &NewMessage::new(Role::User, format!("pedido {i} {filler}")),
                )
                .unwrap();
            store
                .append(
                    &sid,
                    &NewMessage::new(Role::Assistant, format!("feito {i} {filler}")),
                )
                .unwrap();
        }
        let c = Cancel::default();
        // 60 messages ~ 60k tokens: under 75% of 128k, nothing happens
        assert!(auto_compact(&store, &sid, 100, None, &Canned, &c)
            .unwrap()
            .is_none());
        // a 64k window: over the line
        let r = auto_compact(&store, &sid, 100, Some(64_000), &Canned, &c)
            .unwrap()
            .expect("the trigger fires");
        assert_eq!(r.summarized, 58, "everything before the last user message");
        assert!(r.tokens_after < r.tokens_before / 10);
        let replay = replay(&store.recent_messages(&sid, 500).unwrap());
        assert_eq!(replay.len(), 3, "summary + last prompt + last answer");
        assert!(replay[0].content.starts_with(SUMMARY_HEADER));
        assert!(replay[1].content.starts_with("pedido 29"));
        // and the history now fits: no second summary on the next turn
        assert!(auto_compact(&store, &sid, 100, Some(64_000), &Canned, &c)
            .unwrap()
            .is_none());
    }

    #[test]
    fn replayed_synthetic_user_rows_preserve_synthetic_flag() {
        let store = Store::open_in_memory().unwrap();
        let sid = store
            .create_session("syn", zerith_sessions::SessionSource::Native)
            .unwrap()
            .id;
        store
            .append(&sid, &NewMessage::new(Role::User, "real ![a](x.png)"))
            .unwrap();
        store
            .append(
                &sid,
                &NewMessage {
                    raw: Some(json!({"steer": true})),
                    ..NewMessage::new(Role::User, "steer ![b](y.png)")
                },
            )
            .unwrap();
        for key in [
            "tool_context",
            "hook_context",
            "hook_stop",
            "subagents_joined",
            "monitor",
        ] {
            store
                .append(
                    &sid,
                    &NewMessage {
                        raw: Some(json!({ key: true })),
                        ..NewMessage::new(Role::User, format!("{key} ![s](secret.png)"))
                    },
                )
                .unwrap();
        }
        store
            .append(
                &sid,
                &NewMessage {
                    raw: Some(
                        json!({"summary": {"start_id": 999, "end_id": 1000, "supersedes": []}}),
                    ),
                    ..NewMessage::new(Role::User, "summary ![s](secret.png)")
                },
            )
            .unwrap();

        let replayed = replay(&store.recent_messages(&sid, 50).unwrap());
        assert_eq!(replayed.len(), 8);
        assert!(
            !replayed[0].synthetic,
            "plain user message is not synthetic"
        );
        assert!(
            !replayed[1].synthetic,
            "steer user message is not synthetic"
        );
        for m in &replayed[2..] {
            assert!(m.synthetic, "synthetic row must remain synthetic: {m:?}");
        }
    }

    #[test]
    fn third_compaction_ignores_summary_superseded_by_newer_one() {
        struct Count(std::cell::Cell<usize>);
        impl Summarizer for Count {
            fn summarize(&self, _: &str, _: &Cancel) -> std::result::Result<String, String> {
                self.0.set(self.0.get() + 1);
                Ok(format!("resumo {}", self.0.get()))
            }
        }
        let store = Store::open_in_memory().unwrap();
        let sid = store
            .create_session("3x", SessionSource::Native)
            .unwrap()
            .id;
        let sum = Count(std::cell::Cell::new(0));
        let c = Cancel::default();
        for round in 0..3 {
            for i in 0..3 {
                store
                    .append(&sid, &NewMessage::new(Role::User, format!("p{round}-{i}")))
                    .unwrap();
                store
                    .append(
                        &sid,
                        &NewMessage::new(Role::Assistant, format!("r{round}-{i}")),
                    )
                    .unwrap();
            }
            let r = compact_with(&store, &sid, &sum, &c, "manual").unwrap();
            assert!(r.is_some(), "compaction {round} must store a summary");
        }
        assert_eq!(sum.0.get(), 3);
    }

    #[test]
    fn stale_summarize_into_aborts_cleanly_after_newer_message_or_superseded_rewind() {
        struct MutatingSummarizer<F: Fn()>(F);
        impl<F: Fn()> Summarizer for MutatingSummarizer<F> {
            fn summarize(&self, _t: &str, _: &Cancel) -> std::result::Result<String, String> {
                (self.0)();
                Ok("resumo obsoleto".into())
            }
        }

        let scratch =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(&scratch).unwrap();
        let db = dir.path().join("compact-stale.db");

        // Case 1: Concurrent newer message appended mid-summarization.
        {
            let store = Store::open(&db).unwrap();
            let sid = store
                .create_session("c1", SessionSource::Native)
                .unwrap()
                .id;
            for i in 0..4 {
                store
                    .append(&sid, &NewMessage::new(Role::User, format!("u{i}")))
                    .unwrap();
                store
                    .append(&sid, &NewMessage::new(Role::Assistant, format!("a{i}")))
                    .unwrap();
            }
            let rows = store.recent_messages(&sid, 100).unwrap();
            let mut hist = History::new(Vec::new(), replay_items(&rows));
            let db2 = db.clone();
            let sid2 = sid.clone();
            let log = TurnLog::outside_turn(&store, &sid);
            let sum = MutatingSummarizer(move || {
                let s2 = Store::open(&db2).unwrap();
                s2.append(&sid2, &NewMessage::new(Role::User, "concurrent msg"))
                    .unwrap();
            });
            let res = summarize_into(
                &store,
                &sid,
                &mut hist,
                0,
                &sum,
                &Cancel::default(),
                &log,
                "auto",
            )
            .unwrap();
            assert!(res.is_none(), "stale snapshot must skip inserting summary");
            assert!(!store.has_open_compaction(&sid).unwrap());
            let all = store.messages(&sid, 100, 0).unwrap();
            assert!(all.iter().all(|m| !m.content.contains("resumo obsoleto")));
        }

        // Case 2: Concurrent rewind supersedes rows in lo..=hi mid-summarization.
        {
            let store = Store::open(&db).unwrap();
            let sid = store
                .create_session("c2", SessionSource::Native)
                .unwrap()
                .id;
            let mut ids = Vec::new();
            for i in 0..4 {
                ids.push(
                    store
                        .append(&sid, &NewMessage::new(Role::User, format!("u{i}")))
                        .unwrap(),
                );
                ids.push(
                    store
                        .append(&sid, &NewMessage::new(Role::Assistant, format!("a{i}")))
                        .unwrap(),
                );
            }
            let boundary = ids[2];
            let rows = store.recent_messages(&sid, 100).unwrap();
            let mut hist = History::new(Vec::new(), replay_items(&rows));
            let db2 = db.clone();
            let sid2 = sid.clone();
            let log = TurnLog::outside_turn(&store, &sid);
            let sum = MutatingSummarizer(move || {
                let s2 = Store::open(&db2).unwrap();
                s2.conn()
                    .execute(
                        "UPDATE messages SET superseded = 1 WHERE session_id = ?1 AND id > ?2",
                        (&sid2, boundary),
                    )
                    .unwrap();
            });
            let res = summarize_into(
                &store,
                &sid,
                &mut hist,
                0,
                &sum,
                &Cancel::default(),
                &log,
                "auto",
            )
            .unwrap();
            assert!(res.is_none(), "rewound rows must prevent summary insertion");
            assert!(!store.has_open_compaction(&sid).unwrap());
            let all = store.messages(&sid, 100, 0).unwrap();
            assert!(all.iter().all(|m| !m.content.contains("resumo obsoleto")));
        }
    }
}
