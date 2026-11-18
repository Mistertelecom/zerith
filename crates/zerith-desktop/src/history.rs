//! History: every conversation, searchable, archivable, exportable — and the
//! one-way door from Hermes.
//!
//! The importer is read-only by construction: the source database is opened
//! `SQLITE_OPEN_READ_ONLY`, rows are matched on `(source, source_id, profile)`,
//! and an interrupted run resumes where it stopped. Nothing here writes to
//! Hermes.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;
use zerith_importers::{ImportReport, SessionPreview, SourcePreview};
use zerith_sessions::Store;

use crate::state::AppState;

/// One session in the history list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub source: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
}

/// One Hermes conversation, before anything is copied.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSession {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub archived: bool,
}

impl ImportSession {
    fn of(s: &SessionPreview) -> Self {
        Self {
            id: s.id.clone(),
            title: s.title.clone(),
            model: s.model.clone(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            message_count: s.message_count,
            archived: s.archived,
        }
    }
}

/// A source database and what it holds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub path: String,
    pub profile: String,
    pub sessions: Vec<ImportSession>,
    pub total_messages: usize,
    pub min_date: Option<i64>,
    pub max_date: Option<i64>,
}

impl SourceView {
    fn of(s: SourcePreview) -> Self {
        Self {
            path: s.layout.path.display().to_string(),
            profile: s.layout.profile.clone(),
            sessions: s.sessions.iter().map(ImportSession::of).collect(),
            total_messages: s.total_messages,
            min_date: s.min_date,
            max_date: s.max_date,
        }
    }
}

/// What a run actually did, in numbers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportView {
    pub sessions_seen: usize,
    pub sessions_imported: usize,
    pub sessions_skipped: usize,
    pub sessions_resumed: usize,
    pub messages_imported: usize,
    pub messages_skipped: usize,
    pub failed: Vec<String>,
}

impl ReportView {
    fn of(r: ImportReport) -> Self {
        Self {
            sessions_seen: r.sessions_seen,
            sessions_imported: r.sessions_imported,
            sessions_skipped: r.sessions_skipped,
            sessions_resumed: r.sessions_resumed,
            messages_imported: r.messages_imported,
            messages_skipped: r.messages_skipped,
            failed: r.failed,
        }
    }
}

/// Every session, newest first. Archived ones are filtered here so the window
/// can switch between the two lists without another round trip.
#[tauri::command]
pub async fn sessions_history(
    state: State<'_, AppState>,
    include_archived: bool,
) -> Result<Vec<SessionRow>, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let store = Store::open(&db).map_err(|e| e.to_string())?;
        let list = store.list_sessions(5000).map_err(|e| e.to_string())?;
        Ok::<_, String>(
            list.into_iter()
                .filter(|s| include_archived || !s.archived)
                .map(|s| SessionRow {
                    id: s.id,
                    title: s.title,
                    source: s.source.as_str().to_string(),
                    model: s.model,
                    provider: s.provider,
                    created_at: s.created_at,
                    updated_at: s.updated_at,
                    archived: s.archived,
                })
                .collect(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Looks for Hermes databases in the places Hermes actually puts them.
#[tauri::command]
pub async fn hermes_detect() -> Result<Vec<SourceView>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        Ok::<_, String>(
            zerith_importers::detect()
                .into_iter()
                .filter(|l| l.sessions_table && l.messages_table)
                .filter_map(|l| zerith_importers::preview(&l.path).ok())
                .map(SourceView::of)
                .collect(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Previews a database the person picked, without writing to it.
#[tauri::command]
pub async fn hermes_preview(path: String) -> Result<SourceView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        if !p.is_file() {
            return Err(format!("arquivo não encontrado: {path}"));
        }
        zerith_importers::preview(&p)
            .map(SourceView::of)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copies the chosen sessions. Idempotent: a second run adds nothing.
#[tauri::command]
pub async fn hermes_import(
    state: State<'_, AppState>,
    path: String,
    include_archived: bool,
    only_ids: Option<Vec<String>>,
) -> Result<ReportView, String> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        if !p.is_file() {
            return Err(format!("arquivo não encontrado: {path}"));
        }
        let store = Store::open(&db).map_err(|e| e.to_string())?;
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
        {
            return zerith_importers::import_jsonl(&p, "default", &store)
                .map(ReportView::of)
                .map_err(|e| e.to_string());
        }
        zerith_importers::import_selected(
            &p,
            "default",
            &store,
            include_archived,
            only_ids.as_deref(),
        )
        .map(ReportView::of)
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
