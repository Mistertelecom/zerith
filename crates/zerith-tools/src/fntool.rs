//! A tool as data plus a function pointer, for the tools that live outside
//! `builtin.rs`; and the list of all of them.

use std::time::Duration;

use serde_json::Value;
use zerith_security::Action;

use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError, ToolSpec};

pub(crate) type RunFn = fn(&Value, &CallCtx<'_>, &Exec<'_>) -> Result<String>;
pub(crate) type ActionFn = fn(&Value, &CallCtx<'_>) -> Result<Option<Action>>;

pub(crate) struct FnTool {
    pub spec: ToolSpec,
    pub action: ActionFn,
    pub run: RunFn,
    pub safe: bool,
}

impl Tool for FnTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn action(&self, args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
        (self.action)(args, ctx)
    }
    fn run(&self, args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
        (self.run)(args, ctx, exec)
    }
    fn concurrency_safe(&self, _: &Value) -> bool {
        self.safe
    }
}

/// Builds a tool with no policy action (it touches nothing the policy guards).
#[allow(clippy::too_many_arguments)]
pub(crate) fn make(
    name: &str,
    description: &str,
    parameters: Value,
    returns: &'static str,
    permission: Permission,
    timeout_s: u64,
    safe: bool,
    run: RunFn,
) -> Box<dyn Tool> {
    make_with(
        name,
        description,
        parameters,
        returns,
        permission,
        timeout_s,
        safe,
        |_, _| Ok(None),
        run,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_with(
    name: &str,
    description: &str,
    parameters: Value,
    returns: &'static str,
    permission: Permission,
    timeout_s: u64,
    safe: bool,
    action: ActionFn,
    run: RunFn,
) -> Box<dyn Tool> {
    Box::new(FnTool {
        spec: ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters,
            returns,
            permission,
            timeout: Duration::from_secs(timeout_s),
            cancellable: true,
            local: permission != Permission::Network,
            source: "built-in".into(),
        },
        action,
        run,
        safe,
    })
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError::BadArguments(format!("{key} precisa ser uma string")))
}

pub(crate) fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().filter(|s| !s.trim().is_empty())
}

/// Every tool beyond `builtin::all()`.
pub(crate) fn extra() -> Vec<Box<dyn Tool>> {
    let mut v = crate::tasks::all();
    v.extend(crate::agent_tool::all(true));
    v.extend(crate::skill_tool::all());
    v.extend(crate::ask_user::all());
    v.extend(crate::jobs::all());
    v.extend(crate::web::all());
    v.extend(crate::browser::all());
    v
}
