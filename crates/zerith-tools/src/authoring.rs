//! `create_skill`, `create_agent`, `delete_ai_item`: the model writes a skill or
//! an agent for the person to review.
//!
//! These tools only carry a request to the write layers in `zerith-skills`
//! (`author`) and `zerith-core` (`agents_write`), which hold the rules. What
//! the tool adds is context the rules need:
//!
//! - **Who is creating.** The agent tool is built with the tools and permission
//!   mode of the turn it is registered in, so a model-made agent can never be
//!   given more than its creator has.
//! - **Where it lands.** Everything is written disabled; the result tells the
//!   model, and through it the person, to open the screen and enable it.
//!
//! Registered by the host only at the top level of a turn: a subagent must not
//! be able to mint more agents (see `Registry::subset` users).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use zerith_core::agents_write::{self, Creator, NewAgent};
use zerith_core::Paths;
use zerith_security::Action;
use zerith_skills::author::NewSkill;
use zerith_skills::Library;

use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

/// Where a created item is written, and what the creator can do.
#[derive(Clone)]
pub struct Authoring {
    pub paths: Paths,
    pub project_dir: Option<PathBuf>,
    /// The tools the current turn has (for the agent subset rule).
    pub tools: Vec<String>,
    /// The current permission mode, as written in `permissionMode`.
    pub mode: String,
}

fn str_arg<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError::BadArguments(format!("{k} precisa ser um texto")))
}

fn reject_unknown(a: &Value, allowed: &[&str]) -> Result<()> {
    if let Some(obj) = a.as_object() {
        if let Some(extra) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(ToolError::BadArguments(format!(
                "argumento desconhecido: {extra}"
            )));
        }
    }
    Ok(())
}

struct Kind {
    spec: ToolSpec,
    ctx: Arc<Authoring>,
    op: Op,
}

#[derive(Clone, Copy)]
enum Op {
    CreateSkill,
    CreateAgent,
    Delete,
}

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
        returns: "o que foi gravado e o próximo passo para a pessoa",
        permission: Permission::Write,
        timeout: Duration::from_secs(10),
        cancellable: true,
        local: true,
        source: "built-in".into(),
    }
}

/// The three tools, bound to `ctx`.
pub fn tools(ctx: Authoring) -> Vec<Box<dyn Tool>> {
    let ctx = Arc::new(ctx);
    let mk = |op, s| -> Box<dyn Tool> {
        Box::new(Kind {
            spec: s,
            ctx: ctx.clone(),
            op,
        })
    };
    vec![
        mk(
            Op::CreateSkill,
            spec(
                "create_skill",
                "Cria uma skill (instruções reutilizáveis) para a pessoa revisar. Ela nasce DESATIVADA e só passa a valer depois que a pessoa a ativar na tela Skills. Use quando um procedimento se repetir. A descrição é UMA frase dizendo QUANDO usar; as instruções trazem o procedimento em passos, as armadilhas e como verificar o resultado. Não inclua scripts, chaves nem caminhos da pasta pessoal.",
                json!({"type": "object", "properties": {
                    "name": {"type": "string", "description": "minúsculas, números e hífens, até 64"},
                    "description": {"type": "string", "description": "uma frase: quando usar esta skill"},
                    "instructions": {"type": "string", "description": "corpo em Markdown: passos numerados, armadilhas, verificação"},
                    "references": {"type": "array", "description": "arquivos de apoio de texto, para não encher o corpo", "items": {"type": "object", "properties": {
                        "path": {"type": "string", "description": "references/<arquivo>.md (ou templates/, assets/)"},
                        "content": {"type": "string"}
                    }, "required": ["path", "content"]}}
                }, "required": ["name", "description", "instructions"]}),
            ),
        ),
        mk(
            Op::CreateAgent,
            spec(
                "create_agent",
                "Cria um agente (um papel com instruções próprias) para a pessoa revisar. Ele nasce DESATIVADO. Um agente nunca tem mais poder que você: só ferramentas que você tem agora, e o mesmo modo de permissão ou um mais restrito. Sem `tools`, ele recebe só leitura (read_file, list_dir, glob, grep). Dê um papel focado, o mínimo de ferramentas e um critério claro de parada.",
                json!({"type": "object", "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string", "description": "uma frase: para que serve"},
                    "instructions": {"type": "string", "description": "o papel, o que fazer, formato da resposta e quando parar"},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "só ferramentas que você já tem; padrão: somente leitura"},
                    "model": {"type": "string"},
                    "max_turns": {"type": "integer", "description": "1 a 100"},
                    "permission_mode": {"type": "string", "description": "igual ou mais restrito que o seu; nunca bypassPermissions"}
                }, "required": ["name", "description", "instructions"]}),
            ),
        ),
        mk(
            Op::Delete,
            spec(
                "delete_ai_item",
                "Apaga uma skill ou um agente que VOCÊ criou. Itens feitos pela pessoa não podem ser apagados por aqui.",
                json!({"type": "object", "properties": {
                    "kind": {"type": "string", "enum": ["skill", "agent"]},
                    "name": {"type": "string"}
                }, "required": ["kind", "name"]}),
            ),
        ),
    ]
}

impl Tool for Kind {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// A write to the exact file or folder, so the policy asks in default mode
    /// and refuses in plan mode.
    fn action(&self, args: &Value, _: &CallCtx<'_>) -> Result<Option<Action>> {
        let name = str_arg(args, "name")?;
        let path = match self.op {
            Op::CreateAgent => self.ctx.paths.agents().join(format!("{name}.md")),
            Op::Delete if args["kind"] == "agent" => {
                self.ctx.paths.agents().join(format!("{name}.md"))
            }
            _ => self.ctx.paths.skills().join(name),
        };
        Ok(Some(Action::Write(path)))
    }

    fn run(&self, args: &Value, _: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        exec.check()?;
        let session = exec.host.session().unwrap_or_else(|| "sessao".into());
        let paths = &self.ctx.paths;
        match self.op {
            Op::CreateSkill => {
                reject_unknown(args, &["name", "description", "instructions", "references"])?;
                let files = args["references"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|r| {
                                (
                                    r["path"].as_str().unwrap_or("").to_string(),
                                    r["content"].as_str().unwrap_or("").to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let new = NewSkill {
                    name: str_arg(args, "name")?.to_string(),
                    description: str_arg(args, "description")?.to_string(),
                    instructions: str_arg(args, "instructions")?.to_string(),
                    files,
                };
                let lib =
                    Library::open(paths.skills()).map_err(|e| ToolError::Failed(e.to_string()))?;
                let made = lib
                    .create_ai_skill(&new, &session)
                    .map_err(|e| ToolError::Failed(e.to_string()))?;
                let mut out = format!(
                    "Skill {:?} criada com {} arquivo(s), DESATIVADA. Peça à pessoa para abrir Skills, revisar e ativar; até lá ela não está disponível.",
                    made.name, made.files
                );
                for n in &made.notes {
                    out.push_str(&format!("\nAviso do scanner: {n}"));
                }
                Ok(out)
            }
            Op::CreateAgent => {
                reject_unknown(
                    args,
                    &[
                        "name",
                        "description",
                        "instructions",
                        "tools",
                        "model",
                        "max_turns",
                        "permission_mode",
                    ],
                )?;
                let new = NewAgent {
                    name: str_arg(args, "name")?.to_string(),
                    description: str_arg(args, "description")?.to_string(),
                    instructions: str_arg(args, "instructions")?.to_string(),
                    tools: args["tools"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    model: args["model"].as_str().map(String::from),
                    max_turns: args["max_turns"].as_u64().map(|n| n as u32),
                    permission_mode: args["permission_mode"].as_str().map(String::from),
                };
                let have: Vec<&str> = self.ctx.tools.iter().map(String::as_str).collect();
                let creator = Creator {
                    tools: &have,
                    mode: &self.ctx.mode,
                };
                let file = agents_write::create_ai_agent(
                    paths,
                    self.ctx.project_dir.as_deref(),
                    &new,
                    &creator,
                )
                .map_err(ToolError::Failed)?;
                Ok(format!(
                    "Agente {:?} criado em {}, DESATIVADO. Peça à pessoa para abrir Agentes, revisar e ativar.",
                    new.name,
                    file.display()
                ))
            }
            Op::Delete => {
                reject_unknown(args, &["kind", "name"])?;
                let name = str_arg(args, "name")?;
                match args["kind"].as_str() {
                    Some("skill") => {
                        let lib = Library::open(paths.skills())
                            .map_err(|e| ToolError::Failed(e.to_string()))?;
                        lib.delete_ai_skill(name)
                            .map_err(|e| ToolError::Failed(e.to_string()))?;
                    }
                    Some("agent") => {
                        agents_write::delete_ai_agent(paths, name).map_err(ToolError::Failed)?
                    }
                    _ => {
                        return Err(ToolError::BadArguments(
                            "kind deve ser skill ou agent".into(),
                        ))
                    }
                }
                Ok(format!("{name:?} apagado."))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tools: &[&str], mode: &str) -> (Authoring, PathBuf) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/test-scratch/authoring-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (
            Authoring {
                paths: Paths::with_root(&root),
                project_dir: None,
                tools: tools.iter().map(|s| s.to_string()).collect(),
                mode: mode.into(),
            },
            root,
        )
    }

    fn run(tool: &dyn Tool, args: Value, root: &std::path::Path) -> Result<String> {
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            root,
        );
        let cancel = zerith_providers::Cancel::default();
        let host = crate::Host::default();
        let c = CallCtx {
            workspace: root,
            policy: &policy,
            memory: None,
            scope: None,
        };
        let e = Exec {
            cancel: &cancel,
            deadline: std::time::Instant::now() + Duration::from_secs(5),
            timeout: Duration::from_secs(5),
            host: &host,
        };
        tool.run(&args, &c, &e)
    }

    fn by_name(v: &[Box<dyn Tool>], n: &str) -> usize {
        v.iter().position(|t| t.spec().name == n).unwrap()
    }

    #[test]
    fn the_model_creates_a_disabled_skill_and_is_told_to_ask_the_person() {
        let (a, root) = ctx(&["read_file"], "default");
        let t = tools(a.clone());
        let out = run(
            t[by_name(&t, "create_skill")].as_ref(),
            json!({"name": "revisor", "description": "Use para revisar diffs Rust.",
                   "instructions": "# Revisor\n\n1. Leia o diff."}),
            &root,
        )
        .unwrap();
        assert!(out.contains("DESATIVADA"), "{out}");
        let lib = Library::open(a.paths.skills()).unwrap();
        assert!(lib.enabled().unwrap().is_empty());
        assert_eq!(lib.ai_created().unwrap().len(), 1);
    }

    #[test]
    fn an_agent_cannot_have_a_tool_the_creating_turn_lacks() {
        let (a, root) = ctx(&["read_file", "grep"], "default");
        let t = tools(a.clone());
        let e = run(
            t[by_name(&t, "create_agent")].as_ref(),
            json!({"name": "x", "description": "d", "instructions": "faça",
                   "tools": ["read_file", "shell"]}),
            &root,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("não está disponível"), "{e}");
        assert!(zerith_core::agents::list_all(&a.paths, None).is_empty());
    }

    #[test]
    fn an_agent_is_created_disabled_with_read_only_tools_by_default() {
        let (a, root) = ctx(
            &["read_file", "list_dir", "glob", "grep", "shell"],
            "default",
        );
        let t = tools(a.clone());
        let out = run(
            t[by_name(&t, "create_agent")].as_ref(),
            json!({"name": "leitor", "description": "Le codigo.", "instructions": "Leia e resuma."}),
            &root,
        )
        .unwrap();
        assert!(out.contains("DESATIVADO"), "{out}");
        assert!(zerith_core::agents::list(&a.paths, None).is_empty());
        let d = &zerith_core::agents::list_all(&a.paths, None)[0];
        assert!(!d.tools.as_ref().unwrap().iter().any(|t| t == "shell"));
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        let (a, root) = ctx(&["read_file"], "default");
        let t = tools(a);
        let e = run(
            t[by_name(&t, "create_skill")].as_ref(),
            json!({"name": "a", "description": "d", "instructions": "i", "enabled": true}),
            &root,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("argumento desconhecido"), "{e}");
    }

    #[test]
    fn deleting_works_only_for_what_the_model_made() {
        let (a, root) = ctx(&["read_file"], "default");
        std::fs::create_dir_all(a.paths.agents()).unwrap();
        std::fs::write(a.paths.agents().join("meu.md"), "do usuario").unwrap();
        let t = tools(a.clone());
        let del = t[by_name(&t, "delete_ai_item")].as_ref();
        assert!(run(del, json!({"kind": "agent", "name": "meu"}), &root).is_err());
        assert!(a.paths.agents().join("meu.md").exists());
    }

    #[test]
    fn the_policy_action_is_a_write_to_the_exact_target() {
        let (a, root) = ctx(&["read_file"], "default");
        let t = tools(a.clone());
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Plan,
            &root,
        );
        let c = CallCtx {
            workspace: &root,
            policy: &policy,
            memory: None,
            scope: None,
        };
        let act = t[by_name(&t, "create_agent")]
            .action(&json!({"name": "novo"}), &c)
            .unwrap()
            .unwrap();
        assert_eq!(act, Action::Write(a.paths.agents().join("novo.md")));
    }
}
