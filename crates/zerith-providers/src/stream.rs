//! Parsing of one server-sent-events data line.
//!
//! Kept separate from the client so it can be tested without a socket: the
//! shape of OpenAI's stream is the one thing every compatible provider claims
//! to share, and where they differ quietly is exactly here.

use crate::StreamEvent;

/// Parses one `data:` payload. Returns `None` for a heartbeat or a chunk that
/// carries nothing this harness shows.
pub fn parse_data_line(line: &str) -> Option<StreamEvent> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    if let Some(text) = v
        .pointer("/choices/0/delta/content")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(StreamEvent::Delta(text.to_string()));
    }
    if let Some(r) = v
        .pointer("/choices/0/delta/reasoning_content")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(StreamEvent::Reasoning(r.to_string()));
    }
    None
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
}
