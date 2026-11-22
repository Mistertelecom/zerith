//! Hermes JSONL import (`hermes sessions export`).
//!
//! One JSON object per line. Two shapes are accepted, because the export has
//! changed between Hermes versions and a partial export is still worth
//! importing:
//!
//! - a session line: `{"id": .., "title": .., "model": .., "started_at": ..,
//!   "messages": [ {role, content, ..}, .. ], ..}`
//! - a message line: `{"session_id": .., "role": .., "content": .., ..}`
//!
//! Fields this importer does not map are kept verbatim in `raw`. Idempotency
//! uses the same `(source, source_id, profile)` key as the SQLite importer.
//! Resume is per message: an already-known session gets only the messages
//! after the ones it already holds (the export is append-only per session).

use std::io::BufRead;
use std::path::Path;

use serde_json::{Map, Value};
use zerith_sessions::{NewMessage, Role, Session, SessionSource, Store};

use crate::hermes::{tally, to_ms, ImportReport};
use crate::{ImportError, Result};

#[derive(Default)]
struct Pending {
    meta: Map<String, Value>,
    messages: Vec<Map<String, Value>>,
}

/// Imports a `hermes sessions export` file.
pub fn import_jsonl(path: &Path, profile: &str, store: &Store) -> Result<ImportReport> {
    let file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ImportError::NotFound(path.display().to_string()),
        _ => e.into(),
    })?;
    import_jsonl_from(std::io::BufReader::new(file), profile, store)
}

/// Same, from any reader.
pub fn import_jsonl_from(r: impl BufRead, profile: &str, store: &Store) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    // Sessions in first-seen order; a Vec keeps it without another dependency.
    let mut order: Vec<(String, Pending)> = Vec::new();
    // Raw bytes per line: one invalid UTF-8 line is reported, not fatal.
    for (i, line) in r.split(b'\n').enumerate() {
        let line = line?;
        let Ok(line) = String::from_utf8(line) else {
            report
                .failed
                .push(format!("linha {}: UTF-8 inválido", i + 1));
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&line) else {
            report
                .failed
                .push(format!("linha {}: JSON inválido", i + 1));
            continue;
        };
        let is_message = obj.contains_key("role") && !obj.contains_key("messages");
        let sid = if is_message {
            str_field(&obj, &["session_id"])
        } else {
            str_field(&obj, &["id", "session_id"])
        };
        let Some(sid) = sid else {
            report
                .failed
                .push(format!("linha {}: sem id de sessão", i + 1));
            continue;
        };
        let idx = match order.iter().position(|(k, _)| *k == sid) {
            Some(i) => i,
            None => {
                order.push((sid.clone(), Pending::default()));
                order.len() - 1
            }
        };
        let p = &mut order[idx].1;
        if is_message {
            p.messages.push(obj);
        } else {
            if let Some(Value::Array(ms)) = obj.remove("messages") {
                p.messages.extend(ms.into_iter().filter_map(|m| match m {
                    Value::Object(o) => Some(o),
                    _ => None,
                }));
            }
            p.meta = obj;
        }
    }

    for (sid, p) in order {
        report.sessions_seen += 1;
        let m = &p.meta;
        let created = time_ms(m, &["started_at", "created_at"]);
        let updated = time_ms(m, &["ended_at", "updated_at"]);
        let session = Session {
            id: format!("hermes-{profile}-{sid}"),
            title: str_field(m, &["title"]).unwrap_or_else(|| sid.clone()),
            source: SessionSource::Hermes,
            source_id: Some(sid.clone()),
            source_profile: Some(profile.to_string()),
            model: str_field(m, &["model"]),
            provider: str_field(m, &["provider", "billing_provider"]),
            created_at: created,
            updated_at: if updated == 0 { created } else { updated },
            archived: m.get("archived").is_some_and(truthy),
        };
        let (local, created_now) = store.upsert_imported(&session)?;
        // Inactive rows are Hermes' tombstones, as in the SQLite importer.
        let live: Vec<_> = p
            .messages
            .into_iter()
            .filter(|m| m.get("active").is_none_or(|v| v.is_null() || truthy(v)))
            .collect();
        let have = imported_count(store, &local)?;
        let mut n = 0;
        for obj in live.into_iter().skip(have) {
            store.append(&local, &to_message(obj))?;
            n += 1;
        }
        report.messages_imported += n;
        tally(&mut report, created_now, n);
    }
    Ok(report)
}

/// Messages this importer wrote: their `raw` is the Hermes object, which
/// always carries `role`. Messages Zerith appended later (replies, hook
/// notes) are not counted, so they cannot push the resume cursor forward.
fn imported_count(store: &Store, session_id: &str) -> Result<usize> {
    let n: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1
             AND raw IS NOT NULL AND json_valid(raw) AND json_extract(raw, '$.role') IS NOT NULL",
            [session_id],
            |r| r.get(0),
        )
        .map_err(zerith_sessions::store::StoreError::from)?;
    Ok(n as usize)
}

fn to_message(obj: Map<String, Value>) -> NewMessage {
    let s = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);
    let content = match obj.get("content") {
        Some(Value::String(t)) => t.clone(),
        // Content blocks (`[{type:text,text:..}]`): keep the readable part.
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let tool_calls = match obj.get("tool_calls") {
        Some(Value::String(t)) => serde_json::from_str(t).ok(),
        Some(v) if !v.is_null() => Some(v.clone()),
        _ => None,
    };
    NewMessage {
        role: s("role")
            .and_then(|r| Role::parse(&r))
            .unwrap_or(Role::User),
        content,
        tool_name: s("tool_name"),
        tool_calls,
        reasoning: s("reasoning").or_else(|| s("reasoning_content")),
        token_count: obj.get("token_count").and_then(Value::as_i64),
        raw: Some(Value::Object(obj)),
    }
}

fn str_field(m: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match m.get(*k)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Epoch seconds (number or numeric string) to milliseconds.
fn time_ms(m: &Map<String, Value>, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|k| match m.get(*k)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .map_or(0, to_ms)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    }
}
