//! Local, durable memory.
//!
//! The backend is a trait so SQLite FTS5 is not the only possible engine.
//! Nothing here needs a network, an account or a model: store and recall are
//! lexical by default, and a vector index is an optional layer added later.
//! Facts carry `confidence`, so an inference is never shown as a confirmed fact.

pub mod backend;
pub mod extract;
pub mod sqlite;

pub use backend::{
    default_scope, Confidence, Fact, FactKind, MemoryBackend, MemoryError, RecallHit, Scope,
};
pub use extract::{extract, extract_allowed, Extracted};
pub use sqlite::SqliteMemory;
