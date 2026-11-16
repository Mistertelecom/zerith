//! `--json-schema`: a deliberately small validator.
//!
//! Supported keywords: `type` (a name or a list), `enum`, `required`,
//! `properties` and `items`, applied recursively. Everything else
//! (`pattern`, `minimum`, `oneOf`, `$ref`, ...) is ignored, not enforced, and
//! the docs say so. A schema this cannot judge never rejects a good answer;
//! it also never proves one correct.

use serde_json::Value;

/// The JSON a model answered with: the whole text, a fenced block, or the
/// first balanced `{...}` / `[...]`.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let body = after.split_once('\n').map(|(_, b)| b).unwrap_or(after);
        if let Some(end) = body.find("```") {
            if let Ok(v) = serde_json::from_str(body[..end].trim()) {
                return Some(v);
            }
        }
    }
    let open = t.find(['{', '['])?;
    let (o, c) = if t.as_bytes()[open] == b'{' {
        (b'{', b'}')
    } else {
        (b'[', b']')
    };
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, &b) in t.as_bytes().iter().enumerate().skip(open) {
        if in_str {
            match (esc, b) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return serde_json::from_str(&t[open..=i]).ok();
            }
        }
    }
    None
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_ok(want: &str, v: &Value) -> bool {
    let got = type_of(v);
    got == want || (want == "number" && got == "integer")
}

/// Every violation, with a JSON-pointer-ish path. `Ok` when none.
pub fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();
    walk(schema, value, "$", &mut errs);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

fn walk(schema: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    if let Some(t) = schema.get("type") {
        let wanted: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !wanted.is_empty() && !wanted.iter().any(|w| type_ok(w, v)) {
            errs.push(format!(
                "{at}: esperado {}, veio {}",
                wanted.join(" ou "),
                type_of(v)
            ));
            return;
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum") {
        if !options.contains(v) {
            errs.push(format!("{at}: fora dos valores permitidos"));
        }
    }
    if let (Some(Value::Array(req)), Value::Object(obj)) = (schema.get("required"), v) {
        for k in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(k) {
                errs.push(format!("{at}: falta a chave obrigatória {k:?}"));
            }
        }
    }
    if let (Some(Value::Object(props)), Value::Object(obj)) = (schema.get("properties"), v) {
        for (k, sub) in props {
            if let Some(child) = obj.get(k) {
                walk(sub, child, &format!("{at}.{k}"), errs);
            }
        }
    }
    if let (Some(items), Value::Array(list)) = (schema.get("items"), v) {
        for (i, child) in list.iter().enumerate() {
            walk(items, child, &format!("{at}[{i}]"), errs);
        }
    }
}

/// The schema from `--json-schema`: inline when it starts with `{`, else a
/// file.
pub fn load(arg: &str) -> Result<Value, String> {
    let text = if arg.trim_start().starts_with('{') {
        arg.to_string()
    } else {
        std::fs::read_to_string(arg).map_err(|e| format!("--json-schema {arg}: {e}"))?
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("--json-schema não é JSON: {e}"))?;
    if !v.is_object() {
        return Err("--json-schema precisa ser um objeto JSON".into());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_is_found_in_plain_fenced_and_chatty_answers() {
        assert_eq!(extract_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(extract_json("```json\n{\"a\":2}\n```").unwrap()["a"], 2);
        assert_eq!(
            extract_json("Aqui está: {\"a\":\"}\",\"b\":[1,2]} pronto").unwrap()["b"][1],
            2
        );
        assert_eq!(extract_json("[1,2]").unwrap()[0], 1);
        assert!(extract_json("sem json nenhum").is_none());
        assert!(extract_json("{\"a\":").is_none());
    }

    #[test]
    fn required_keys_and_types_are_checked_recursively() {
        let schema = json!({
            "type": "object",
            "required": ["nome", "idade"],
            "properties": {
                "nome": {"type": "string"},
                "idade": {"type": "integer"},
                "nota": {"type": "number"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "nivel": {"enum": ["a", "b"]}
            }
        });
        let good = json!({"nome":"x","idade":3,"nota":2,"tags":["a"],"nivel":"a","extra":true});
        assert!(validate(&schema, &good).is_ok());

        let bad = json!({"nome":1,"tags":["a",2],"nivel":"z"});
        let errs = validate(&schema, &bad).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("\"idade\"")), "{errs:?}");
        assert!(errs.iter().any(|e| e.starts_with("$.nome")));
        assert!(errs.iter().any(|e| e.starts_with("$.tags[1]")));
        assert!(errs.iter().any(|e| e.starts_with("$.nivel")));

        assert!(validate(&schema, &json!([1])).is_err());
        // A float is not an integer; an integer is a number.
        assert!(validate(&json!({"type":"integer"}), &json!(1.5)).is_err());
        assert!(validate(&json!({"type":"number"}), &json!(1)).is_ok());
        assert!(validate(&json!({"type":["string","null"]}), &json!(null)).is_ok());
        // Unsupported keywords never reject.
        assert!(validate(&json!({"pattern":"^x$","minimum":9}), &json!("y")).is_ok());
    }

    #[test]
    fn load_accepts_inline_json_and_rejects_the_rest() {
        assert!(load("{\"type\":\"object\"}").is_ok());
        assert!(load("{oops").is_err());
        assert!(load("arquivo-que-nao-existe.json").is_err());
    }
}
