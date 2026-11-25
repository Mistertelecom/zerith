//! Nomes antigos de ferramentas que viraram uma ação de uma ferramenta só
//! (`memory_save` → `memory` com `action: "save"`). Ideia do Hermes
//! (`toolsets.py`, MIT): famílias parecidas numa ferramenta com `action`
//! custam menos schema por requisição.
//!
//! O nome antigo continua valendo em tudo que é configuração do usuário:
//! despacho, `tools:` de agentes, regras `allow/ask/deny` e `matcher` de hooks.

use serde_json::Value;

/// (nome antigo, ferramenta nova, ação).
pub const LEGACY: &[(&str, &str, &str)] = &[
    ("memory_save", "memory", "save"),
    ("memory_recall", "memory", "recall"),
    ("memory_update", "memory", "update"),
    ("memory_forget", "memory", "forget"),
    ("agent_result", "agent_manage", "result"),
    ("agent_list", "agent_manage", "list"),
    ("agent_send", "agent_manage", "send"),
    ("agent_stop", "agent_manage", "stop"),
    ("lsp_diagnostics", "lsp", "diagnostics"),
    ("lsp_definition", "lsp", "definition"),
    ("lsp_references", "lsp", "references"),
    ("lsp_symbols", "lsp", "symbols"),
];

/// Nome antigo → (ferramenta nova, ação).
pub fn resolve(name: &str) -> Option<(&'static str, &'static str)> {
    LEGACY
        .iter()
        .find(|(old, _, _)| *old == name)
        .map(|(_, new, action)| (*new, *action))
}

/// Nome antigo de uma chamada da ferramenta nova (`memory` + `save` →
/// `memory_save`).
pub fn legacy_name(tool: &str, args: &Value) -> Option<&'static str> {
    let action = args.get("action").and_then(Value::as_str)?;
    LEGACY
        .iter()
        .find(|(_, new, a)| *new == tool && *a == action)
        .map(|(old, _, _)| *old)
}

/// Todos os nomes pelos quais uma chamada responde: o usado e o equivalente
/// (antigo ou novo). Uma regra ou um matcher que cite qualquer um vale.
pub fn names<'a>(tool: &'a str, args: &Value) -> Vec<&'a str> {
    match resolve(tool) {
        Some((new, _)) => vec![tool, new],
        None => std::iter::once(tool)
            .chain(legacy_name(tool, args))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_and_new_names_point_at_each_other() {
        assert_eq!(resolve("memory_save"), Some(("memory", "save")));
        assert_eq!(resolve("memory"), None);
        assert_eq!(
            legacy_name("memory", &json!({"action": "forget"})),
            Some("memory_forget")
        );
        assert_eq!(legacy_name("memory", &json!({"action": "x"})), None);
        assert_eq!(
            names("memory", &json!({"action": "save"})),
            ["memory", "memory_save"]
        );
        assert_eq!(
            names("memory_recall", &json!({})),
            ["memory_recall", "memory"]
        );
        assert_eq!(names("shell", &json!({})), ["shell"]);
    }
}
