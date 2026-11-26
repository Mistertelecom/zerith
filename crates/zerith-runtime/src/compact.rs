//! The model's view of the conversation, and how it shrinks.
//!
//! `messages` stays the replay source. A compaction never deletes rows: it
//! appends one user row whose `raw` says `{"summary": {start_id, end_id,
//! supersedes}}`, and replay then hides the rows in `start_id..=end_id` and
//! shows the summary in their place.
//!
//! Shrinking goes in phases, cheapest first:
//! 0. cheap passes (in memory only): identical tool results keep only the
//!    newest copy, and big old results outside the protected tail become
//!    one line ([`History::cheap_passes`]);
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
/// Replaces an older copy of a tool result repeated later (same tool, same
/// arguments, same output).
pub const DUPLICATE: &str = "[saída idêntica repetida mais adiante]";
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
/// Tokens at the end of the history whose tool results the cheap passes never
/// demote (opencode `PRUNE_PROTECT`).
pub const PROTECT_TOKENS: usize = 40_000;
/// A tool result shorter than this is not worth deduplicating: the marker
/// would save next to nothing (Hermes `_PRUNE_MIN_CHARS`).
const DEDUPE_MIN_CHARS: usize = 200;
/// An old tool result longer than this becomes one line.
const DEMOTE_MIN_CHARS: usize = 2_000;

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

    /// The call each tool result answers, as `(tool, arguments)`, by index.
    /// `pair` puts every result right after its assistant message, so the
    /// lookup only searches the latest assistant's calls (call ids may repeat
    /// across a resumed session).
    fn call_of(&self) -> Vec<Option<(&str, &str)>> {
        let mut calls: &[ToolCall] = &[];
        self.items
            .iter()
            .map(|it| match it.msg.role {
                PRole::Assistant => {
                    calls = &it.msg.tool_calls;
                    None
                }
                PRole::Tool => {
                    let id = it.msg.tool_call_id.as_deref()?;
                    calls
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| (c.name.as_str(), c.arguments.as_str()))
                }
                _ => None,
            })
            .collect()
    }

    /// Phase 0, before any summary: deduplicate, then demote big old tool
    /// results outside the newest [`PROTECT_TOKENS`] (at most half of
    /// `budget`, so a small window still has something to demote). Only tool
    /// result bodies change: user and assistant messages and the call/result
    /// pairing stay as they are. Returns `(deduplicated, demoted)`.
    /// Concept after Hermes `agent/context_compressor.py` (dedupe and demote
    /// passes before the summary, MIT) and opencode v1
    /// `session/compaction.ts` prune (protects the last 40k tokens, MIT).
    // ponytail: no opencode-style "only if it frees 20k" floor; these passes
    // run only past the threshold, where any saving helps.
    pub fn cheap_passes(&mut self, budget: usize) -> (usize, usize) {
        let deduped = self.dedupe_tools();
        let demoted = self.demote_old_tools(PROTECT_TOKENS.min(budget / 2));
        (deduped, demoted)
    }

    /// Identical tool results (same tool, arguments and output: a repeated
    /// `show running-config`, the same `read_file`) keep only the newest copy.
    pub fn dedupe_tools(&mut self) -> usize {
        let dups: Vec<usize> = {
            let calls = self.call_of();
            let mut seen = HashSet::new();
            (0..self.items.len())
                .rev()
                .filter(|&i| {
                    let m = &self.items[i].msg;
                    m.role == PRole::Tool
                        && m.content.len() >= DEDUPE_MIN_CHARS
                        && calls[i].is_some_and(|(n, a)| !seen.insert((n, a, m.content.as_str())))
                })
                .collect()
        };
        for &i in &dups {
            self.items[i].msg.content = DUPLICATE.into();
        }
        dups.len()
    }

    /// Tool results before the protected tail (the newest `protect` tokens
    /// and everything from the latest real user message on) longer than
    /// [`DEMOTE_MIN_CHARS`] become one line naming the call, its first line,
    /// and the spill file when there is one.
    pub fn demote_old_tools(&mut self, protect: usize) -> usize {
        let last_user = self
            .items
            .iter()
            .rposition(|i| i.msg.role == PRole::User && !i.msg.synthetic)
            .unwrap_or(self.items.len());
        let mut tail = 0;
        let mut start = self.items.len();
        for (i, it) in self.items.iter().enumerate().rev() {
            tail += cost(&it.msg);
            if tail > protect {
                break;
            }
            start = i;
        }
        let edits: Vec<(usize, String)> = {
            let calls = self.call_of();
            (0..start.min(last_user))
                .filter_map(|i| {
                    let m = &self.items[i].msg;
                    if m.role != PRole::Tool || m.content.len() <= DEMOTE_MIN_CHARS {
                        return None;
                    }
                    let (name, args) = calls[i].unwrap_or(("ferramenta", ""));
                    Some((i, demoted_line(name, args, &m.content)))
                })
                .collect()
        };
        let n = edits.len();
        for (i, line) in edits {
            self.items[i].msg.content = line;
        }
        n
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

/// One line for a demoted tool result: `[saída antiga de <tool> (<args>)
/// removida: <first line>…]`, plus the spill file when the output names one.
/// An external-content result keeps its `[conteúdo externo de ...]` header
/// as first line, so none of its body leaves the wrapper.
fn demoted_line(name: &str, args: &str, content: &str) -> String {
    let clip = |s: &str, n: usize| -> String {
        let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > n {
            format!("{}…", flat.chars().take(n).collect::<String>())
        } else {
            flat
        }
    };
    let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    // spill.rs writes `saída completa: <path> (N linhas)`, RTK `...: <path>; use`.
    let spill = content.split_once("saída completa: ").map(|(_, rest)| {
        let end = [rest.find(" ("), rest.find(';'), rest.find('\n')]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        format!(" completa em {}", &rest[..end])
    });
    format!(
        "[saída antiga de {name} ({}) removida: {}…{}]",
        clip(args, 80),
        clip(first, 120),
        spill.unwrap_or_default()
    )
}

/// Shrinks `msgs` to `budget` tokens without a model call: the cheap passes,
/// then old tool results lose their body, then whole oldest turns go. The
/// leading system prompt and everything from the latest user message on are
/// kept.
pub fn fit_context(msgs: &mut Vec<ChatMessage>, budget: usize) {
    let lead = msgs.iter().take_while(|m| m.role == PRole::System).count();
    let items = msgs.split_off(lead).into_iter().map(Item::plain).collect();
    let mut h = History::new(std::mem::take(msgs), items);
    h.cheap_passes(budget);
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
    compact_keep(store, session_id, summarizer, cancel, trigger, 0)
}

/// [`compact_with`] keeping the newest `keep` tokens out of the summary.
fn compact_keep(
    store: &Store,
    session_id: &str,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
    trigger: &str,
    keep: usize,
) -> Result<Option<CompactReport>> {
    store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.to_string()))?;
    let rows = store.recent_messages(session_id, COMPACT_WINDOW)?;
    let mut hist = History::new(Vec::new(), replay_items(&rows));
    let log = TurnLog::outside_turn(store, session_id);
    summarize_into(
        store, session_id, &mut hist, keep, summarizer, cancel, &log, trigger,
    )
    .map_err(|e| RunError::Provider(ProviderError::Transport(e)))
}

/// What one compaction did, for the `PostCompact` hook.
#[derive(Debug, Default)]
pub(crate) struct CompactDone<'a> {
    /// `auto` (pressure inside a turn or before one) or `manual` (`/compact`).
    pub trigger: &'a str,
    pub tokens_before: usize,
    pub tokens_after: usize,
    /// Repeated tool outputs collapsed, old big ones demoted (cheap passes).
    pub deduped: usize,
    pub demoted: usize,
    /// Old turns dropped whole (the last resort).
    pub dropped: usize,
    pub summary: Option<&'a CompactReport>,
}

/// Fires `PostCompact` (informational: its output changes nothing). Called
/// only when something was compacted.
pub(crate) fn post_compact(
    settings: &zerith_core::Settings,
    log: &TurnLog<'_>,
    session_id: &str,
    cwd: &std::path::Path,
    done: &CompactDone<'_>,
) {
    use zerith_core::hooks::{run_hooks, HookEvent, HookPayload};
    if settings.hooks.post_compact.is_empty() {
        return;
    }
    let mut extra = json!({
        "trigger": done.trigger,
        "tokens_before": done.tokens_before,
        "tokens_after": done.tokens_after,
        "deduped": done.deduped,
        "demoted": done.demoted,
        "dropped": done.dropped,
        "summarized": done.summary.is_some(),
    });
    if let Some(r) = done.summary {
        extra["summary"] = json!({"messages": r.summarized, "start_id": r.start_id,
                                  "end_id": r.end_id, "summary_id": r.summary_id});
    }
    let serde_json::Value::Object(extra) = extra else {
        return;
    };
    let out = run_hooks(
        HookEvent::PostCompact,
        &HookPayload {
            session_id: session_id.to_string(),
            cwd: cwd.to_path_buf(),
            subject: Some(done.trigger.to_string()),
            extra,
            ..Default::default()
        },
        settings,
    );
    for err in &out.errors {
        log.emit("hook/error", json!({"event": "PostCompact", "error": err}));
    }
}

/// Whether a history of `tokens` should be summarized before the next turn:
/// past `pct`% of the window (`context_limit`, else [`DEFAULT_CONTEXT`]).
pub fn needs_compaction_at(tokens: usize, context_limit: Option<u32>, pct: usize) -> bool {
    tokens * 100 > window_of(context_limit) * pct
}

fn window_of(context_limit: Option<u32>) -> usize {
    context_limit
        .filter(|&l| l > 0)
        .map_or(DEFAULT_CONTEXT, |l| l as usize)
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
    let imported_cap =
        if settings.context.imported_compact_tokens > 0 && is_imported(store, session_id) {
            settings.context.imported_compact_tokens
        } else {
            0
        };
    let Some((keep, mut hist)) = compaction_needed(
        store,
        session_id,
        history,
        limit,
        threshold_pct,
        imported_cap,
    )?
    else {
        return Ok(None);
    };

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
    let log = TurnLog::outside_turn(store, session_id);
    let summarizer = ClientSummarizer {
        client,
        model: sum_model,
    };
    let r = summarize_into(
        store,
        session_id,
        &mut hist,
        keep,
        &summarizer,
        cancel,
        &log,
        "auto",
    )
    .map_err(|e| RunError::Provider(ProviderError::Transport(e)))?;
    if let Some(rep) = &r {
        // ponytail: the host path does not know the session's folder; the
        // hook runs in the process's. Pass the workspace if a hook needs it.
        let cwd = std::env::current_dir().unwrap_or_default();
        let done = CompactDone {
            trigger: "auto",
            tokens_before: rep.tokens_before,
            tokens_after: rep.tokens_after,
            summary: Some(rep),
            ..CompactDone::default()
        };
        let log = TurnLog::outside_turn(store, session_id);
        post_compact(settings, &log, session_id, &cwd, &done);
    }
    Ok(r)
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
    auto_compact_capped(
        store,
        session_id,
        history,
        context_limit,
        threshold_pct,
        0,
        summarizer,
        cancel,
    )
}

/// Whether `session_id` came from another harness (Hermes, Claude Code,
/// Codex): its history was written for that one and is replayed as is.
pub fn is_imported(store: &Store, session_id: &str) -> bool {
    use zerith_sessions::SessionSource as S;
    store
        .session(session_id)
        .ok()
        .flatten()
        .is_some_and(|s| matches!(s.source, S::Hermes | S::ClaudeCode | S::Codex))
}

/// [`auto_compact_at`] with a comfort cap for imported conversations: with
/// `imported_cap > 0` the history is summarized past `imported_cap` tokens
/// (not only past `threshold_pct` of a 1M window, which a chat never
/// reaches), and on the first turn it continues in Zerith (no Zerith summary
/// yet) as soon as it passes a quarter of the cap. The newest
/// `imported_cap / 4` tokens stay verbatim.
///
/// Measured on 2026-11-25: a 326-row Hermes chat (YSERVICE) replayed ~25k
/// tokens of history on every step of a MikroTik turn, a 34 KB Hermes
/// compaction note and 155 assistant messages among them, with 75% of 1M
/// never in sight. Hermes itself summarizes at 50% of the window
/// (`compression.threshold`, agent/context_compressor.py) and its gateway
/// "hygiene" pass at 85% or 400 messages (gateway/run_turn.py, MIT); this cap
/// is lower on purpose: every step re-sends the history, so a smaller one is
/// faster and cheaper. The price is one summary call and old detail only in
/// the summary. `[context] imported_compact_tokens = 0` turns it off.
fn compaction_needed(
    store: &Store,
    session_id: &str,
    history: usize,
    context_limit: Option<u32>,
    threshold_pct: usize,
    imported_cap: usize,
) -> Result<Option<(usize, History)>> {
    // ponytail: counts the replayed rows only; the system prompt and tool
    // specs (a few k tokens) ride on the margin.
    let rows = store.history_rows(session_id, history.max(1))?;
    let mut hist = History::new(Vec::new(), replay_items(&rows));
    let window_line = window_of(context_limit) * threshold_pct / 100;
    let first_here = !hist.items.iter().any(|i| i.summary_row.is_some());
    let line = match imported_cap {
        0 => window_line,
        cap if first_here => (cap / 4).min(window_line),
        cap => cap.min(window_line),
    };
    if hist.tokens() <= line {
        return Ok(None);
    }
    // The turn runs the same passes on its own replay (`agent::shrink`, in
    // memory, nothing stored): when they bring the history under the line,
    // no summary is paid for.
    hist.cheap_passes(line);
    if hist.tokens() <= line {
        return Ok(None);
    }
    let keep = if imported_cap > 0 {
        imported_cap / 4
    } else {
        0
    };
    Ok(Some((keep, hist)))
}

#[allow(clippy::too_many_arguments)]
pub fn auto_compact_capped(
    store: &Store,
    session_id: &str,
    history: usize,
    context_limit: Option<u32>,
    threshold_pct: usize,
    imported_cap: usize,
    summarizer: &dyn Summarizer,
    cancel: &Cancel,
) -> Result<Option<CompactReport>> {
    let Some((keep, _)) = compaction_needed(
        store,
        session_id,
        history,
        context_limit,
        threshold_pct,
        imported_cap,
    )?
    else {
        return Ok(None);
    };
    compact_keep(store, session_id, summarizer, cancel, "auto", keep)
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
        || obj.contains_key("dynamic_reminder")
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

    fn call(id: &str, name: &str, args: &str) -> ChatMessage {
        ChatMessage::assistant(
            "",
            vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args.into(),
            }],
        )
    }

    fn items(msgs: Vec<ChatMessage>) -> History {
        History::new(Vec::new(), msgs.into_iter().map(Item::plain).collect())
    }

    /// The same `show running-config` twice: the older copy becomes the
    /// marker, the newer stays; a different command or a short output is
    /// left alone, and nothing but tool bodies changes.
    #[test]
    fn identical_tool_results_keep_only_the_newest_copy() {
        let run = format!("hostname r1\n{}", "interface x\n".repeat(50));
        let args = r#"{"command":"show running-config"}"#;
        let mut h = items(vec![
            ChatMessage::user("veja o r1"),
            call("a", "ssh", args),
            ChatMessage::tool("a", run.clone()),
            call("b", "ssh", r#"{"command":"show version"}"#),
            ChatMessage::tool("b", run.clone()),
            call("c", "ssh", args),
            ChatMessage::tool("c", "ok"),
            call("d", "ssh", args),
            ChatMessage::tool("d", run.clone()),
            ChatMessage::assistant("pronto", vec![]),
        ]);
        let before: Vec<ChatMessage> = h.items.iter().map(|i| i.msg.clone()).collect();
        assert_eq!(h.dedupe_tools(), 1);
        assert_eq!(h.items[2].msg.content, DUPLICATE);
        assert_eq!(h.items[4].msg.content, run, "other arguments: not a copy");
        assert_eq!(h.items[8].msg.content, run, "the newest copy stays");
        for (a, b) in before.iter().zip(&h.items) {
            assert_eq!(a.role, b.msg.role);
            assert_eq!(a.tool_call_id, b.msg.tool_call_id);
            assert_eq!(a.tool_calls, b.msg.tool_calls);
            if a.role != PRole::Tool {
                assert_eq!(a.content, b.msg.content);
            }
        }
    }

    /// Old big results become one line naming the call and the spill file;
    /// the protected tail, the current turn and small results stay whole.
    #[test]
    fn old_big_results_are_demoted_outside_the_protected_tail() {
        let big = |tag: &str| format!("{tag} primeira linha\n{}", "y".repeat(8_000));
        let spilled = format!(
            "{}\n\n[saída completa: C:/spill/s1/a-1.txt (900 linhas), meio omitido; use read_file]",
            big("cabeça")
        );
        let mut h = items(vec![
            ChatMessage::user("primeiro"),
            call("a", "shell", r#"{"command":"cat log"}"#),
            ChatMessage::tool("a", spilled),
            call("b", "read_file", r#"{"path":"x.rs"}"#),
            ChatMessage::tool("b", "curto"),
            ChatMessage::assistant("li", vec![]),
            ChatMessage::user("segundo"),
            call("c", "read_file", r#"{"path":"y.rs"}"#),
            ChatMessage::tool("c", big("meio")),
            ChatMessage::assistant("li", vec![]),
            ChatMessage::user("agora"),
            call("d", "read_file", r#"{"path":"z.rs"}"#),
            ChatMessage::tool("d", big("atual")),
        ]);
        // A tail of ~5k tokens protects the second turn's result too.
        assert_eq!(h.demote_old_tools(5_000), 1);
        let line = &h.items[2].msg.content;
        assert!(
            line.starts_with(
                r#"[saída antiga de shell ({"command":"cat log"}) removida: cabeça primeira linha…"#
            ),
            "{line}"
        );
        assert!(
            line.ends_with(" completa em C:/spill/s1/a-1.txt]"),
            "{line}"
        );
        assert_eq!(h.items[4].msg.content, "curto");
        assert!(h.items[8].msg.content.starts_with("meio"));
        // No protected tokens: only the current turn (from the last real
        // user message on) is kept.
        assert_eq!(h.demote_old_tools(0), 1);
        assert!(h.items[8]
            .msg
            .content
            .starts_with("[saída antiga de read_file"));
        assert!(h.items[12].msg.content.starts_with("atual"));
        assert_eq!(pair(h.items.clone()).len(), h.items.len(), "pairing intact");
    }

    /// Repeated big reads push the history past the line, but the cheap
    /// passes bring it back under: the summarizer is never called.
    #[test]
    fn the_summary_is_skipped_when_the_cheap_passes_are_enough() {
        struct Never;
        impl Summarizer for Never {
            fn summarize(&self, _: &str, _: &Cancel) -> std::result::Result<String, String> {
                panic!("cheap passes were enough: no summary");
            }
        }
        let store = Store::open_in_memory().unwrap();
        let sid = store
            .create_session("barata", SessionSource::Native)
            .unwrap()
            .id;
        let out = "linha de config\n".repeat(1_500); // ~6k tokens
        for i in 0..12 {
            store
                .append(&sid, &NewMessage::new(Role::User, format!("pedido {i}")))
                .unwrap();
            let id = format!("c{i}");
            store
                .append(
                    &sid,
                    &NewMessage {
                        tool_calls: Some(json!([{"id": id, "name": "read_file",
                                                  "arguments": format!(r#"{{"path":"f{i}"}}"#)}])),
                        ..NewMessage::new(Role::Assistant, "")
                    },
                )
                .unwrap();
            store
                .append(
                    &sid,
                    &NewMessage {
                        tool_name: Some("read_file".into()),
                        tool_calls: Some(json!({"tool_call_id": id})),
                        ..NewMessage::new(Role::Tool, out.clone())
                    },
                )
                .unwrap();
        }
        // ~72k tokens against a 64k window: past 75%.
        let r = auto_compact(&store, &sid, 100, Some(64_000), &Never, &Cancel::default());
        assert!(r.unwrap().is_none());
        assert!(!store.has_open_compaction(&sid).unwrap());
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

    /// A Hermes chat shaped like the 2026-11-25 one (YSERVICE: 20 prompts,
    /// a 34 KB compaction note, ~150 tool rounds): on a 1M window the 75%
    /// line never fires; as an imported session it is summarized on the
    /// first turn here, keeping the newest 10k tokens verbatim, and then only
    /// past the 40k cap. A native session of the same shape is left alone.
    #[test]
    fn an_imported_chat_is_summarized_on_its_first_turn_here() {
        struct Any;
        impl Summarizer for Any {
            fn summarize(&self, _: &str, _: &Cancel) -> std::result::Result<String, String> {
                Ok("resumo do Hermes".into())
            }
        }
        let fill = |store: &Store, sid: &str| {
            let note = format!("[CONTEXT COMPACTION] {}", "nota ".repeat(6_900));
            store
                .append(sid, &NewMessage::new(Role::User, note))
                .unwrap();
            let out = "linha de saída do roteador\n".repeat(55); // ~1.5 KB
            for turn in 0..20 {
                store
                    .append(sid, &NewMessage::new(Role::User, format!("pedido {turn}")))
                    .unwrap();
                for k in 0..7 {
                    let id = format!("c{turn}-{k}");
                    store
                        .append(
                            sid,
                            &NewMessage {
                                tool_calls: Some(json!([{"id": id, "name": "terminal",
                                    "arguments": format!(r#"{{"command":"cmd {turn} {k}"}}"#)}])),
                                ..NewMessage::new(Role::Assistant, "verificando")
                            },
                        )
                        .unwrap();
                    store
                        .append(
                            sid,
                            &NewMessage {
                                tool_name: Some("terminal".into()),
                                tool_calls: Some(json!({"tool_call_id": id})),
                                ..NewMessage::new(Role::Tool, format!("{turn}-{k}\n{out}"))
                            },
                        )
                        .unwrap();
                }
                store
                    .append(
                        sid,
                        &NewMessage::new(Role::Assistant, format!("feito {turn}")),
                    )
                    .unwrap();
            }
        };
        let tokens = |store: &Store, sid: &str| {
            History::new(
                Vec::new(),
                replay_items(&store.history_rows(sid, 40).unwrap()),
            )
            .tokens()
        };
        let c = Cancel::default();
        let store = Store::open_in_memory().unwrap();
        let native = store.create_session("n", SessionSource::Native).unwrap().id;
        fill(&store, &native);
        let hermes = store.create_session("h", SessionSource::Hermes).unwrap().id;
        fill(&store, &hermes);
        assert!(!is_imported(&store, &native) && is_imported(&store, &hermes));
        let before = tokens(&store, &hermes);
        assert!(before > 40_000, "{before}");
        let at = |sid: &str, cap| {
            auto_compact_capped(&store, sid, 40, Some(1_000_000), 75, cap, &Any, &c).unwrap()
        };
        assert!(at(&native, 0).is_none(), "75% of 1M is far away");
        let r = at(&hermes, 40_000).expect("first turn here: summarized");
        let after = tokens(&store, &hermes);
        println!("importada: {before} → {after} tokens de histórico");
        assert!(after < 12_000, "{after}");
        assert!(after > 1_000, "the newest rounds stay verbatim: {after}");
        assert_eq!(r.tokens_after, after);
        // Summarized once: the next turn is under the 40k cap.
        assert!(at(&hermes, 40_000).is_none());
        // Off: an imported chat behaves like a native one.
        let other = store
            .create_session("h2", SessionSource::Hermes)
            .unwrap()
            .id;
        fill(&store, &other);
        assert!(at(&other, 0).is_none());
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
            "dynamic_reminder",
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
        assert_eq!(replayed.len(), 9);
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
