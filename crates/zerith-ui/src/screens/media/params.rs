//! Fixed generation parameters: the form drawn from a model's descriptors, the
//! validation that keeps only what the model accepts, and the one-line summary.
//!
//! The engine validates again before any paid call; this module exists so the
//! user sees a bad value, or a parameter a new model dropped, while editing.

use std::collections::BTreeMap;

use rune::TextEdit;
use serde_json::{json, Value};
use zerith_providers::media::{MediaModel, ParamKind, ParamSpec};

/// Params the API wants as JSON integers (mirrors the engine).
const INT_PARAMS: [&str; 4] = ["duration", "n", "seed", "output_compression"];

/// A value as the form shows it: strings bare, numbers and bools by `Display`.
pub fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| {
        v.as_str()
            .and_then(|s| s.trim().replace(',', ".").parse().ok())
    })
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Checks `v` against `spec` and returns it typed as the API wants it, or why
/// it does not fit (pt-BR, ready to show).
pub fn validate(spec: &ParamSpec, v: &Value) -> Result<Value, String> {
    match &spec.kind {
        ParamKind::Enum(values) => {
            let r = render(v);
            if !values.contains(&r) {
                return Err(format!("“{r}” não é aceito; opções: {}", values.join(", ")));
            }
            if INT_PARAMS.contains(&spec.name.as_str()) {
                Ok(r.parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| json!(r)))
            } else {
                Ok(json!(r))
            }
        }
        ParamKind::Range { min, max } => {
            let x = number(v).ok_or_else(|| format!("“{}” não é um número", render(v)))?;
            if x < *min || x > *max {
                return Err(format!("{x} fora do intervalo {min}–{max}"));
            }
            if INT_PARAMS.contains(&spec.name.as_str()) || x.fract() == 0.0 {
                Ok(json!(x as i64))
            } else {
                Ok(json!(x))
            }
        }
        ParamKind::Flag => match v {
            Value::Bool(_) => Ok(v.clone()),
            Value::String(s) if s == "true" || s == "false" => Ok(json!(s == "true")),
            _ => Err("esperava ligado ou desligado".into()),
        },
        ParamKind::Text if spec.name == "seed" => int(v)
            .map(Value::from)
            .ok_or_else(|| "esperava um número inteiro".into()),
        ParamKind::Text => {
            let r = render(v);
            if r.trim().is_empty() {
                Err("vazio".into())
            } else {
                Ok(json!(r.trim()))
            }
        }
        ParamKind::Other => Err("este parâmetro não tem controle aqui".into()),
    }
}

/// Keeps the saved parameters `model` accepts and drops the rest. The second
/// value is one pt-BR note per dropped parameter, shown next to the form.
pub fn revalidate(
    saved: &BTreeMap<String, Value>,
    model: &MediaModel,
) -> (BTreeMap<String, Value>, Vec<String>) {
    let mut kept = BTreeMap::new();
    let mut notes = Vec::new();
    for (name, value) in saved {
        match model.params.iter().find(|p| &p.name == name) {
            None => notes.push(format!(
                "“{name}” foi removido: {} não aceita esse parâmetro.",
                model.name
            )),
            Some(spec) => match validate(spec, value) {
                Ok(v) => {
                    kept.insert(name.clone(), v);
                }
                Err(why) => notes.push(format!("“{name}” foi removido: {why} em {}.", model.name)),
            },
        }
    }
    (kept, notes)
}

/// `aspect_ratio 16:9 · n 1`, or a sentence when nothing is fixed.
pub fn summary(params: &BTreeMap<String, Value>) -> String {
    if params.is_empty() {
        return "Nenhum parâmetro fixo: o modelo usa os próprios padrões.".into();
    }
    params
        .iter()
        .map(|(k, v)| format!("{k} {}", render(v)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// One editable parameter. Which fields matter follows `kind`: `choice` for
/// enums, `flag` for switches, `edit` for ranges and text.
pub struct Item {
    pub name: String,
    pub kind: ParamKind,
    pub choice: Option<String>,
    pub flag: Option<bool>,
    pub edit: TextEdit,
}

/// The form of one model: a control per parameter it accepts.
#[derive(Default)]
pub struct Draft {
    pub items: Vec<Item>,
    /// Handed out when a handler outlives its field (the model changed).
    scratch: TextEdit,
}

impl Draft {
    /// The form for `model`, prefilled from the saved parameters.
    pub fn of(model: &MediaModel, saved: &BTreeMap<String, Value>) -> Self {
        let items = model
            .params
            .iter()
            .filter(|p| p.kind != ParamKind::Other)
            .map(|p| {
                let value = saved.get(&p.name);
                Item {
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    choice: match (&p.kind, value) {
                        (ParamKind::Enum(opts), Some(v)) => {
                            Some(render(v)).filter(|r| opts.contains(r))
                        }
                        _ => None,
                    },
                    flag: match (&p.kind, value) {
                        (ParamKind::Flag, Some(Value::Bool(b))) => Some(*b),
                        _ => None,
                    },
                    edit: TextEdit::new(match (&p.kind, value) {
                        (ParamKind::Range { .. } | ParamKind::Text, Some(v)) => render(v),
                        _ => String::new(),
                    }),
                }
            })
            .collect();
        Self {
            items,
            scratch: TextEdit::new(""),
        }
    }

    pub fn edit_mut(&mut self, i: usize) -> &mut TextEdit {
        match self.items.get_mut(i) {
            Some(it) => &mut it.edit,
            None => &mut self.scratch,
        }
    }

    /// The form read back into the map to save. An empty field or an untouched
    /// choice leaves the parameter out; a bad value names the parameter.
    pub fn to_map(&self, model: &MediaModel) -> Result<BTreeMap<String, Value>, String> {
        let mut out = BTreeMap::new();
        for it in &self.items {
            let Some(spec) = model.params.iter().find(|p| p.name == it.name) else {
                continue;
            };
            let raw: Option<Value> = match &it.kind {
                ParamKind::Enum(_) => it.choice.clone().map(Value::from),
                ParamKind::Flag => it.flag.map(Value::from),
                ParamKind::Range { .. } | ParamKind::Text => {
                    let t = it.edit.text.trim();
                    (!t.is_empty()).then(|| Value::from(t.to_string()))
                }
                ParamKind::Other => None,
            };
            if let Some(v) = raw {
                let typed = validate(spec, &v).map_err(|why| format!("{}: {why}", it.name))?;
                out.insert(it.name.clone(), typed);
            }
        }
        Ok(out)
    }

    /// Whether the form differs from what is saved.
    pub fn dirty(&self, model: &MediaModel, saved: &BTreeMap<String, Value>) -> bool {
        match self.to_map(model) {
            Ok(m) => &m != saved,
            Err(_) => true,
        }
    }
}
