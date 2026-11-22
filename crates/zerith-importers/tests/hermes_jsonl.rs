//! Hermes JSONL export import, against small anonymized fixtures.

use zerith_importers::{import_jsonl, import_jsonl_from, ImportError};
use zerith_sessions::{SessionSource, Store};

/// Session-per-line with nested messages, plus one message-per-line record,
/// one junk line and unknown fields.
const EXPORT: &str = r#"{"id":"s1","title":"Conversa A","model":"hermes-3","started_at":1700000000.5,"source":"cli","cwd":"/x","messages":[{"id":1,"role":"user","content":"oi","timestamp":1700000001.0,"message_uid":"u1"},{"id":2,"role":"assistant","content":[{"type":"text","text":"olá"}],"reasoning":"pensando","tool_calls":[{"id":"t"}]},{"id":3,"role":"assistant","content":"apagada","active":0}]}
not json
{"id":"s2","started_at":"1700000100","archived":true,"messages":[]}
{"session_id":"s2","role":"user","content":"solta","extra_field":42}
"#;

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

#[test]
fn sessions_and_messages_import_with_unknowns_in_raw() {
    let s = store();
    let r = import_jsonl_from(EXPORT.as_bytes(), "default", &s).unwrap();
    assert_eq!((r.sessions_seen, r.sessions_imported), (2, 2));
    assert_eq!(r.messages_imported, 3, "inactive row skipped");
    assert_eq!(r.failed.len(), 1);

    let a = s.session("hermes-default-s1").unwrap().unwrap();
    assert_eq!(a.title, "Conversa A");
    assert_eq!(a.source, SessionSource::Hermes);
    assert_eq!(a.created_at, 1_700_000_000_500);
    assert_eq!(a.model.as_deref(), Some("hermes-3"));
    let msgs = s.messages(&a.id, 10, 0).unwrap();
    assert_eq!(msgs[1].content, "olá");
    assert_eq!(msgs[1].reasoning.as_deref(), Some("pensando"));
    assert!(msgs[1].tool_calls.is_some());
    assert_eq!(msgs[0].raw.as_ref().unwrap()["message_uid"], "u1");

    let b = s.session("hermes-default-s2").unwrap().unwrap();
    assert!(b.archived);
    assert_eq!(b.title, "s2");
    assert_eq!(b.created_at, 1_700_000_100_000);
    let m = &s.messages(&b.id, 10, 0).unwrap()[0];
    assert_eq!(m.raw.as_ref().unwrap()["extra_field"], 42);
}

#[test]
fn reimport_is_idempotent_and_resumes_new_messages() {
    let s = store();
    import_jsonl_from(EXPORT.as_bytes(), "default", &s).unwrap();
    let again = import_jsonl_from(EXPORT.as_bytes(), "default", &s).unwrap();
    assert_eq!((again.sessions_imported, again.sessions_skipped), (0, 2));
    assert_eq!(again.messages_imported, 0);

    let more = format!(
        "{EXPORT}{}\n",
        r#"{"session_id":"s2","role":"assistant","content":"nova"}"#
    );
    let r = import_jsonl_from(more.as_bytes(), "default", &s).unwrap();
    assert_eq!((r.sessions_resumed, r.messages_imported), (1, 1));
    assert_eq!(s.message_count("hermes-default-s2").unwrap(), 2);
    // Another profile is another key.
    let other = import_jsonl_from(EXPORT.as_bytes(), "work", &s).unwrap();
    assert_eq!(other.sessions_imported, 2);
}

#[test]
fn a_missing_file_is_not_found() {
    let s = store();
    let err = import_jsonl(std::path::Path::new("nao-existe.jsonl"), "default", &s).unwrap_err();
    assert!(matches!(err, ImportError::NotFound(_)));
}

#[test]
fn resume_ignores_zerith_replies_and_bad_utf8_lines_are_reported() {
    use zerith_sessions::{NewMessage, Role};
    let s = store();
    import_jsonl_from(EXPORT.as_bytes(), "default", &s).unwrap();
    // The user continued the imported chat inside Zerith.
    s.append(
        "hermes-default-s2",
        &NewMessage::new(Role::Assistant, "local reply".to_string()),
    )
    .unwrap();

    let mut more = EXPORT.as_bytes().to_vec();
    more.extend_from_slice(b"\xff\xfe broken\n");
    more.extend_from_slice(br#"{"session_id":"s2","role":"assistant","content":"nova"}"#);
    more.push(b'\n');
    let r = import_jsonl_from(more.as_slice(), "default", &s).unwrap();
    assert_eq!(r.messages_imported, 1, "new Hermes message is not skipped");
    assert!(
        r.failed.iter().any(|f| f.contains("UTF-8")),
        "{:?}",
        r.failed
    );
    assert_eq!(s.message_count("hermes-default-s2").unwrap(), 3);
}
