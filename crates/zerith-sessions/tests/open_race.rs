//! Opening the same database from many threads at once (the window does it:
//! the list, the pause switch, the search and the counters each open their
//! own connection) must wait for the lock instead of failing with
//! "database is locked".

use std::sync::{Arc, Barrier};

use zerith_sessions::Store;

#[test]
fn many_threads_opening_a_fresh_database_all_succeed() {
    let dir = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let db = dir.path().join("zerith.db");
    let n = 12;
    let gate = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let (db, gate) = (db.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                Store::open(&db).map(|_| ()).map_err(|e| e.to_string())
            })
        })
        .collect();
    let errors: Vec<String> = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap().err())
        .collect();
    assert!(errors.is_empty(), "opens failed: {errors:?}");
}
