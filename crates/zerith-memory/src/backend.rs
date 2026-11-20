//! The `MemoryBackend` contract and its data types.

use serde::{Deserialize, Serialize};

/// Memory is scoped, never global by accident. The scope is an opaque string
/// the caller builds (`user:joao`, `project:zerith`); the backend never guesses
/// which scope a write belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope(pub String);

impl Scope {
    pub fn user(id: &str) -> Self {
        Scope(format!("user:{id}"))
    }

    pub fn project(id: &str) -> Self {
        Scope(format!("project:{id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The scope a conversation in `workspace` uses: facts about the work go
    /// to the project, so one client's routers never show up in another's
    /// chat; preferences and instructions stay with the user, everywhere.
    /// Reads see both, and the facts saved before this split (all `user:`).
    pub fn workspace(workspace: &std::path::Path) -> Self {
        let real = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let mut id = real.to_string_lossy().replace('\\', "/");
        if let Some(rest) = id.strip_prefix("//?/") {
            id = rest.to_string();
        }
        if cfg!(windows) {
            id = id.to_lowercase();
        }
        Scope(format!("project:{id}{SEP}{}", default_scope().0))
    }

    /// Every scope: the memory screen, which manages all of it.
    pub fn all() -> Self {
        Scope("*".into())
    }

    /// Where a new fact of `kind` is written.
    pub fn write_for(&self, kind: FactKind) -> &str {
        let first = self.0.split(SEP).next().unwrap_or("");
        let last = self.0.rsplit(SEP).next().unwrap_or(first);
        match kind {
            FactKind::Preference | FactKind::Instruction => last,
            _ => first,
        }
    }
}

/// Joins the scopes of a composite read set (no path holds it on Windows).
// ponytail: a composite scope is one string the SQL matches with instr();
// a table of scopes if memory ever has more than a few thousand facts.
pub const SEP: char = '|';

/// The scope CLI and UI both use: `user:$ZERITH_USER`, else `user:local`.
/// One rule, so a fact stored in one front end is visible in the other.
pub fn default_scope() -> Scope {
    let id = std::env::var("ZERITH_USER")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string());
    Scope::user(&id)
}

/// What kind of memory this is. Kept separate from confidence so an inference
/// can never be displayed as a confirmed fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactKind {
    Preference,
    Fact,
    Decision,
    Project,
    Instruction,
    Entity,
}

impl FactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FactKind::Preference => "preference",
            FactKind::Fact => "fact",
            FactKind::Decision => "decision",
            FactKind::Project => "project",
            FactKind::Instruction => "instruction",
            FactKind::Entity => "entity",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "preference" => FactKind::Preference,
            "decision" => FactKind::Decision,
            "project" => FactKind::Project,
            "instruction" => FactKind::Instruction,
            "entity" => FactKind::Entity,
            _ => FactKind::Fact,
        }
    }
}

/// `confirmed` is something the user said; `inferred` is something the system
/// concluded. They are stored with the same shape and never merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Confirmed,
    Inferred,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Confirmed => "confirmed",
            Confidence::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub id: i64,
    pub scope: String,
    pub kind: FactKind,
    pub text: String,
    /// Where this came from: a session id, `manual`, an importer.
    pub source: Option<String>,
    pub session_id: Option<String>,
    pub confidence: Confidence,
    pub created_at: i64,
    pub updated_at: i64,
    /// Set when a later fact replaced this one. The row stays for history.
    pub superseded_by: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    pub fact: Fact,
    /// FTS5 bm25 score; lower is a better match.
    pub score: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] zerith_sessions::store::StoreError),
    #[error("fact not found: {0}")]
    NotFound(i64),
    #[error("empty memory text")]
    Empty,
    #[error("parece conter senha ou chave: guarde no cofre do sistema, não na memória")]
    Secret,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("linha {line}: {message}")]
    Import { line: usize, message: String },
}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// What a backend must do. `recall` returns ranked rows and never the whole
/// store: the caller applies the token budget.
pub trait MemoryBackend {
    fn remember(
        &self,
        scope: &Scope,
        kind: FactKind,
        text: &str,
        confidence: Confidence,
        source: Option<&str>,
    ) -> Result<i64>;

    fn recall(&self, scope: &Scope, query: &str, limit: usize) -> Result<Vec<RecallHit>>;

    fn list(&self, scope: &Scope, limit: usize) -> Result<Vec<Fact>>;

    fn get(&self, id: i64) -> Result<Fact>;

    fn update(&self, id: i64, text: &str) -> Result<()>;

    /// Permanent deletion: the text is wiped and the row becomes a tombstone so
    /// the dedupe key cannot resurrect it.
    fn forget(&self, id: i64) -> Result<()>;

    /// Marks a contradicting fact as replaced by `by`, keeping the history.
    fn supersede(&self, old_id: i64, by: i64) -> Result<()>;

    /// Everything in scope, for export. Includes superseded rows, excludes
    /// deleted ones.
    fn export(&self, scope: &Scope) -> Result<Vec<Fact>>;

    /// Whether automatic extraction may write from this session (not paused,
    /// not excluded). Backends without the switches always allow it.
    fn is_extraction_allowed(&self, _session_id: &str) -> Result<bool> {
        Ok(true)
    }

    /// Recall across several scopes (e.g. user + project), merged by score.
    /// bm25 scores from one FTS index are comparable across scopes.
    fn recall_multi(&self, scopes: &[Scope], query: &str, limit: usize) -> Result<Vec<RecallHit>> {
        let mut hits = Vec::new();
        for s in scopes {
            hits.extend(self.recall(s, query, limit)?);
        }
        hits.sort_by(|a, b| a.score.total_cmp(&b.score));
        hits.truncate(limit);
        Ok(hits)
    }
}
