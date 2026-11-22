//! Zerith global visual workflow engine, cron scheduler, and notification channels.

pub mod channels;
pub mod model;
pub mod runner;
pub mod schedule;
pub mod store;

pub use model::{
    ChannelConfig, ChannelKind, NodeEvent, NodeKind, NodeRunLog, Workflow, WorkflowEdge,
    WorkflowNode, WorkflowRun,
};
pub use runner::{execute_workflow, test_single_node, AiExecutor, NoopAiExecutor, RunOptions};
pub use schedule::{CronSchedule, LocalDateTime};
pub use store::{random_token, starter_templates, Store};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("sqlite: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("canal: {0}")]
    Channel(String),
    #[error("execução: {0}")]
    Exec(String),
    #[error("ia: {0}")]
    Ai(String),
    #[error("erro de lock no banco de workflows")]
    Lock,
}

pub type Result<T> = std::result::Result<T, WorkflowError>;

pub fn unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// OS local UTC offset (minutes east) at `unix_secs`, DST-aware. Used when a
/// trigger stores no explicit `tz_offset_minutes`.
pub fn local_offset_minutes(unix_secs: i64) -> i32 {
    use chrono::{Local, Offset, TimeZone};
    Local
        .timestamp_opt(unix_secs, 0)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc() / 60)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
