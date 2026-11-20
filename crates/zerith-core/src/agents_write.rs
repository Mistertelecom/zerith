//! Writing agents the model creates, safely.
//!
//! A model-made agent can later be chosen to run with the person's tools, so
//! creating one must not quietly become capability. The rules, enforced here:
//!
//! - It is written **disabled** and marked `created-by: ai`. [`list`] and
//!   [`find`] do not return disabled agents, so nothing can run it until the
//!   person reviews the file and turns it on ([`set_disabled`]).
//! - It can never have more power than its creator: every tool it names must be
//!   one the current turn has, its permission mode is the creator's or
//!   stricter (never `bypassPermissions`), and with no tools named it gets a
//!   read-only set instead of "inherit everything".
//! - It never replaces an existing agent, in any layer.
//! - The model can delete only agents it created.
//!
//! [`list`]: crate::agents::list
//! [`find`]: crate::agents::find

use std::path::{Path, PathBuf};

use crate::agents::{self, DEFAULT, MAX_INSTRUCTIONS};
use crate::Paths;

/// The tools a model-made agent gets when it names none: read-only.
pub const AI_DEFAULT_TOOLS: [&str; 4] = ["read_file", "list_dir", "glob", "grep"];

/// Permission modes from strictest to loosest.
const MODE_ORDER: [&str; 5] = [
    "plan",
    "dontAsk",
    "default",
    "acceptEdits",
    "bypassPermissions",
];

/// What the model asked for.
#[derive(Debug, Clone, Default)]
pub struct NewAgent {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub permission_mode: Option<String>,
}

/// What the model that is creating the agent can itself do.
#[derive(Debug, Clone)]
pub struct Creator<'a> {
    /// The tools the current turn has.
    pub tools: &'a [&'a str],
    /// The current permission mode, as written in `permissionMode`.
    pub mode: &'a str,
}

fn mode_rank(m: &str) -> Option<usize> {
    MODE_ORDER.iter().position(|x| x.eq_ignore_ascii_case(m))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n != DEFAULT
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn private_path(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    l.contains("c:\\users\\") || l.contains("c:/users/") || l.contains("/home/")
}

/// Everything that can be checked without the disk. Returns the tool list that
/// will be written.
pub fn validate_new(new: &NewAgent, creator: &Creator<'_>) -> Result<Vec<String>, String> {
    if !valid_name(&new.name) {
        return Err(format!(
            "nome {:?} inválido: letras, números, hífen ou sublinhado (máx. 64), e não use `default`",
            new.name
        ));
    }
    let d = new.description.trim();
    if d.is_empty() || d.contains(['\n', '\r']) || d.chars().count() > 1024 {
        return Err("a descrição é obrigatória, de uma linha só, com até 1024 caracteres".into());
    }
    let body = new.instructions.trim();
    if body.is_empty() || body.len() > MAX_INSTRUCTIONS {
        return Err(format!(
            "as instruções são obrigatórias e têm até {MAX_INSTRUCTIONS} caracteres"
        ));
    }
    if private_path(d) || private_path(body) {
        return Err("o texto contém um caminho da pasta pessoal do usuário".into());
    }
    if zerith_secret_like(d) || zerith_secret_like(body) {
        return Err("o texto parece conter uma chave ou segredo".into());
    }
    // Tools: explicit, and never more than the creator has. Absent would mean
    // "inherit everything", which is exactly the power this must not grant.
    let wanted: Vec<String> = if new.tools.is_empty() {
        AI_DEFAULT_TOOLS.iter().map(|t| t.to_string()).collect()
    } else {
        agents::canonical_names(&new.tools)
    };
    for t in &wanted {
        if !creator.tools.iter().any(|c| c == t) {
            return Err(format!(
                "a ferramenta {t:?} não está disponível para você neste turno; um agente criado pela IA nunca tem mais poder que quem o criou"
            ));
        }
    }
    if let Some(m) = &new.permission_mode {
        let Some(r) = mode_rank(m) else {
            return Err(format!("modo de permissão {m:?} desconhecido"));
        };
        if mode_rank("bypassPermissions") == Some(r) {
            return Err("um agente criado pela IA não pode usar bypassPermissions".into());
        }
        if let Some(cr) = mode_rank(creator.mode) {
            if r > cr {
                return Err(format!(
                    "o modo {m:?} é mais permissivo que o seu ({}); use o mesmo ou um mais restrito",
                    creator.mode
                ));
            }
        }
    }
    if let Some(m) = new.model.as_deref() {
        let m = m.trim();
        if m.contains(['\n', '\r'])
            || m.chars().any(char::is_control)
            || m.contains("---")
            || m.chars().count() > 200
        {
            return Err(
                "o modelo deve ser uma única linha sem caracteres de controle (máx. 200 caracteres)"
                    .into(),
            );
        }
    }
    if let Some(n) = new.max_turns {
        if n == 0 || n > 100 {
            return Err("maxTurns deve ficar entre 1 e 100".into());
        }
    }
    Ok(wanted)
}

/// Key-shaped text: a long run after a known prefix. Kept local so the core
/// crate does not depend on the security crate.
fn zerith_secret_like(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .any(|w| {
            w.len() >= 20
                && ["sk-", "ghp_", "gho_", "AKIA", "xoxb-", "xoxp-", "AIza"]
                    .iter()
                    .any(|p| w.starts_with(p))
        })
}

fn render(new: &NewAgent, tools: &[String]) -> String {
    let mut head = String::from("---\n");
    head.push_str(&format!("name: {}\n", new.name));
    head.push_str(&format!(
        "description: \"{}\"\n",
        new.description.trim().replace('"', "'")
    ));
    head.push_str(&format!("tools: {}\n", tools.join(", ")));
    if let Some(m) = new.model.as_deref().filter(|m| !m.trim().is_empty()) {
        head.push_str(&format!("model: {}\n", m.trim()));
    }
    if let Some(n) = new.max_turns {
        head.push_str(&format!("maxTurns: {n}\n"));
    }
    if let Some(m) = &new.permission_mode {
        head.push_str(&format!("permissionMode: {m}\n"));
    }
    // Written off and marked: the person turns it on after reading it.
    head.push_str("created-by: ai\ndisabled: true\n---\n\n");
    head.push_str(new.instructions.trim());
    head.push('\n');
    head
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    // A sibling then a rename: a crash never leaves half a file under the name.
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Writes a model-made agent into the user's agents folder, **disabled**.
/// Refuses a name that exists in any layer. Returns the file written.
pub fn create_ai_agent(
    paths: &Paths,
    project_dir: Option<&Path>,
    new: &NewAgent,
    creator: &Creator<'_>,
) -> Result<PathBuf, String> {
    let tools = validate_new(new, creator)?;
    let file = paths.agents().join(format!("{}.md", new.name));
    let taken = agents::list_all(paths, project_dir)
        .iter()
        .any(|a| a.name == new.name)
        || file.exists();
    if taken {
        return Err(format!(
            "já existe um agente chamado {:?}; escolha outro nome (os seus não são alterados)",
            new.name
        ));
    }
    std::fs::create_dir_all(paths.agents()).map_err(|e| e.to_string())?;
    write_atomic(&file, &render(new, &tools))?;
    Ok(file)
}

/// Turns an agent on or off by editing its `disabled:` line in place.
pub fn set_disabled(paths: &Paths, name: &str, disabled: bool) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("nome {name:?} inválido"));
    }
    let path = paths.agents().join(format!("{name}.md"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let flag = format!("disabled: {disabled}");
    let mut done = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            if !done && l.starts_with("disabled:") {
                done = true;
                flag.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !done {
        if lines.first().map(|l| l.trim()) != Some("---") {
            return Err("o agente não tem cabeçalho para alterar".into());
        }
        lines.insert(1, flag);
    }
    write_atomic(&path, &(lines.join("\n") + "\n"))
}

/// Deletes an agent, but only one the model created.
pub fn delete_ai_agent(paths: &Paths, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("nome {name:?} inválido"));
    }
    let path = paths.agents().join(format!("{name}.md"));
    let def = agents::load_file(&path, "user")
        .ok_or_else(|| format!("não há agente {name:?} na sua pasta"))?;
    if !def.created_by_ai {
        return Err(format!(
            "{name:?} não foi criado pela IA; só a pessoa pode apagar agentes dela"
        ));
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (Paths, PathBuf) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/test-scratch/agentw-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (Paths::with_root(&root), root)
    }

    const ALL: [&str; 6] = [
        "read_file",
        "list_dir",
        "glob",
        "grep",
        "write_file",
        "shell",
    ];

    fn creator() -> Creator<'static> {
        Creator {
            tools: &ALL,
            mode: "default",
        }
    }

    fn agent(name: &str) -> NewAgent {
        NewAgent {
            name: name.into(),
            description: "Revisa diffs Rust procurando unwrap.".into(),
            instructions: "Voce revisa diffs e aponta unwrap fora de testes.".into(),
            ..NewAgent::default()
        }
    }

    #[test]
    fn a_new_agent_is_disabled_marked_and_read_only_by_default() {
        let (p, _r) = paths();
        let f = create_ai_agent(&p, None, &agent("revisor"), &creator()).unwrap();
        assert!(f.is_file());
        // Invisible to every normal lookup until the person turns it on.
        assert!(agents::list(&p, None).is_empty());
        assert!(agents::find(&p, None, "revisor").is_none());
        let all = agents::list_all(&p, None);
        assert_eq!(all.len(), 1);
        let a = &all[0];
        assert!(a.disabled && a.created_by_ai);
        assert_eq!(
            a.tools.as_deref().unwrap(),
            AI_DEFAULT_TOOLS.map(String::from).as_slice()
        );
    }

    #[test]
    fn enabling_makes_it_reachable_and_disabling_hides_it_again() {
        let (p, _r) = paths();
        create_ai_agent(&p, None, &agent("revisor"), &creator()).unwrap();
        set_disabled(&p, "revisor", false).unwrap();
        assert!(agents::find(&p, None, "revisor").is_some());
        set_disabled(&p, "revisor", true).unwrap();
        assert!(agents::find(&p, None, "revisor").is_none());
    }

    #[test]
    fn it_cannot_have_a_tool_its_creator_lacks() {
        let (p, _r) = paths();
        let mut a = agent("x");
        a.tools = vec!["shell".into()];
        let limited = Creator {
            tools: &["read_file"],
            mode: "default",
        };
        let e = create_ai_agent(&p, None, &a, &limited).unwrap_err();
        assert!(e.contains("não está disponível"), "{e}");
        assert!(agents::list_all(&p, None).is_empty(), "nothing written");
    }

    #[test]
    fn claude_code_tool_names_are_mapped_before_the_check() {
        let (p, _r) = paths();
        let mut a = agent("mapeado");
        a.tools = vec!["Read".into(), "Grep".into()];
        create_ai_agent(&p, None, &a, &creator()).unwrap();
        let t = agents::list_all(&p, None)[0].tools.clone().unwrap();
        assert_eq!(t, vec!["read_file", "grep"]);
    }

    #[test]
    fn the_mode_can_be_equal_or_stricter_never_looser_never_bypass() {
        let (p, _r) = paths();
        for (mode, ok) in [
            ("plan", true),
            ("dontAsk", true),
            ("default", true),
            ("acceptEdits", false),
            ("bypassPermissions", false),
            ("inventado", false),
        ] {
            let mut a = agent("modo");
            a.permission_mode = Some(mode.into());
            let r = validate_new(&a, &creator());
            assert_eq!(r.is_ok(), ok, "{mode}: {r:?}");
        }
        // A creator in `plan` can only make `plan` agents.
        let strict = Creator {
            tools: &ALL,
            mode: "plan",
        };
        let mut a = agent("modo");
        a.permission_mode = Some("default".into());
        assert!(validate_new(&a, &strict).is_err());
        let _ = p;
    }

    #[test]
    fn it_never_replaces_an_existing_agent() {
        let (p, _r) = paths();
        std::fs::create_dir_all(p.agents()).unwrap();
        std::fs::write(p.agents().join("meu.md"), "do usuario, nao mexa").unwrap();
        let e = create_ai_agent(&p, None, &agent("meu"), &creator()).unwrap_err();
        assert!(e.contains("já existe"), "{e}");
        assert_eq!(
            std::fs::read_to_string(p.agents().join("meu.md")).unwrap(),
            "do usuario, nao mexa"
        );
    }

    #[test]
    fn the_model_can_delete_only_what_it_created() {
        let (p, _r) = paths();
        std::fs::create_dir_all(p.agents()).unwrap();
        std::fs::write(p.agents().join("meu.md"), "do usuario").unwrap();
        assert!(delete_ai_agent(&p, "meu").is_err());
        assert!(p.agents().join("meu.md").exists());
        create_ai_agent(&p, None, &agent("dela"), &creator()).unwrap();
        delete_ai_agent(&p, "dela").unwrap();
        assert!(!p.agents().join("dela.md").exists());
    }

    #[test]
    fn bad_names_and_private_text_are_refused() {
        for bad in ["", "default", "a b", "../x", "a/b", &"x".repeat(65)] {
            assert!(validate_new(&agent(bad), &creator()).is_err(), "{bad:?}");
        }
        let mut a = agent("ok");
        a.instructions = r"leia C:\Users\Joao\x.txt".into();
        assert!(validate_new(&a, &creator()).is_err());
        let mut a = agent("ok");
        a.instructions = "use sk-abcdefghijklmnopqrstuvwxyz123456".into();
        assert!(validate_new(&a, &creator()).is_err());
        let mut a = agent("ok");
        a.description = "duas\nlinhas".into();
        assert!(validate_new(&a, &creator()).is_err());
        let mut a = agent("ok");
        a.max_turns = Some(0);
        assert!(validate_new(&a, &creator()).is_err());
        let mut a = agent("ok");
        a.model = Some("gpt-4o\ndisabled: false\n---".into());
        assert!(
            validate_new(&a, &creator()).is_err(),
            "model must not inject YAML frontmatter"
        );
    }

    #[test]
    fn a_colon_or_quote_in_the_description_does_not_break_the_header() {
        let (p, _r) = paths();
        let mut a = agent("aspas");
        a.description = "Use quando: o usuário disser \"relatório\"".into();
        create_ai_agent(&p, None, &a, &creator()).unwrap();
        let got = &agents::list_all(&p, None)[0];
        assert_eq!(got.description, "Use quando: o usuário disser 'relatório'");
        assert_eq!(got.name, "aspas");
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let (p, _r) = paths();
        create_ai_agent(&p, None, &agent("limpo"), &creator()).unwrap();
        let leftovers = std::fs::read_dir(p.agents())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
