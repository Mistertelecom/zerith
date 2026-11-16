//! Adapters: one runtime event in, chat state out.
//!
//! `apply` maps each `RunEvent` to chat state; agent-work events (tasks,
//! subagents, questions, compaction) go through `apply_work` as `WorkEvent`s.

use std::time::Instant;

use zerith_runtime::RunEvent;

use super::{request_approval, Approval, Line, Link};
use crate::agentwork::{
    self, DiffSummary, Item, Question, QuestionSet, QuestionState, SubState, Subagent, TaskItem,
    TaskState, ToolRow, ToolState, WorkEvent,
};
use crate::Shell;

/// The last assistant line, if the turn is still writing it.
fn tail_is(lines: &[Line], role: &str) -> bool {
    lines.last().is_some_and(|l| l.role == role)
}

pub fn apply(s: &mut Shell, event: RunEvent) {
    let c = &mut s.chat;
    match event {
        RunEvent::Delta(chunk) => {
            if tail_is(&c.lines, "assistant") {
                if let Some(last) = c.lines.last_mut() {
                    last.content.push_str(&chunk);
                }
            } else {
                c.lines.push(Line::new("assistant", chunk));
            }
        }
        RunEvent::Reasoning(chunk) => {
            if tail_is(&c.lines, "thinking") {
                if let Some(last) = c.lines.last_mut() {
                    last.content.push_str(&chunk);
                }
            } else {
                c.lines.push(Line::new("thinking", chunk));
            }
        }
        RunEvent::Done { .. } => {
            c.busy = false;
            c.cancel = None;
            c.approvals = None;
            c.status.clear();
            c.compacting = false;
            c.lines.retain(|l| {
                l.role != "thinking" && !(l.role == "assistant" && l.content.is_empty())
            });
            // A call still marked running cannot be: the turn is over.
            for line in &mut c.lines {
                if let Some(Item::Tool(t)) = &mut line.item {
                    if t.state == ToolState::Running {
                        t.state = ToolState::Failed;
                    }
                }
            }
            c.link = Link::Up;
            s.refresh_sessions();
        }
        RunEvent::ToolCall { id, name, args } => {
            // The text the model wrote before the call is complete: drop an
            // empty placeholder so the row sits directly under it.
            if tail_is(&c.lines, "assistant")
                && c.lines.last().is_some_and(|l| l.content.is_empty())
            {
                c.lines.pop();
            }
            c.lines.push(Line::item(Item::Tool(ToolRow {
                id,
                arg: agentwork::tool_arg(&args),
                diff: agentwork::diff_of_call(&name, &args),
                name,
                state: ToolState::Running,
                millis: None,
                started: Some(Instant::now()),
                preview: String::new(),
            })));
        }
        RunEvent::ToolResult {
            id,
            name,
            ok,
            output_preview,
        } => {
            let state = if ok { ToolState::Ok } else { ToolState::Failed };
            let row = c.lines.iter_mut().rev().find_map(|l| match &mut l.item {
                Some(Item::Tool(t)) if t.id == id => Some(t),
                _ => None,
            });
            match row {
                Some(t) => {
                    t.state = state;
                    t.millis = t.started.map(|at| at.elapsed().as_millis() as u64);
                    t.preview = output_preview;
                    // A refused edit shows no diff: nothing changed.
                    if !ok {
                        t.diff = None::<DiffSummary>;
                    }
                }
                // A result with no call row (a resumed turn): show it alone.
                None => c.lines.push(Line::item(Item::Tool(ToolRow {
                    id,
                    name,
                    arg: String::new(),
                    state,
                    millis: None,
                    started: None,
                    preview: output_preview,
                    diff: None,
                }))),
            }
            // The next text delta starts a fresh answer below the tool rows.
            c.lines.push(Line::new("assistant", ""));
        }
        RunEvent::ApprovalNeeded { id, tool, summary } => {
            // The plan gate is an approval whose tool is `exit_plan_mode` and
            // whose summary is the plan text: it gets the plan card.
            let title = if agentwork::is_plan(&tool, "") {
                agentwork::PLAN_TITLE.to_string()
            } else {
                format!("{tool} quer executar")
            };
            request_approval(
                s,
                Approval {
                    id: id.to_string(),
                    title,
                    tool,
                    summary,
                },
            );
        }
        RunEvent::Usage { input, output } => c.usage = Some((input, output)),
        RunEvent::Tasks { items } => {
            let items = items
                .into_iter()
                .filter(|t| t.status != zerith_sessions::TodoStatus::Deleted)
                .map(|t| {
                    TaskItem::new(
                        t.subject,
                        match t.status {
                            zerith_sessions::TodoStatus::InProgress => TaskState::InProgress,
                            zerith_sessions::TodoStatus::Completed => TaskState::Completed,
                            _ => TaskState::Pending,
                        },
                    )
                })
                .collect();
            apply_work(s, WorkEvent::Tasks(items));
        }
        RunEvent::Subagent {
            id,
            name,
            state,
            summary,
        } => {
            let state = match state.as_str() {
                "running" => SubState::Running,
                "done" => SubState::Done,
                _ => SubState::Error,
            };
            apply_work(
                s,
                WorkEvent::Subagent(Subagent {
                    id,
                    name,
                    state,
                    summary,
                }),
            );
        }
        RunEvent::Question { id, questions } => {
            let questions = questions
                .into_iter()
                .map(|q| Question {
                    header: q.header,
                    text: q.question,
                    options: q.options.into_iter().map(|o| o.label).collect(),
                    multi: q.multi_select,
                })
                .collect();
            apply_work(
                s,
                WorkEvent::Question(QuestionSet {
                    id: id.to_string(),
                    questions,
                }),
            );
        }
        RunEvent::Compacted { .. } => apply_work(s, WorkEvent::Compaction { running: false }),
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

/// One event of agent work. Pure state: no I/O, so every call is testable.
pub fn apply_work(s: &mut Shell, ev: WorkEvent) {
    let c = &mut s.chat;
    match ev {
        WorkEvent::Tasks(items) => c.tasks = items,
        WorkEvent::Subagent(sub) => upsert_subagent(&mut c.lines, sub),
        WorkEvent::Question(set) => c.question = Some(QuestionState::new(set)),
        WorkEvent::Compaction { running } => {
            c.compacting = running;
            if !running {
                c.lines
                    .push(Line::item(Item::Note("Contexto compactado".into())));
            }
        }
    }
}

/// A subagent row appears once and then changes state in place, so parallel
/// ones stay stacked in the order they started.
fn upsert_subagent(lines: &mut Vec<Line>, sub: Subagent) {
    let existing = lines.iter_mut().rev().find_map(|l| match &mut l.item {
        Some(Item::Subagent(old)) if old.id == sub.id => Some(old),
        _ => None,
    });
    match existing {
        Some(old) => *old = sub,
        None => lines.push(Line::item(Item::Subagent(sub))),
    }
}

/// Answers the open question card and hands the answers to the runtime.
pub fn answer_question(s: &mut Shell) {
    let Some(q) = s.chat.question.take_if(|q| q.ready()) else {
        return;
    };
    let answers = q.answers();
    crate::wire::answer_question(s.chat.questions.as_ref(), &q.set.id, &answers);
    s.chat.answers.push((q.set.id, answers));
}

/// Dismisses the question card without answering.
pub fn skip_question(s: &mut Shell) {
    if let Some(q) = s.chat.question.take() {
        // An empty answer set means "declined".
        crate::wire::answer_question(s.chat.questions.as_ref(), &q.set.id, &[]);
    }
}
