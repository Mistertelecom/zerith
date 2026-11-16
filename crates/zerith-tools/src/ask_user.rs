//! `ask_user`: structured questions for the person (Claude Code's
//! AskUserQuestion). The turn loop settles the call: it emits the question,
//! blocks on the answer and returns it. Run outside a turn, nobody can answer.

use serde_json::{json, Value};

use crate::fntool::make;
use crate::{Permission, Result, Tool, ToolError};

pub const MAX_QUESTIONS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    /// A very short label (a chip in the UI).
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// Validates the arguments and returns the questions.
pub fn parse_questions(args: &Value) -> Result<Vec<Question>> {
    let bad = |m: &str| ToolError::BadArguments(m.to_string());
    let list = args["questions"]
        .as_array()
        .filter(|l| !l.is_empty() && l.len() <= MAX_QUESTIONS)
        .ok_or_else(|| bad("questions precisa ter de 1 a 4 perguntas"))?;
    list.iter()
        .map(|q| {
            let text = |v: &Value| v.as_str().map(str::trim).unwrap_or("").to_string();
            let question = text(&q["question"]);
            if question.is_empty() {
                return Err(bad("cada pergunta precisa de question"));
            }
            let options: Vec<QuestionOption> = q["options"]
                .as_array()
                .map(|o| {
                    o.iter()
                        .map(|o| QuestionOption {
                            label: text(&o["label"]),
                            description: text(&o["description"]),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !(2..=4).contains(&options.len()) || options.iter().any(|o| o.label.is_empty()) {
                return Err(bad("cada pergunta precisa de 2 a 4 opções com label"));
            }
            Ok(Question {
                header: text(&q["header"]).chars().take(12).collect(),
                question,
                options,
                multi_select: q["multi_select"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make(
        "ask_user",
        "Faz de 1 a 4 perguntas objetivas ao usuário quando falta uma decisão que só ele pode tomar. Cada pergunta traz de 2 a 4 opções; o usuário também pode responder com texto livre.",
        json!({"type": "object", "properties": {"questions": {
            "type": "array", "minItems": 1, "maxItems": MAX_QUESTIONS,
            "items": {"type": "object", "properties": {
                "question": {"type": "string"},
                "header": {"type": "string", "description": "rótulo curto (até 12 caracteres)"},
                "options": {"type": "array", "minItems": 2, "maxItems": 4, "items": {
                    "type": "object", "properties": {
                        "label": {"type": "string"},
                        "description": {"type": "string"}
                    }, "required": ["label"]}},
                "multi_select": {"type": "boolean"}
            }, "required": ["question", "options"]}
        }}, "required": ["questions"]}),
        "uma linha por pergunta com a resposta do usuário",
        Permission::Read,
        3600,
        false,
        |a, _, _| {
            parse_questions(a)?;
            Err(ToolError::Failed("o usuário não respondeu".into()))
        },
    )]
}
