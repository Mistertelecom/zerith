//! Subagents each open their own connection to one database file; writes from
//! several of them at once must wait on the busy timeout, not fail.

use serde_json::json;
use zerith_sessions::{NewMessage, Role, SessionSource, Store};

#[test]
fn parallel_connections_writing_never_hit_database_is_locked() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("s.db");
    let first = Store::open(&db).unwrap();
    let parent = first.create_session("p", SessionSource::Native).unwrap().id;
    let errs = std::sync::Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for k in 0..6 {
            let (db, parent, errs) = (&db, &parent, &errs);
            s.spawn(move || {
                for round in 0..40 {
                    let note = |what: &str, r: Result<(), String>| {
                        if let Err(e) = r {
                            errs.lock().unwrap().push(format!("{what}: {e}"));
                        }
                    };
                    let st = match Store::open(db) {
                        Ok(s) => s,
                        Err(e) => {
                            errs.lock().unwrap().push(format!("open: {e}"));
                            continue;
                        }
                    };
                    let sid = match st.create_subagent_session(parent, "t", Some("m")) {
                        Ok(s) => s.id,
                        Err(e) => {
                            errs.lock().unwrap().push(format!("create: {e}"));
                            continue;
                        }
                    };
                    note(
                        "session",
                        st.session(&sid).map(|_| ()).map_err(|e| e.to_string()),
                    );
                    note(
                        "event",
                        st.append_event(&sid, Some("t"), "turn/start", &json!({}))
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                    );
                    note(
                        "append",
                        st.append(&sid, &NewMessage::new(Role::User, "x"))
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                    );
                    note(
                        "recent",
                        st.recent_messages(&sid, 10)
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                    );
                    note(
                        "event2",
                        st.append_event(&sid, Some("t"), "step/start", &json!({}))
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                    );
                    note(
                        "append2",
                        st.append(&sid, &NewMessage::new(Role::Assistant, "y"))
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                    );
                    let _ = (k, round);
                }
            });
        }
    });
    let e = errs.lock().unwrap();
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for x in e.iter() {
        *kinds.entry(x.clone()).or_default() += 1;
    }
    assert!(e.is_empty(), "{kinds:#?}");
}
