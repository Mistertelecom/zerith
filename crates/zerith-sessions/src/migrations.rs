//! Versioned schema. One file, one ordered list.
//!
//! Memory tables live here too: they share the same database file, so they
//! share the same migration order. `zerith-memory` reads and writes them
//! through the connection this module hands out; it owns no schema of its own.

use rusqlite::Connection;

pub const CURRENT_VERSION: i32 = 3;

/// Every migration, in order. Index 0 upgrades 0 -> 1.
const MIGRATIONS: &[&str] = &[V1, V2, V3];

const V3: &str = r#"
-- Full-text search over message content. External content: the text lives
-- once, in `messages`; the index holds only tokens.
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2"
);

CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
END;

CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;

-- Backfill rows written before this version. `rebuild` re-reads the content
-- table, so running it twice yields the same index.
INSERT INTO messages_fts(messages_fts) VALUES('rebuild');

-- Small durable switches (memory pause, per-session exclusion, import
-- cursors). Key/value on purpose: each is one row, none needs a schema.
CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
"#;

const V1: &str = r#"
CREATE TABLE sessions (
    id             TEXT PRIMARY KEY,
    title          TEXT NOT NULL DEFAULT '',
    source         TEXT NOT NULL DEFAULT 'native',
    source_id      TEXT,
    source_profile TEXT,
    model          TEXT,
    provider       TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    archived       INTEGER NOT NULL DEFAULT 0
);

-- Idempotent import key: the same source row can be imported twice without
-- producing a second session.
CREATE UNIQUE INDEX sessions_source_key
    ON sessions(source, source_id, source_profile)
    WHERE source_id IS NOT NULL;

CREATE TABLE messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role        TEXT NOT NULL,
    content     TEXT NOT NULL DEFAULT '',
    tool_name   TEXT,
    tool_calls  TEXT,
    reasoning   TEXT,
    token_count INTEGER,
    created_at  INTEGER NOT NULL,
    raw         TEXT
);

CREATE INDEX messages_by_session ON messages(session_id, id);

CREATE TABLE memory_facts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    scope       TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'fact',
    text        TEXT NOT NULL,
    source      TEXT,
    session_id  TEXT,
    confidence  TEXT NOT NULL DEFAULT 'confirmed',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    superseded_by INTEGER REFERENCES memory_facts(id),
    deleted     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX memory_facts_by_scope ON memory_facts(scope, deleted);
CREATE UNIQUE INDEX memory_facts_dedupe ON memory_facts(scope, text) WHERE deleted = 0;

-- Lexical recall. `unicode61 remove_diacritics 2` keeps Portuguese searches
-- working without an accent for every word.
CREATE VIRTUAL TABLE memory_fts USING fts5(
    text,
    content='memory_facts',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2"
);

CREATE TRIGGER memory_facts_ai AFTER INSERT ON memory_facts BEGIN
    INSERT INTO memory_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER memory_facts_ad AFTER DELETE ON memory_facts BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, text) VALUES('delete', old.id, old.text);
END;

CREATE TRIGGER memory_facts_au AFTER UPDATE ON memory_facts BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, text) VALUES('delete', old.id, old.text);
    INSERT INTO memory_fts(rowid, text) VALUES (new.id, new.text);
END;
"#;

const V2: &str = r#"
-- Subagent work, as durable rows: a plan that only lives in memory is a plan
-- that disappears with the crash it was meant to survive.
CREATE TABLE agent_tasks (
    id            TEXT PRIMARY KEY,
    plan_id       TEXT NOT NULL,
    title         TEXT NOT NULL,
    prompt        TEXT NOT NULL,
    agent         TEXT NOT NULL DEFAULT 'default',
    depends_on    TEXT,
    session_id    TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    state         TEXT NOT NULL DEFAULT 'pending',
    result        TEXT,
    error         TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX agent_tasks_by_plan ON agent_tasks(plan_id, created_at);
"#;

/// Runs every pending migration inside one transaction.
pub fn run(conn: &mut Connection) -> rusqlite::Result<i32> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at INTEGER NOT NULL
         );",
    )?;
    let mut applied: i32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    while (applied as usize) < MIGRATIONS.len() {
        let sql = MIGRATIONS[applied as usize];
        let next = applied + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![next, now_ms()],
        )?;
        tx.commit()?;
        applied = next;
    }
    Ok(applied)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
