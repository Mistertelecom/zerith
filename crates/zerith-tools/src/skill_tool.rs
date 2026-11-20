//! `skill{name}` and `skill_search{query}`: progressive disclosure. The system
//! prompt lists skills by name and description (only the relevant ones when
//! the library is large); `skill_search` finds the rest by keyword and `skill`
//! reads one body (`Services::skill` holds the library). The skill's
//! `allowed-tools` come back as a note and are remembered in the host
//! (`Host::skill_allowed_tools`) so a guard can narrow the tool set later; they
//! are advice, not enforcement.

use serde_json::{json, Value};

use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

/// Hits returned when the model does not say how many.
const SEARCH_DEFAULT: usize = 8;
const SEARCH_MAX: usize = 20;
/// A long description is cut here: the line is a pointer, `skill` has the rest.
const DESC_CHARS: usize = 200;

pub fn check(args: &Value) -> Result<&str> {
    str_arg(args, "name")
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        make(
            "skill",
            "Carrega as instruções completas de uma skill listada no prompt ou achada com `skill_search`. Chame antes de seguir a skill.",
            json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "nome da skill, como listado"}
            }, "required": ["name"]}),
            "o corpo da skill e, se houver, as ferramentas que ela declara",
            Permission::Read,
            10,
            false,
            |a, _, e| {
                let name = check(a)?;
                let (_, svc) = e.host.bound()?;
                let load = svc.skill(name).map_err(ToolError::Failed)?;
                e.host.note_skill(name, load.allowed_tools.clone());
                let mut out = load.body;
                if let Some(t) = load.allowed_tools {
                    out.push_str(&format!(
                        "\n\n[a skill declara allowed-tools: {}; é um aviso, o runtime ainda não restringe]",
                        t.join(", ")
                    ));
                }
                Ok(out)
            },
        ),
        make(
            "skill_search",
            "Busca entre TODAS as skills instaladas por palavras-chave (nome, descrição, categoria, tags e início do corpo). O prompt lista só parte delas: antes de uma tarefa especializada (um equipamento, formato de arquivo, serviço ou procedimento), pesquise com palavras-chave em inglês E em português juntas (ex.: \"mikrotik roteador router ssh\", \"legenda subtitles video\") e depois chame `skill` com o nome escolhido.",
            json!({"type": "object", "properties": {
                "query": {"type": "string", "description": "palavras-chave em inglês e português"},
                "limit": {"type": "integer", "description": "quantas trazer (padrão 8, máx. 20)"}
            }, "required": ["query"]}),
            "as skills mais relevantes: nome — descrição (categoria) e as palavras que casaram",
            Permission::Read,
            10,
            true,
            |a, _, e| {
                let query = str_arg(a, "query")?;
                let limit = a["limit"]
                    .as_u64()
                    .map_or(SEARCH_DEFAULT, |n| (n as usize).clamp(1, SEARCH_MAX));
                let (_, svc) = e.host.bound()?;
                let lib = svc
                    .skills_root()
                    .ok_or_else(|| ToolError::Failed("nenhuma biblioteca de skills neste turno".into()))?;
                let hits = zerith_skills::search::search(&lib, query, limit);
                Ok(render(query, &hits, zerith_skills::search::count(&lib)))
            },
        ),
    ]
}

fn render(query: &str, hits: &[zerith_skills::search::Hit], total: usize) -> String {
    if hits.is_empty() {
        return format!(
            "Nenhuma das {total} skills casou com \"{query}\". Tente sinônimos, em inglês e em português, ou siga sem skill."
        );
    }
    let mut out = format!(
        "{} de {total} skills para \"{query}\" (melhor primeiro). Carregue a escolhida com `skill`.",
        hits.len()
    );
    for h in hits {
        let mut desc: String = h.description.chars().take(DESC_CHARS).collect();
        if h.description.chars().count() > DESC_CHARS {
            desc = format!("{}…", desc.trim_end());
        }
        out.push_str(&format!("\n- {} — {desc}", h.name));
        if !h.category.is_empty() {
            out.push_str(&format!(" ({})", h.category));
        }
        out.push_str(&format!(" [casou: {}]", h.matched.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_line_names_the_skill_its_category_and_why() {
        let hits = [zerith_skills::search::Hit {
            name: "mikrotik-routeros-cli".into(),
            description: "Use when accessing a MikroTik RouterOS device via SSH.".into(),
            category: "networking".into(),
            score: 3.0,
            matched: vec!["mikrotik".into(), "ssh".into()],
        }];
        let out = render("mikrotik ssh", &hits, 307);
        assert!(out.starts_with("1 de 307 skills"), "{out}");
        assert!(out.contains(
            "- mikrotik-routeros-cli — Use when accessing a MikroTik RouterOS device via SSH. (networking) [casou: mikrotik, ssh]"
        ));
        assert!(render("x", &[], 307).contains("Nenhuma das 307"));
    }
}
