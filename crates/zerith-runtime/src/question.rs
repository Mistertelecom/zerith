//! `ask_user`: the turn blocks on an answer exactly like an approval does.
//!
//! The loop emits `RunEvent::Question { id, questions }` and waits on the
//! [`QuestionGate`]; the host calls [`QuestionSender::answer`] from any thread.
//! Fail closed: a cancel, or a host that went away, is "o usuário não
//! respondeu", never a guessed answer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::Value;
use zerith_providers::Cancel;
use zerith_tools::Question;

use crate::RunEvent;

const NO_ANSWER: &str = "o usuário não respondeu";

static NEXT: AtomicU64 = AtomicU64::new(1);

/// The UI's half. One answer per question, in order; for a multi-select
/// question the host joins the chosen labels (", ") into one string, and a
/// free-text reply is passed as it was typed.
#[derive(Clone)]
pub struct QuestionSender(mpsc::Sender<(u64, Vec<String>)>);

impl QuestionSender {
    /// False when the turn is no longer listening (ended or cancelled).
    pub fn answer(&self, id: u64, answers: Vec<String>) -> bool {
        self.0.send((id, answers)).is_ok()
    }
}

/// The turn's half. Keep one per conversation, like the `ApprovalGate`.
pub struct QuestionGate {
    rx: Mutex<mpsc::Receiver<(u64, Vec<String>)>>,
}

pub fn question_channel() -> (QuestionSender, QuestionGate) {
    let (tx, rx) = mpsc::channel();
    (QuestionSender(tx), QuestionGate { rx: Mutex::new(rx) })
}

impl QuestionGate {
    fn wait(&self, id: u64, cancel: &Cancel) -> Option<Vec<String>> {
        let rx = self.rx.lock().ok()?;
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((got, a)) if got == id => return Some(a),
                // A late answer to an earlier question: ignore it.
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// Settles one `ask_user` call: validates, emits the event, blocks, formats.
/// `(ok, text)` is the tool result the model reads.
pub(crate) fn settle(
    gate: Option<&QuestionGate>,
    args: &Value,
    cancel: &Cancel,
    on_event: &mut dyn FnMut(RunEvent),
) -> (bool, String) {
    let questions = match zerith_tools::parse_questions(args) {
        Ok(q) => q,
        Err(e) => return (false, format!("erro: {e}")),
    };
    let Some(gate) = gate else {
        return (false, format!("erro: {NO_ANSWER} (não há quem responda)"));
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    on_event(RunEvent::Question {
        id,
        questions: questions.clone(),
    });
    match gate.wait(id, cancel) {
        Some(answers) => (true, format_answers(&questions, &answers)),
        None => (false, format!("erro: {NO_ANSWER}")),
    }
}

fn format_answers(questions: &[Question], answers: &[String]) -> String {
    questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let a = answers
                .get(i)
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .unwrap_or("(sem resposta)");
            format!("{} -> {a}", q.question)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args() -> Value {
        json!({"questions": [
            {"question": "Qual banco?", "options": [{"label": "SQLite"}, {"label": "Postgres"}]},
            {"question": "Quais extras?", "multi_select": true,
             "options": [{"label": "A"}, {"label": "B"}]}
        ]})
    }

    #[test]
    fn the_answer_comes_back_formatted_per_question() {
        let (tx, gate) = question_channel();
        let mut seen = Vec::new();
        let cancel = Cancel::default();
        std::thread::scope(|s| {
            // The host answers from another thread as soon as it sees the event.
            let (ev_tx, ev_rx) = mpsc::channel::<u64>();
            s.spawn(move || {
                let id = ev_rx.recv().unwrap();
                tx.answer(id, vec!["SQLite".into(), "A, B".into()]);
            });
            let (ok, text) = settle(Some(&gate), &args(), &cancel, &mut |e| {
                if let RunEvent::Question { id, questions } = &e {
                    assert_eq!(questions.len(), 2);
                    ev_tx.send(*id).unwrap();
                }
                seen.push(e);
            });
            assert!(ok, "{text}");
            assert_eq!(text, "Qual banco? -> SQLite\nQuais extras? -> A, B");
        });
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn cancel_and_a_missing_gate_fail_closed() {
        let (_tx, gate) = question_channel();
        let cancel = Cancel::default();
        cancel.cancel();
        let (ok, text) = settle(Some(&gate), &args(), &cancel, &mut |_| {});
        assert!(!ok && text.contains("o usuário não respondeu"), "{text}");
        let (ok, text) = settle(None, &args(), &Cancel::default(), &mut |_| {});
        assert!(!ok && text.contains("o usuário não respondeu"), "{text}");
        // A sender that went away is not a yes.
        let (tx, gate) = question_channel();
        drop(tx);
        let (ok, _) = settle(Some(&gate), &args(), &Cancel::default(), &mut |_| {});
        assert!(!ok);
    }

    #[test]
    fn bad_questions_are_refused_before_anyone_is_asked() {
        let (_tx, gate) = question_channel();
        let mut asked = false;
        let (ok, text) = settle(
            Some(&gate),
            &json!({"questions": [{"question": "q", "options": [{"label": "só"}]}]}),
            &Cancel::default(),
            &mut |_| asked = true,
        );
        assert!(!ok && !asked, "{text}");
    }
}
