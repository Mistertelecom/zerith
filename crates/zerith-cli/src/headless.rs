//! `zerith -p`: one agent turn without a REPL, for scripts and CI.
//!
//! Tools are on unless `--no-tools`. There is nobody to ask, so anything the
//! policy would ask about is refused (fail closed). Unless the user chose a
//! mode (`--permission-mode`, `ZERITH_MODE` or `permissions.default_mode`),
//! the mode is `default`: reads inside the workspace run, writes, shell and
//! network are refused. To let a headless run edit files, say so:
//! `--permission-mode acceptEdits` or `--allowed-tools 'write_file,edit_file'`.

use std::cell::Cell;
use std::fmt;

use zerith_core::{secrets, Paths, Settings};
use zerith_mcp::McpRegistry;
use zerith_memory::{default_scope, MemoryBackend, SqliteMemory};
use zerith_providers::{Cancel, Client, ProviderConfig, ProviderError};
use zerith_runtime::{run_agent_turn, AgentOptions, RunError, RunEvent, TurnContext};
use zerith_security::{Mode, Policy, Rule, RuleKind};
use zerith_sessions::Store;
use zerith_skills::Library;
use zerith_tools::Registry;

use crate::args::{parse_headless, select_session, Headless, OutputFormat};
use crate::{schema, signals};

/// An error that carries the process exit code.
#[derive(Debug)]
pub struct ExitWith(pub u8, pub String);

impl fmt::Display for ExitWith {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.1)
    }
}

impl std::error::Error for ExitWith {}

type Boxed = Box<dyn std::error::Error>;

/// The provider for this process: env as before, then the merged settings
/// for the model, then the OS keyring for the key.
pub fn provider_config(settings: &Settings) -> Result<ProviderConfig, Boxed> {
    let mut cfg = ProviderConfig::from_env();
    if cfg.default_model.is_none() {
        cfg.default_model = settings.model.clone();
    }
    if cfg.api_key.is_none() && cfg.kind.needs_key() {
        let key_ref = settings
            .provider_entry(&cfg.name)
            .and_then(|p| p.api_key_ref.as_deref());
        cfg.api_key = secrets::resolve_api_key(&cfg.name, key_ref)?;
    }
    Ok(cfg)
}

pub fn load_settings(paths: &Paths) -> Settings {
    let cwd = std::env::current_dir().ok();
    let loaded = Settings::load(paths, cwd.as_deref());
    for e in &loaded.errors {
        eprintln!("zerith: {e}");
    }
    loaded.settings
}

/// The policy a headless run uses (see the module docs for the default mode).
pub fn headless_policy(
    settings: &Settings,
    workspace: &std::path::Path,
    h: &Headless,
) -> Result<Policy, Boxed> {
    let mut policy = Policy::from_settings(settings, workspace)?;
    let handle = policy.mode_handle();
    let chosen = settings.permissions.default_mode.is_some()
        || std::env::var("ZERITH_MODE").is_ok_and(|v| !v.trim().is_empty());
    match h.permission_mode {
        Some(m) => handle.set(m),
        None if !chosen => handle.set(Mode::Default),
        None => {}
    }
    for (list, kind) in [
        (&h.disallowed_tools, RuleKind::Deny),
        (&h.allowed_tools, RuleKind::Allow),
    ] {
        for text in list {
            let rule = Rule::parse(kind, text)
                .ok_or_else(|| format!("regra de ferramenta inválida: {text:?}"))?;
            policy.rules.push(rule);
        }
    }
    Ok(policy)
}

fn price(flag: Option<f64>, var: &str) -> Option<f64> {
    flag.or_else(|| std::env::var(var).ok()?.trim().parse().ok())
}

/// Dollars for `input`/`output` tokens at USD-per-million prices.
pub fn cost_usd(input: u64, output: u64, price_in: f64, price_out: f64) -> f64 {
    (input as f64 * price_in + output as f64 * price_out) / 1e6
}

pub fn run(paths: &Paths, args: &[String]) -> Result<(), Boxed> {
    let h = parse_headless(args)?;
    let schema = h.json_schema.as_deref().map(schema::load).transpose()?;
    let prices = match h.max_budget_usd {
        Some(_) => {
            let pi = price(h.price_in, "ZERITH_PRICE_IN");
            let po = price(h.price_out, "ZERITH_PRICE_OUT");
            match (pi, po) {
                (Some(i), Some(o)) => Some((i, o)),
                _ => {
                    return Err("--max-budget-usd precisa dos preços por milhão de tokens: \
                                --price-in/--price-out (ou ZERITH_PRICE_IN/ZERITH_PRICE_OUT). \
                                O Zerith não tem tabela de preços própria."
                        .into())
                }
            }
        }
        None => None,
    };

    let settings = load_settings(paths);
    let cfg = provider_config(&settings)?;
    let model = h
        .model
        .clone()
        .or_else(|| cfg.default_model.clone())
        .or_else(|| cfg.known_models.first().cloned())
        .ok_or("sem modelo: use --model ou ZERITH_MODEL")?;
    let client = Client::new(cfg)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let session_id = select_session(&store, &h.session, &h.prompt)?;

    let workspace = std::env::current_dir()?;
    let policy = headless_policy(&settings, &workspace, &h)?;
    let mut tools = Registry::new();
    let mut mcp = McpRegistry::load(paths, Some(&workspace));
    if !h.no_tools {
        if let Ok(lib) = zerith_plugins::Library::open(&paths.root().join("plugins")) {
            if let Ok(c) = lib.contributions() {
                mcp.extend(c.mcp.into_iter().map(|(_, s)| s).collect());
            }
        }
        for note in &mcp.notes {
            eprintln!("zerith: {note}");
        }
        let done = mcp.register_into(&mut tools);
        for e in &done.errors {
            eprintln!("zerith: {e}");
        }
    }
    // `@server:resource` in the prompt becomes inline context.
    let prompt = if h.no_tools || mcp.is_empty() {
        h.prompt.clone()
    } else {
        let (text, errors) = mcp.expand_mentions(&h.prompt);
        for e in &errors {
            eprintln!("zerith: {e}");
        }
        text
    };

    let memory = SqliteMemory::open(paths.db())?;
    let scope = default_scope();
    let memory = (!settings.memory.paused).then_some(&memory as &dyn MemoryBackend);
    let skills = Library::open(paths.skills()).ok();

    let cancel = Cancel::default();
    signals::install(cancel.clone());

    let mut opts = AgentOptions::for_profile(zerith_runtime::active_profile());
    if !h.no_tools {
        opts.tools = Some(&tools);
        opts.policy = Some(&policy);
    }
    if let Some(n) = h.max_turns {
        opts.max_steps = n;
    }

    let format = h.format;
    let spent = Cell::new(0f64);
    let over_budget = Cell::new(false);
    let result = run_agent_turn(
        &store,
        &client,
        TurnContext {
            memory,
            scope: Some(&scope),
            skills: skills.as_ref(),
            ..TurnContext::default()
        },
        opts,
        &session_id,
        &model,
        &prompt,
        &cancel,
        |e| {
            if let (RunEvent::Usage { input, output }, Some((pi, po)), Some(max)) =
                (&e, prices, h.max_budget_usd)
            {
                spent.set(spent.get() + cost_usd(*input, *output, pi, po));
                // Checked after each model request: the request that crosses
                // the line has already been paid for.
                if spent.get() >= max {
                    over_budget.set(true);
                    cancel.cancel();
                }
            }
            if format != OutputFormat::StreamJson {
                return;
            }
            let line = match e {
                RunEvent::Delta(t) => serde_json::json!({"type": "delta", "text": t}),
                RunEvent::Reasoning(t) => serde_json::json!({"type": "reasoning", "text": t}),
                RunEvent::Done { stored } => {
                    serde_json::json!({"type": "done", "message_id": stored})
                }
                RunEvent::Usage { input, output } => {
                    serde_json::json!({"type": "usage", "input": input, "output": output})
                }
                RunEvent::ToolCall { id, name, args } => {
                    serde_json::json!({"type": "tool_call", "id": id, "name": name, "args": args})
                }
                RunEvent::ToolResult {
                    id,
                    name,
                    ok,
                    output_preview,
                } => serde_json::json!({
                    "type": "tool_result", "id": id, "name": name, "ok": ok,
                    "output_preview": output_preview
                }),
                _ => return,
            };
            println!("{line}");
        },
    );

    let report = match result {
        Ok(r) => r,
        Err(_) if over_budget.get() => {
            return Err(Box::new(ExitWith(
                1,
                format!(
                    "orçamento excedido: US$ {:.4} de US$ {:.4}",
                    spent.get(),
                    h.max_budget_usd.unwrap_or(0.0)
                ),
            )))
        }
        Err(RunError::Provider(ProviderError::Cancelled)) => {
            return Err(Box::new(ExitWith(
                signals::received().unwrap_or(143),
                "cancelado".into(),
            )))
        }
        Err(e) => return Err(e.into()),
    };

    let stopped_at_limit = report.text.starts_with("[parado: limite de");
    let structured = schema
        .as_ref()
        .map(|s| match schema::extract_json(&report.text) {
            Some(v) => schema::validate(s, &v).map(|()| v),
            None => Err(vec!["a resposta não contém JSON".to_string()]),
        });
    let usage = serde_json::json!({
        "prompt_tokens": report.prompt_tokens,
        "completion_tokens": report.completion_tokens,
    });
    let cost = prices.map(|(i, o)| cost_usd(report.prompt_tokens, report.completion_tokens, i, o));
    let mut obj = serde_json::json!({
        "session_id": report.session_id,
        "result": report.text,
        "usage": usage,
    });
    if let Some(c) = cost {
        obj["cost_usd"] = c.into();
    }
    if stopped_at_limit {
        obj["stop_reason"] = "max_turns".into();
    }
    match &structured {
        Some(Ok(v)) => obj["structured_output"] = v.clone(),
        Some(Err(errs)) => obj["schema_errors"] = errs.clone().into(),
        None => {}
    }
    match format {
        OutputFormat::Text => println!("{}", report.text),
        OutputFormat::Json => println!("{obj}"),
        OutputFormat::StreamJson => {
            obj["type"] = "result".into();
            println!("{obj}");
        }
    }
    if let Some(Err(errs)) = &structured {
        return Err(Box::new(ExitWith(
            1,
            format!("a resposta não segue o schema: {}", errs.join("; ")),
        )));
    }
    if stopped_at_limit {
        return Err(Box::new(ExitWith(
            1,
            format!("parado no limite de {} passos", opts_steps(&h)),
        )));
    }
    Ok(())
}

fn opts_steps(h: &Headless) -> usize {
    h.max_turns.unwrap_or(zerith_runtime::DEFAULT_MAX_STEPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(args: &[&str]) -> Headless {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_headless(&v).unwrap()
    }

    #[test]
    fn cost_uses_prices_per_million_tokens() {
        assert!((cost_usd(1_000_000, 500_000, 2.0, 8.0) - 6.0).abs() < 1e-9);
        assert_eq!(cost_usd(0, 0, 2.0, 8.0), 0.0);
    }

    #[test]
    fn tool_flags_become_rules_and_default_mode_is_the_cautious_one() {
        let dir = std::env::current_dir().unwrap();
        let settings = Settings::default();
        let p = headless_policy(
            &settings,
            &dir,
            &h(&[
                "-p",
                "x",
                "--allowed-tools",
                "read_file,shell(cargo test*)",
                "--disallowed-tools",
                "write_file",
            ]),
        );
        // ZERITH_MODE in the developer's environment would make this vary.
        if std::env::var("ZERITH_MODE").is_ok() {
            return;
        }
        let p = p.unwrap();
        assert_eq!(p.mode(), Mode::Default);
        assert_eq!(p.rules.len(), 3);
        assert!(p
            .rules
            .iter()
            .any(|r| r.kind == RuleKind::Deny && r.tool == "write_file"));

        let q = headless_policy(
            &settings,
            &dir,
            &h(&["-p", "x", "--permission-mode", "plan"]),
        )
        .unwrap();
        assert_eq!(q.mode(), Mode::Plan);
    }
}
