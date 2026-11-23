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
    ), execute_code(), sleep()]
}

/// `sleep`: the wait itself runs in the turn loop, which sees the inbox and
/// the monitors and can wake early. Run outside a turn it fails closed.
fn sleep() -> Box<dyn Tool> {
    make(
        "sleep",
        "Espera sem gastar resposta do modelo: até `seconds` (máximo 3600), ou menos se o usuário mandar mensagem, um monitor imprimir algo novo ou a conversa for parada. Use para dar tempo a algo que você não consegue acompanhar com monitor (um equipamento reiniciando, uma propagação de BGP, um job externo) em vez de rodar `sleep` no shell ou ficar consultando.",
        json!({"type": "object", "properties": {
            "seconds": {"type": "integer", "description": "quanto esperar no máximo; padrão 60, máximo 3600"}
        }}),
        "quanto esperou e por que acordou",
        Permission::Read,
        3700,
        false,
        |_, _, _| Err(ToolError::Failed("sleep só roda dentro de um turno".into())),
    )
}

/// `execute_code`: the schema here, the run in the turn loop (`zerith-runtime`
/// `code_exec`), which owns the pipeline every script call goes through. Run
/// outside a turn it fails closed. Concept after Hermes Agent (MIT).
fn execute_code() -> Box<dyn Tool> {
    make(
        "execute_code",
        "Roda um script Python que chama as ferramentas do Zerith e devolve só o que ele imprime. Use quando precisar de 3 ou mais chamadas com lógica entre elas: o mesmo comando em vários equipamentos, filtrar uma saída enorme antes de ela entrar no contexto, laço com nova tentativa. Para uma chamada só, use a ferramenta direto. No script já estão disponíveis, sem import: shell, read_file, grep, glob, list_dir, write_file, edit_file, web_fetch, web_search, net_inventory, net_probe, net_path, net_diff, terminal_open, terminal_send, terminal_batch, terminal_close, json_parse e ToolError; cada função recebe os mesmos argumentos nomeados da ferramenta (ex.: `shell(command=\"ping -c1 10.0.0.1\")`) e devolve o texto do resultado, ou levanta ToolError. Cada chamada segue as mesmas regras, aprovações e mascaramento de segredos de uma chamada normal. Limites: 5 minutos, 50 chamadas por script, 50 KB de saída (início e fim).",
        json!({"type": "object", "properties": {
            "code": {"type": "string", "description": "script Python; imprima o resultado final com print()"}
        }, "required": ["code"]}),
        "o que o script imprimiu, e o número de chamadas feitas",
        Permission::Shell,
        330,
        false,
        |_, _, _| Err(ToolError::Failed("execute_code só roda dentro de um turno".into())),
    )
}
