//! `skill{name}`: progressive disclosure. The system prompt lists skills by
//! name and description; the model calls this to read one body
//! (`Services::skill` holds the library). The skill's `allowed-tools` come back
//! as a note and are remembered in the host (`Host::skill_allowed_tools`) so a
//! guard can narrow the tool set later; they are advice, not enforcement.

use serde_json::{json, Value};

use crate::fntool::{make, str_arg};
use crate::{Permission, Result, Tool, ToolError};

pub fn check(args: &Value) -> Result<&str> {
    str_arg(args, "name")
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make(
        "skill",
        "Carrega as instruções completas de uma skill listada no prompt. Chame antes de seguir a skill.",
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
    )]
}
