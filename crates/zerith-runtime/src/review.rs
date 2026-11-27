//! Independent `/review` engine (inspired by Hermes Agent's `review_engine.py`).
//!
//! Snapshots the recent conversation turns plus live `git status` and `git diff`
//! from the session's workspace, then asks an independent reviewer model
//! (`routing.review_model` -> `routing.subagent_model` -> active chat model) for
//! a structured senior code review without polluting the active turn's tool loop.

use std::path::Path;
use std::process::{Command, Stdio};

use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client};
use zerith_sessions::{Role, Store};

use crate::{Result, RunError, TurnContext};

const MAX_EXCERPT_MESSAGES: usize = 10;
const MAX_MSG_CHARS: usize = 3_000;
const MAX_DIFF_CHARS: usize = 24_000;

const REVIEW_SYSTEM_PROMPT: &str = "\
Você é um revisor técnico sênior independente (comando /review do Zerith). \
Revise o trabalho apresentado no trecho recente da conversa e no `git diff` real do repositório.\n\
Entregue uma revisão objetiva em português do Brasil com as seções:\n\
1. **Resumo do que foi alterado**\n\
2. **Defeitos, bugs ou riscos reais** (cite arquivo e linha quando visíveis; diga claramente se não encontrou problemas)\n\
3. **O que foi verificado vs. o que só foi lido/suposto**\n\
4. **Veredito e próximos passos recomendados**";

#[derive(Debug, Clone)]
pub struct ReviewAnswer {
    pub text: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
}

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn collect_git_snapshot(workspace: &Path) -> String {
    if !workspace.is_dir() {
        return String::new();
    }
    let status = run_git(workspace, &["status", "--short"]);
    let stat = run_git(workspace, &["diff", "--stat"]);
    let diff = run_git(workspace, &["diff", "--no-color"]);
    let staged = run_git(workspace, &["diff", "--cached", "--no-color"]);
    let mut parts = Vec::new();
    if let Some(s) = status {
        parts.push(format!("### git status --short\n```text\n{s}\n```"));
    }
    if let Some(s) = stat {
        parts.push(format!("### git diff --stat\n```text\n{s}\n```"));
    }
    let combined_diff = match (staged, diff) {
        (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let Some(d) = combined_diff {
        let truncated: String = d.chars().take(MAX_DIFF_CHARS).collect();
        let note = if d.chars().count() > MAX_DIFF_CHARS {
            "\n... [diff cortado em 24 mil caracteres]"
        } else {
            ""
        };
        parts.push(format!("### git diff\n```diff\n{truncated}{note}\n```"));
    }
    parts.join("\n\n")
}

/// Resolves which model `/review` should run on:
/// `routing.review_model` -> `routing.subagent_model` -> `fallback_model`.
pub fn resolve_review_model<'a>(
    settings: Option<&'a zerith_core::Settings>,
    fallback_model: &'a str,
) -> &'a str {
    let non_empty = |v: Option<&'a str>| v.map(str::trim).filter(|s| !s.is_empty());
    settings
        .and_then(|s| {
            non_empty(s.routing.review_model.as_deref())
                .or_else(|| non_empty(s.routing.subagent_model.as_deref()))
        })
        .unwrap_or(fallback_model)
}

/// Runs an independent `/review` over the session's recent turns and current
/// workspace git diff.
pub fn run(
    store: &Store,
    client: &Client,
    ctx: TurnContext<'_>,
    session_id: &str,
    fallback_model: &str,
    focus: &str,
    cancel: &Cancel,
) -> Result<ReviewAnswer> {
    let _ = store
        .session(session_id)?
        .ok_or_else(|| RunError::NoSession(session_id.into()))?;

    let rows = store.history_rows(session_id, 40)?;
    let mut excerpt: Vec<String> = rows
        .into_iter()
        .rev()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .filter(|m| !m.content.trim().is_empty())
        .filter(|m| {
            m.raw.as_ref().is_none_or(|r| {
                r.get("dynamic_reminder").is_none()
                    && r.get("tool_context").is_none()
                    && r.get("hook_context").is_none()
            })
        })
        .take(MAX_EXCERPT_MESSAGES)
        .map(|m| {
            let who = match m.role {
                Role::User => "Usuário",
                Role::Assistant => "Assistente",
                _ => "Msg",
            };
            let body: String = m.content.trim().chars().take(MAX_MSG_CHARS).collect();
            format!("**{who}:** {body}")
        })
        .collect();
    excerpt.reverse();

    let git_block = ctx
        .project_dir
        .map(collect_git_snapshot)
        .unwrap_or_default();

    let mut prompt_parts = Vec::new();
    let focus = focus.trim();
    if !focus.is_empty() {
        prompt_parts.push(format!("## Foco pedido pelo usuário\n{focus}"));
    }
    if !excerpt.is_empty() {
        prompt_parts.push(format!(
            "## Trecho recente da conversa\n{}",
            excerpt.join("\n\n")
        ));
    }
    if !git_block.is_empty() {
        prompt_parts.push(format!("## Alterações atuais no workspace\n{git_block}"));
    }
    if prompt_parts.is_empty() {
        return Ok(ReviewAnswer {
            text: "Nada para revisar ainda: sem histórico recente nem alterações git no workspace."
                .into(),
            model: fallback_model.to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_read_tokens: 0,
        });
    }

    let chosen_spec = resolve_review_model(ctx.settings, fallback_model);
    let mut gateway = match ctx.settings {
        Some(st) => crate::resolve_gateway(st, chosen_spec, client.config()).0,
        None => zerith_providers::Gateway::new(Vec::new()),
    };
    if gateway.candidates.is_empty() {
        gateway = crate::agent::default_gateway(client, chosen_spec);
    }
    gateway.final_retry = Some(client.retry_policy());

    let req = ChatRequest::new(
        chosen_spec,
        vec![
            ChatMessage::system(REVIEW_SYSTEM_PROMPT),
            ChatMessage::user(prompt_parts.join("\n\n")),
        ],
    )
    .max_tokens(2048);

    let out = gateway.complete_with(
        &req,
        cancel,
        |c| {
            store
                .is_account_available(
                    &c.account_id,
                    c.window_hours,
                    c.quota_tokens,
                    c.quota_requests,
                )
                .unwrap_or(true)
        },
        |_, _, _| {},
    )?;
    let (prompt_tokens, completion_tokens, cache_read_tokens) =
        out.value.usage.as_ref().map_or((0, 0, 0), |u| {
            (u.prompt_tokens, u.completion_tokens, u.cache_read_tokens)
        });
    Ok(ReviewAnswer {
        text: out.value.text.trim().to_string(),
        model: out.model,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_review_model_prefers_review_then_subagent_then_fallback() {
        let mut s = zerith_core::Settings::default();
        assert_eq!(
            resolve_review_model(Some(&s), "deepseek-chat"),
            "deepseek-chat"
        );
        s.routing.subagent_model = Some("codex:gpt-5-codex".into());
        assert_eq!(
            resolve_review_model(Some(&s), "deepseek-chat"),
            "codex:gpt-5-codex"
        );
        s.routing.review_model = Some("anthropic:claude-opus-4-6".into());
        assert_eq!(
            resolve_review_model(Some(&s), "deepseek-chat"),
            "anthropic:claude-opus-4-6"
        );
    }
}
