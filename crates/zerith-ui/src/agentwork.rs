//! View-model of agent work: task checklist, subagents, plan and question
//! cards, permission modes, slash/`@` triggers, the context meter and compact
//! tool rows.
//!
//! Everything here is plain data and pure functions, so the screens only draw
//! it and tests can drive every transition without a window. The runtime
//! events that feed it live in `screens::chat::events`, one adapter per
//! event, each marked `TODO(wire)` until the variant exists in
//! `zerith_runtime::RunEvent`.

use std::path::Path;

use rune_kit::diff::{FileDiff, LineKind};

// ---------------------------------------------------------------- tasks

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    InProgress,
    Completed,
}

impl TaskState {
    /// Tolerant of the spellings agents use (`in_progress`, `done`, ...).
    pub fn parse(s: &str) -> Self {
        match s {
            "in_progress" | "running" | "active" => TaskState::InProgress,
            "completed" | "done" => TaskState::Completed,
            _ => TaskState::Pending,
        }
    }
}

/// One line of the live checklist.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskItem {
    pub text: String,
    /// Present-tense form shown while the task runs; empty falls back to `text`.
    pub active: String,
    pub state: TaskState,
}

impl TaskItem {
    pub fn new(text: impl Into<String>, state: TaskState) -> Self {
        Self {
            text: text.into(),
            active: String::new(),
            state,
        }
    }
    /// What the row says: the active form while running.
    pub fn shown(&self) -> &str {
        if self.state == TaskState::InProgress && !self.active.is_empty() {
            &self.active
        } else {
            &self.text
        }
    }
}

/// `(completed, total)`.
pub fn task_progress(items: &[TaskItem]) -> (usize, usize) {
    let done = items
        .iter()
        .filter(|t| t.state == TaskState::Completed)
        .count();
    (done, items.len())
}

// ------------------------------------------------------------ subagents

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubState {
    Running,
    Done,
    Error,
}

impl SubState {
    pub fn parse(s: &str) -> Self {
        match s {
            "done" | "completed" | "ok" => SubState::Done,
            "error" | "failed" => SubState::Error,
            _ => SubState::Running,
        }
    }
}

/// A subagent as the transcript shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    pub id: String,
    pub name: String,
    pub state: SubState,
    pub summary: String,
}

// ------------------------------------------------------------ questions

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub header: String,
    pub text: String,
    pub options: Vec<String>,
    pub multi: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuestionSet {
    pub id: String,
    pub questions: Vec<Question>,
}

/// A question card being answered: the picks so far, per question.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestionState {
    pub set: QuestionSet,
    pub picks: Vec<Vec<usize>>,
}

impl QuestionState {
    pub fn new(set: QuestionSet) -> Self {
        let picks = vec![Vec::new(); set.questions.len()];
        Self { set, picks }
    }

    /// Radio for a single-choice question, checkbox for a multi-select one.
    pub fn pick(&mut self, q: usize, option: usize) {
        let Some(question) = self.set.questions.get(q) else {
            return;
        };
        if option >= question.options.len() {
            return;
        }
        let multi = question.multi;
        let picks = &mut self.picks[q];
        if multi {
            match picks.iter().position(|p| *p == option) {
                Some(at) => {
                    picks.remove(at);
                }
                None => picks.push(option),
            }
        } else {
            *picks = vec![option];
        }
    }

    pub fn is_picked(&self, q: usize, option: usize) -> bool {
        self.picks.get(q).is_some_and(|p| p.contains(&option))
    }

    /// Submit stays disabled until every question has an answer.
    pub fn ready(&self) -> bool {
        !self.picks.is_empty() && self.picks.iter().all(|p| !p.is_empty())
    }

    /// The chosen option texts, per question, in option order.
    pub fn answers(&self) -> Vec<Vec<String>> {
        self.set
            .questions
            .iter()
            .zip(&self.picks)
            .map(|(q, picks)| {
                let mut sorted = picks.clone();
                sorted.sort_unstable();
                sorted.into_iter().map(|i| q.options[i].clone()).collect()
            })
            .collect()
    }
}

// ---------------------------------------------------------------- modes

/// The permission modes of Claude Code, in the vocabulary of this window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    DontAsk,
    Bypass,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 5] = [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
        PermissionMode::Bypass,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Default => "Padrão",
            PermissionMode::AcceptEdits => "Aceitar edições",
            PermissionMode::Plan => "Plano",
            PermissionMode::DontAsk => "Sem perguntas",
            PermissionMode::Bypass => "Livre",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            PermissionMode::Default => "Pergunta antes de escrever ou executar",
            PermissionMode::AcceptEdits => "Edições no projeto rodam sem perguntar",
            PermissionMode::Plan => "Somente leitura: propõe um plano antes de agir",
            PermissionMode::DontAsk => "Nega o que exigiria pergunta, sem interromper",
            PermissionMode::Bypass => "Executa tudo o que a política não proíbe",
        }
    }

    /// Shift+Tab, as in Claude Code: padrão, aceitar edições, plano, again.
    /// The two modes that skip questions are chosen on purpose, never cycled
    /// into; from one of them the cycle returns to the default.
    pub fn next(self) -> Self {
        match self {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            _ => PermissionMode::Default,
        }
    }

    pub fn to_security(self) -> zerith_security::Mode {
        use zerith_security::Mode;
        match self {
            PermissionMode::Default => Mode::Default,
            PermissionMode::AcceptEdits => Mode::AcceptEdits,
            PermissionMode::Plan => Mode::Plan,
            PermissionMode::DontAsk => Mode::DontAsk,
            PermissionMode::Bypass => Mode::Bypass,
        }
    }

    pub fn from_security(m: zerith_security::Mode) -> Self {
        use zerith_security::Mode;
        match m {
            Mode::Default => PermissionMode::Default,
            Mode::AcceptEdits => PermissionMode::AcceptEdits,
            Mode::Plan => PermissionMode::Plan,
            Mode::DontAsk => PermissionMode::DontAsk,
            Mode::Bypass => PermissionMode::Bypass,
        }
    }
}

// -------------------------------------------------------------- context

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    Ok,
    /// Past 80 %: amber.
    Warn,
    /// Past 95 %: the window is nearly gone.
    Full,
}

/// Tokens in the window against the provider's limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextMeter {
    pub used: u64,
    pub limit: Option<u32>,
}

impl ContextMeter {
    /// `0.0..=1.0`, or `None` when the provider declares no limit.
    pub fn fraction(&self) -> Option<f32> {
        let limit = self.limit.filter(|l| *l > 0)?;
        Some((self.used as f32 / limit as f32).min(1.0))
    }

    pub fn fill(&self) -> Fill {
        match self.fraction() {
            Some(f) if f > 0.95 => Fill::Full,
            Some(f) if f > 0.80 => Fill::Warn,
            _ => Fill::Ok,
        }
    }

    pub fn label(&self) -> String {
        match self.limit {
            Some(l) => format!("{} / {}", compact_count(self.used), compact_count(l as u64)),
            None => format!("{} tokens", compact_count(self.used)),
        }
    }
}

/// `1234` is `1,2k`; `128000` is `128k`; `2_000_000` is `2M`.
pub fn compact_count(n: u64) -> String {
    let fmt = |v: f64, unit: &str| {
        if v >= 10.0 || (v - v.round()).abs() < 0.05 {
            format!("{:.0}{unit}", v)
        } else {
            format!("{:.1}{unit}", v).replace('.', ",")
        }
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => fmt(n as f64 / 1e3, "k"),
        _ => fmt(n as f64 / 1e6, "M"),
    }
}

/// US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input_per_m: f64,
    pub output_per_m: f64,
}

/// What a turn cost, or `None` when the price is unknown: no price, no number.
pub fn turn_cost(usage: (u64, u64), price: Option<Price>) -> Option<f64> {
    let p = price?;
    Some(usage.0 as f64 * p.input_per_m / 1e6 + usage.1 as f64 * p.output_per_m / 1e6)
}

pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        "< US$ 0,01".to_string()
    } else {
        format!("US$ {usd:.2}").replace('.', ",")
    }
}

// ---------------------------------------------------------- composer popups

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub detail: &'static str,
}

pub const SLASH: [SlashCommand; 9] = [
    SlashCommand {
        name: "compact",
        detail: "Resumir a conversa para liberar contexto",
    },
    SlashCommand {
        name: "context",
        detail: "Mostrar o uso da janela de contexto",
    },
    SlashCommand {
        name: "plan",
        detail: "Entrar no modo plano",
    },
    SlashCommand {
        name: "clear",
        detail: "Começar uma conversa nova",
    },
    SlashCommand {
        name: "model",
        detail: "Trocar de modelo",
    },
    SlashCommand {
        name: "resume",
        detail: "Abrir uma conversa anterior",
    },
    SlashCommand {
        name: "agents",
        detail: "Ver e escolher agentes",
    },
    SlashCommand {
        name: "skills",
        detail: "Ver as skills instaladas",
    },
    SlashCommand {
        name: "memory",
        detail: "Ver o que o Zerith lembra",
    },
];

/// Commands whose name starts with `query`, then those that merely contain it.
pub fn filter_slash(query: &str) -> Vec<SlashCommand> {
    let q = query.to_lowercase();
    let mut out: Vec<SlashCommand> = SLASH
        .iter()
        .copied()
        .filter(|c| c.name.starts_with(&q))
        .collect();
    out.extend(
        SLASH
            .iter()
            .copied()
            .filter(|c| !c.name.starts_with(&q) && c.name.contains(&q)),
    );
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Slash,
    File,
}

/// What the caret is inside: a `/command` at the start of the draft, or an
/// `@file` token. `start..end` is the byte span the choice replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Active {
    pub trigger: Trigger,
    pub query: String,
    pub start: usize,
    pub end: usize,
}

pub fn trigger(text: &str, caret: usize) -> Option<Active> {
    let caret = caret.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }
    let before = &text[..caret];
    let end = caret
        + text[caret..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - caret);
    if let Some(rest) = before.strip_prefix('/') {
        if !rest.contains(char::is_whitespace) {
            return Some(Active {
                trigger: Trigger::Slash,
                query: rest.to_string(),
                start: 0,
                end,
            });
        }
    }
    let at = before.rfind('@')?;
    let token = &before[at + 1..];
    let lead_ok = at == 0 || before[..at].ends_with(char::is_whitespace);
    (lead_ok && !token.contains(char::is_whitespace)).then(|| Active {
        trigger: Trigger::File,
        query: token.to_string(),
        start: at,
        end,
    })
}

/// Paths matching `query`: file-name hits first, then path hits, shortest first.
pub fn filter_files<'a>(files: &'a [String], query: &str, max: usize) -> Vec<&'a String> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return files.iter().take(max).collect();
    }
    let name_of = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let mut hits: Vec<(u8, &String)> = files
        .iter()
        .filter_map(|p| {
            let lower = p.to_lowercase();
            if name_of(p).starts_with(&q) {
                Some((0, p))
            } else if name_of(p).contains(&q) {
                Some((1, p))
            } else {
                lower.contains(&q).then_some((2, p))
            }
        })
        .collect();
    hits.sort_by_key(|(rank, p)| (*rank, p.len()));
    hits.into_iter().take(max).map(|(_, p)| p).collect()
}

/// Files under `root`, `/`-separated and relative, skipping hidden folders,
/// build output and dependencies. Bounded in count and depth, so a huge
/// checkout cannot stall the worker that calls it.
pub fn list_files(root: &Path, cap: usize) -> Vec<String> {
    const SKIP: [&str; 4] = ["target", "node_modules", "dist", "build"];
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if depth < 8 && !SKIP.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                    if out.len() >= cap {
                        out.sort();
                        return out;
                    }
                }
            }
        }
    }
    out.sort();
    out
}

// ------------------------------------------------------------- tool rows

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Ok,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Del,
    Ctx,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub text: String,
}

/// The compact diff of an edit or write: its changed lines, capped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiffSummary {
    pub rows: Vec<DiffRow>,
    /// Lines left out by the cap.
    pub more: usize,
    pub adds: usize,
    pub dels: usize,
}

/// Rows shown before "+N linhas".
pub const DIFF_ROWS: usize = 14;

/// One call as the transcript shows it: a one-liner that can open.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRow {
    pub id: String,
    pub name: String,
    /// The one argument worth showing: a path, a pattern or a command.
    pub arg: String,
    pub state: ToolState,
    pub millis: Option<u64>,
    pub started: Option<std::time::Instant>,
    pub preview: String,
    pub diff: Option<DiffSummary>,
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// The short argument of a call: `path`, `pattern`, `command`, else the first
/// string the model wrote, cut to one line.
pub fn tool_arg(args: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return one_line(args, 90);
    };
    let pick = ["path", "pattern", "command", "query", "name"]
        .iter()
        .find_map(|k| json_str(&v, k))
        .or_else(|| {
            v.as_object()?
                .values()
                .find_map(|x| x.as_str().filter(|s| !s.is_empty()))
        });
    one_line(pick.unwrap_or_default(), 90)
}

fn one_line(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or_default().trim();
    if first.chars().count() > max {
        let cut: String = first.chars().take(max).collect();
        format!("{cut}…")
    } else {
        first.to_string()
    }
}

/// The compact diff of `edit_file` / `write_file` arguments; `None` for any
/// other tool or for arguments that are not JSON.
pub fn diff_of_call(name: &str, args: &str) -> Option<DiffSummary> {
    if name != "edit_file" && name != "write_file" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let path = json_str(&v, "path").unwrap_or("arquivo");
    let (old, new) = if name == "edit_file" {
        (json_str(&v, "old_string")?, json_str(&v, "new_string")?)
    } else {
        ("", json_str(&v, "content")?)
    };
    let file = FileDiff::from_texts_with_context(path, old, new, 1);
    let mut s = DiffSummary {
        adds: file.additions(),
        dels: file.deletions(),
        ..DiffSummary::default()
    };
    let mut total = 0usize;
    for line in file.hunks.iter().flat_map(|h| &h.lines) {
        total += 1;
        if s.rows.len() < DIFF_ROWS {
            s.rows.push(DiffRow {
                kind: match line.kind {
                    LineKind::Added => DiffKind::Add,
                    LineKind::Removed => DiffKind::Del,
                    _ => DiffKind::Ctx,
                },
                text: line.text.clone(),
            });
        }
    }
    s.more = total - s.rows.len();
    Some(s)
}

/// `320 ms`, `1,4 s`, `2 min 5 s`.
pub fn format_duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1_000..=59_999 => format!("{:.1} s", ms as f64 / 1e3).replace('.', ","),
        _ => format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1_000),
    }
}

/// An approval is a plan when it came from the plan gate.
pub fn is_plan(tool: &str, title: &str) -> bool {
    tool == "exit_plan_mode" || title == PLAN_TITLE
}

pub const PLAN_TITLE: &str = "Plano proposto";

// ------------------------------------------------------- transcript items

/// What a transcript line carries beyond text.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Tool(ToolRow),
    Subagent(Subagent),
    /// A quiet system note: "Contexto compactado".
    Note(String),
}

/// An event of agent work, in the window's own vocabulary. The adapters in
/// `screens::chat::events` build these from runtime events; the screen only
/// ever sees these.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkEvent {
    Tasks(Vec<TaskItem>),
    Subagent(Subagent),
    Question(QuestionSet),
    Compaction { running: bool },
}
