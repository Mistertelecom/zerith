use serde_json::json;
use zerith_sessions::{SessionSource, Store};

fn setup() -> (Store, String) {
    let s = Store::open_in_memory().unwrap();
    let id = s.create_session("t", SessionSource::Native).unwrap().id;
    (s, id)
}

#[test]
fn events_get_gap_free_seq_and_page_by_after_seq() {
    let (s, id) = setup();
    for i in 0..5 {
        let seq = s
            .append_event(&id, Some("t1"), "step/start", &json!({"n": i}))
            .unwrap();
        assert_eq!(seq, i + 1);
    }
    let all = s.events(&id, 0, 100).unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[2].payload["n"], 2);
    let page = s.events(&id, 2, 2).unwrap();
    assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), [3, 4]);
    assert!(s.schema_version().unwrap() >= 4);
}

#[test]
fn a_turn_without_turn_end_is_detectable_as_a_crash() {
    let (s, id) = setup();
    // Turn a finished; turn b died after its first tool call; turn c was
    // cancelled by a hook.
    s.append_event(&id, Some("a"), "turn/start", &json!({}))
        .unwrap();
    s.append_event(&id, Some("a"), "turn/end", &json!({"reason": "done"}))
        .unwrap();
    s.append_event(&id, Some("b"), "turn/start", &json!({}))
        .unwrap();
    s.append_event(&id, Some("b"), "tool/call", &json!({"id": "c1"}))
        .unwrap();
    s.append_event(&id, Some("c"), "turn/start", &json!({}))
        .unwrap();
    s.append_event(
        &id,
        Some("c"),
        "turn/end",
        &json!({"reason": "cancelled", "cause": "hook"}),
    )
    .unwrap();

    let turns = s.turn_summary(&id).unwrap();
    assert_eq!(turns.len(), 3);
    assert!(!turns[0].is_open());
    assert_eq!(turns[0].ended.as_ref().unwrap().reason, "done");
    assert!(turns[1].is_open(), "turn/start without turn/end");
    assert_eq!(turns[1].turn_id, "b");
    let c = turns[2].ended.as_ref().unwrap();
    assert_eq!(
        (c.reason.as_str(), c.cause.as_deref()),
        ("cancelled", Some("hook"))
    );
}

#[test]
fn an_unfinished_compaction_is_detectable() {
    let (s, id) = setup();
    assert!(!s.has_open_compaction(&id).unwrap());
    s.append_event(&id, None, "compaction/start", &json!({}))
        .unwrap();
    assert!(s.has_open_compaction(&id).unwrap());
    s.append_event(&id, None, "compaction/end", &json!({"ok": true}))
        .unwrap();
    assert!(!s.has_open_compaction(&id).unwrap());
}

#[test]
fn deleting_a_session_removes_its_events() {
    let (s, id) = setup();
    s.append_event(&id, None, "turn/start", &json!({})).unwrap();
    s.conn()
        .execute("DELETE FROM sessions WHERE id = ?1", [&id])
        .unwrap();
    assert!(s.events(&id, 0, 10).unwrap().is_empty());
}
