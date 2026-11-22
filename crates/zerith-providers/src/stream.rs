//! Server-sent events: line reading, and the OpenAI chunk format.
//!
//! Kept separate from the client so it can be tested without a socket: the
//! shape of OpenAI's stream is the one thing every compatible provider claims
//! to share, and where they differ quietly is exactly here.

use crate::{Cancel, ProviderError, Result, StreamEvent, ToolCall, Usage};
use serde_json::{json, Value};
use std::io::BufRead;

/// Parses one `data:` payload. Returns `None` for a heartbeat or a chunk that
/// carries no text this harness shows. Tool-call fragments need state across
/// chunks and are handled by [`OpenAiStream`] instead.
pub fn parse_data_line(line: &str) -> Option<StreamEvent> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let v: Value = serde_json::from_str(data).ok()?;
    text_events(&v).into_iter().flatten().next()
}

fn text_events(v: &Value) -> [Option<StreamEvent>; 2] {
    let Some(delta) = v.pointer("/choices/0/delta") else {
        return [None, None];
    };
    let nonempty = |k: &str| {
        delta
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    [
        nonempty("reasoning_content")
            .or_else(|| nonempty("reasoning"))
            .map(StreamEvent::Reasoning),
        nonempty("content").map(StreamEvent::Delta),
    ]
}

/// Calls `f` with every `data:` payload until it returns `true` or the body
/// ends. Cancellation is checked both between lines and every 80 ms while
/// waiting for the next line so silent reasoning phases abort immediately.
pub(crate) fn for_each_data(
    resp: ureq::Response,
    cancel: &Cancel,
    f: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    for_each_data_reader(resp.into_reader(), cancel, f)
}

pub(crate) fn for_each_data_reader(
    reader: impl std::io::Read + Send + 'static,
    cancel: &Cancel,
    mut f: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        let buf = std::io::BufReader::new(reader);
        for line in buf.lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(80)) {
            Ok(res) => res.map_err(|e| ProviderError::Transport(e.to_string()))?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if f(data)? {
            break;
        }
    }
    if cancel.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    Ok(())
}

/// Accumulates one OpenAI-compatible stream: text is forwarded as it comes,
/// tool calls are assembled from `delta.tool_calls[index]` fragments and
/// usage is kept for the final `Done`.
#[derive(Default)]
pub(crate) struct OpenAiStream {
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    chunks: usize,
    finish_reason: Option<String>,
    done: bool,
}

/// The message a stream that ended before its completion event fails with.
/// Id for a tool call the server sent without one. Unique per process, not
/// just per step: `call_{i}` alone repeats every step, so history would hold
/// duplicate ids and per-call spill files would overwrite each other. The
/// process id keeps ids from a previous run (stored in sessions) distinct too.
pub(crate) fn fallback_call_id(i: usize) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("call_{i}_{}_{n}", std::process::id())
}

pub(crate) const CUT: &str = "a resposta foi cortada antes do fim";

/// An error the server sent inside a 200 stream, typed like the HTTP error it
/// stands for, so failover, retry and the headline treat it the same way.
/// `kind` is Anthropic's `error.type`; `code` an OpenAI-style numeric code.
pub(crate) fn stream_error(e: &Value) -> ProviderError {
    let body = json!({ "error": e }).to_string();
    let code = e["code"]
        .as_u64()
        .or_else(|| e["code"].as_str().and_then(|s| s.parse().ok()))
        .or_else(|| e["status"].as_u64())
        .filter(|c| (400..600).contains(c));
    let kind = e["type"]
        .as_str()
        .or_else(|| e["code"].as_str())
        .or_else(|| e["status"].as_str());
    match (kind, code) {
        (_, Some(429))
        | (Some("rate_limit_error" | "rate_limit_exceeded" | "RESOURCE_EXHAUSTED"), _) => {
            ProviderError::RateLimited {
                retry_after_ms: None,
                body,
            }
        }
        (_, Some(status)) => ProviderError::Http {
            status: status as u16,
            body,
        },
        (Some("overloaded_error" | "server_is_overloaded" | "UNAVAILABLE"), _) => {
            ProviderError::Http { status: 529, body }
        }
        (Some("api_error" | "server_error" | "INTERNAL"), _) => {
            ProviderError::Http { status: 500, body }
        }
        _ => ProviderError::Malformed(crate::openai::error_message(e)),
    }
}

impl OpenAiStream {
    pub(crate) fn feed(&mut self, data: &str, on: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        if data == "[DONE]" {
            self.done = true;
            return Ok(true);
        }
        // A garbage line between good ones is skipped; a stream that never
        // produced a good line is reported in `finish`.
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        if let Some(e) = v.get("error") {
            return Err(stream_error(e));
        }
        self.chunks += 1;
        if let Some(r) = v
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.finish_reason = Some(r.to_string());
        }
        for ev in text_events(&v).into_iter().flatten() {
            on(ev);
        }
        if let Some(frags) = v
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            const MAX_TOOL_CALLS: usize = 128;
            for (pos, frag) in frags.iter().enumerate() {
                let i = if let Some(raw_idx) = frag["index"].as_u64() {
                    let Ok(idx) = usize::try_from(raw_idx) else {
                        continue;
                    };
                    idx
                } else {
                    match frag["id"].as_str().filter(|s| !s.is_empty()) {
                        Some(id) => {
                            if let Some(existing) = self.calls.iter().position(|c| c.id == id) {
                                existing
                            } else if self
                                .calls
                                .get(pos)
                                .is_some_and(|c| !c.id.is_empty() && c.id != id)
                            {
                                self.calls.len()
                            } else {
                                pos
                            }
                        }
                        None => pos,
                    }
                };
                if i >= MAX_TOOL_CALLS {
                    continue;
                }
                if self.calls.len() <= i {
                    self.calls.resize_with(i + 1, ToolCall::default);
                }
                let call = &mut self.calls[i];
                // Some proxies repeat id/name on every fragment: set once.
                if let Some(id) = frag["id"].as_str().filter(|s| !s.is_empty()) {
                    if call.id.is_empty() {
                        call.id = id.to_string();
                    }
                }
                if let Some(n) = frag["function"]["name"].as_str() {
                    if call.name.is_empty() {
                        call.name = n.to_string();
                    }
                }
                if let Some(a) = frag["function"]["arguments"].as_str() {
                    call.arguments.push_str(a);
                }
            }
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            if let Some(u) = crate::openai::parse_openai_usage(u) {
                self.usage = Some(u);
            }
        }
        Ok(false)
    }

    pub(crate) fn finish(self, on: &mut impl FnMut(StreamEvent)) -> Result<()> {
        if self.chunks == 0 {
            return Err(ProviderError::Malformed(
                "o stream terminou sem nenhum chunk válido".into(),
            ));
        }
        // Some servers omit `[DONE]`; a finish_reason says the same.
        if !self.done && self.finish_reason.is_none() {
            return Err(ProviderError::Transport(CUT.into()));
        }
        let calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.name.is_empty())
            .map(|(i, mut c)| {
                if c.id.is_empty() {
                    c.id = fallback_call_id(i);
                }
                c
            })
            .collect();
        if !calls.is_empty() {
            on(StreamEvent::ToolCalls(calls));
        }
        on(StreamEvent::Done {
            usage: self.usage,
            stop_reason: self.finish_reason,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_is_extracted_and_an_empty_one_is_not_an_event() {
        let line = r#"data: {"choices":[{"delta":{"content":"oi"}}]}"#;
        assert_eq!(parse_data_line(line), Some(StreamEvent::Delta("oi".into())));

        let empty = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        assert_eq!(parse_data_line(empty), None);
    }

    #[test]
    fn the_sentinel_and_heartbeats_are_not_events() {
        assert_eq!(parse_data_line("data: [DONE]"), None);
        assert_eq!(parse_data_line("data:"), None);
        assert_eq!(parse_data_line(": ping"), None);
    }

    #[test]
    fn a_broken_payload_is_skipped_rather_than_panicking() {
        assert_eq!(parse_data_line("data: {not json"), None);
    }

    #[test]
    fn tool_call_fragments_are_assembled_by_index() {
        let mut s = OpenAiStream::default();
        let mut events = Vec::new();
        let mut on = |e| events.push(e);
        for d in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"glob","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"x\"}"}}]}}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
            "[DONE]",
        ] {
            s.feed(d, &mut on).unwrap();
        }
        s.finish(&mut on).unwrap();
        let StreamEvent::ToolCalls(calls) = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(calls[0].arguments, r#"{"path":"x"}"#);
        assert_eq!(calls[1].name, "glob");
        assert!(
            matches!(&events[1], StreamEvent::Done { usage: Some(u), .. } if u.total_tokens == 5)
        );
    }

    fn replay(lines: &[&str]) -> (Result<()>, Vec<StreamEvent>) {
        let mut s = OpenAiStream::default();
        let mut ev = Vec::new();
        for d in lines {
            if let Err(e) = s.feed(d, &mut |e| ev.push(e)) {
                return (Err(e), ev);
            }
        }
        let r = s.finish(&mut |e| ev.push(e));
        (r, ev)
    }

    #[test]
    fn a_stream_cut_mid_tool_call_is_an_error_not_an_answer() {
        let (r, ev) = replay(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"write_file","arguments":"{\"path\":\"a"}}]}}]}"#,
        ]);
        assert!(
            matches!(r, Err(ProviderError::Transport(ref m)) if m == CUT),
            "{r:?}"
        );
        assert!(ev.is_empty(), "no half call reaches the loop: {ev:?}");
    }

    #[test]
    fn a_finish_reason_without_done_completes_and_is_carried() {
        let (r, ev) = replay(&[
            r#"{"choices":[{"delta":{"content":"oi"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        ]);
        r.unwrap();
        assert!(matches!(
            ev.last(),
            Some(StreamEvent::Done { stop_reason: Some(s), .. }) if s == "length"
        ));
        assert!(crate::is_cut_off("length") && !crate::is_cut_off("stop"));
    }

    #[test]
    fn errors_inside_the_stream_keep_their_http_meaning() {
        let (r, _) = replay(&[r#"{"error":{"code":503,"message":"upstream down"}}"#]);
        assert!(matches!(r, Err(ProviderError::Http { status: 503, .. })));
        let (r, _) = replay(&[r#"{"error":{"code":429,"message":"slow down"}}"#]);
        assert!(matches!(r, Err(ProviderError::RateLimited { .. })));
        let (r, _) = replay(&[r#"{"error":{"message":"what"}}"#]);
        assert!(matches!(r, Err(ProviderError::Malformed(_))));
    }

    #[test]
    fn a_stalled_stream_aborts_immediately_when_cancelled() {
        struct StalledReader {
            emitted_first: bool,
        }
        impl std::io::Read for StalledReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.emitted_first {
                    self.emitted_first = true;
                    let first = b"data: hello\n";
                    buf[..first.len()].copy_from_slice(first);
                    return Ok(first.len());
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
                Ok(0)
            }
        }

        let cancel = Cancel::default();
        let cancel_trigger = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(90));
            cancel_trigger.cancel();
        });

        let started = std::time::Instant::now();
        let mut seen = Vec::new();
        let res = for_each_data_reader(
            StalledReader {
                emitted_first: false,
            },
            &cancel,
            |d| {
                seen.push(d.to_string());
                Ok(false)
            },
        );
        let elapsed = started.elapsed();
        assert!(matches!(res, Err(ProviderError::Cancelled)), "{res:?}");
        assert_eq!(seen, vec!["hello"]);
        assert!(
            elapsed < std::time::Duration::from_millis(800),
            "cancel took too long: {elapsed:?}"
        );
    }

    #[test]
    fn multi_tool_chunks_without_index_match_by_id_or_append_new_slots() {
        let (r, ev) = replay(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_b","function":{"name":"glob","arguments":"{\"pattern\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_b","function":{"arguments":"\"*.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        r.unwrap();
        let StreamEvent::ToolCalls(calls) = &ev[0] else {
            panic!("{ev:?}");
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].name, "glob");
        assert_eq!(calls[1].arguments, r#"{"pattern":"*.rs"}"#);
    }

    #[test]
    fn copresent_reasoning_and_content_are_both_emitted() {
        let (r, ev) = replay(&[
            r#"{"choices":[{"delta":{"reasoning_content":"pensando","content":"olá"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":" mais","content":" mundo"},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        r.unwrap();
        assert_eq!(
            ev,
            vec![
                StreamEvent::Reasoning("pensando".into()),
                StreamEvent::Delta("olá".into()),
                StreamEvent::Reasoning(" mais".into()),
                StreamEvent::Delta(" mundo".into()),
                StreamEvent::Done {
                    usage: None,
                    stop_reason: Some("stop".into()),
                },
            ]
        );
    }

    #[test]
    fn empty_finish_reason_does_not_mark_truncated_stream_finished() {
        let (r, _) =
            replay(&[r#"{"choices":[{"delta":{"content":"parcial"},"finish_reason":""}]}"#]);
        assert!(
            matches!(r, Err(ProviderError::Transport(ref m)) if m == CUT),
            "{r:?}"
        );
    }

    #[test]
    fn fallback_call_ids_never_repeat_across_steps() {
        let a = fallback_call_id(0);
        let b = fallback_call_id(0);
        assert_ne!(a, b);
        assert!(a.starts_with("call_0_"), "{a}");
    }
}
