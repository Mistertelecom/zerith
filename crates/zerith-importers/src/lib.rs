//! Importing another agent's history. Today: Hermes.
//!
//! Three rules from the spec, and none of them is negotiable:
//!
//! 1. **Read-only.** The source database is opened `SQLITE_OPEN_READ_ONLY`.
//!    Nothing in this crate can write to `~/.hermes`, by construction.
//! 2. **Idempotent.** A row is matched on `(source, source_id, profile)`, so
//!    running an import twice adds nothing the second time.
//! 3. **Resumable.** The import commits session by session; an interrupted run
//!    leaves complete sessions behind, and the next run skips them.
//!
//! Unknown columns are kept verbatim in `raw` rather than dropped: a Hermes
//! upgrade must not lose data this crate has not learned about yet.

pub mod claude_code;
mod hermes;
mod hermes_jsonl;
pub mod hermes_migrate;
pub mod hermes_setup;
pub mod plan;

pub use hermes::{
    detect, import, import_selected, import_with, preview, ImportReport, SessionOptions,
    SessionPreview, SourceLayout, SourcePreview,
};
pub use hermes_jsonl::{import_jsonl, import_jsonl_from};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("source database not found: {0}")]
    NotFound(String),
    #[error("source database has no usable schema: {0}")]
    Schema(String),
    #[error("source: {0}")]
    Source(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ImportError>;
