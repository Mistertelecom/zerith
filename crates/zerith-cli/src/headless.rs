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

use zerith_core::settings::{ProviderEntry, WireProtocol};
use zerith_core::{secrets, AccountEntry, MediaSettings, Paths, Settings};
use zerith_mcp::McpRegistry;
use zerith_memory::{MemoryBackend, Scope, SqliteMemory};
use zerith_providers::pricing::{self, Price, PriceTable, Tokens};
use zerith_providers::{Cancel, Client, Protocol, ProviderConfig, ProviderError, ProviderKind};
use zerith_runtime::{run_agent_turn, AgentOptions, RunError, RunEvent, TurnContext, TurnReport};
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

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|v| !v.is_empty())
}

fn env_non_empty(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(crate) fn effective_accounts(settings: &Settings) -> Vec<&AccountEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut rev = Vec::new();
    for acc in settings.accounts.iter().rev() {
        let id = acc.id.trim();
        if !id.is_empty() && seen.insert(id) {
            rev.push(acc);
        }
    }
    rev.reverse();
    rev
}

fn apply_provider_entry(cfg: &mut ProviderConfig, entry: &ProviderEntry) {
    if let Some(p) = entry.protocol {
        cfg.protocol = match p {
            WireProtocol::OpenAi => Protocol::OpenAi,
            WireProtocol::Anthropic => Protocol::Anthropic,
        };
    }
    if let Some(u) = non_empty(entry.base_url.as_deref()) {
        cfg.base_url = u.to_string();
    }
    if let Some(m) = non_empty(entry.default_model.as_deref()) {
        cfg.default_model = Some(m.to_string());
    }
    if let Some(t) = entry.timeout_ms {
        cfg.timeout_ms = t;
    }
    if entry.context_limit.is_some() {
        cfg.context_limit = entry.context_limit;
    }
}

fn resolve_config_key(
    settings: &Settings,
    cfg: &ProviderConfig,
    env: &impl Fn(&str) -> Option<String>,
    store: &dyn secrets::SecretStore,
) -> Result<Option<String>, secrets::SecretError> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(acc_id) = non_empty(cfg.account_id.as_deref()) {
        if let Some(acc_ref) = settings
            .account(acc_id)
            .and_then(|a| non_empty(a.api_key_ref.as_deref()))
        {
            candidates.push(acc_ref);
        }
        if !candidates.contains(&acc_id) {
            candidates.push(acc_id);
        }
    }
    if let Some(prov_ref) = settings
        .provider_entry(&cfg.name)
        .and_then(|p| non_empty(p.api_key_ref.as_deref()))
    {
        if !candidates.contains(&prov_ref) {
            candidates.push(prov_ref);
        }
    }
    let prov_name = cfg.name.trim();
    if !prov_name.is_empty() && !candidates.contains(&prov_name) {
        candidates.push(prov_name);
    }
    if candidates.is_empty() {
        return secrets::resolve_api_key_with(store, env, &cfg.name, None);
    }
    for key_ref in candidates {
        if let Some(k) = secrets::resolve_api_key_with(store, env, &cfg.name, Some(key_ref))? {
            if !k.trim().is_empty() {
                return Ok(Some(k));
            }
        }
    }
    Ok(None)
}

/// The provider for this process: env as before, then gateway/settings
/// routing (when env does not force provider/base_url), then OS keyring.
pub fn provider_config(settings: &Settings) -> Result<ProviderConfig, Boxed> {
    provider_config_with(
        settings,
        ProviderConfig::from_env(),
        |k| std::env::var(k).ok(),
        &secrets::Configured,
    )
}

fn provider_config_with(
    settings: &Settings,
    base_env_cfg: ProviderConfig,
    env: impl Fn(&str) -> Option<String>,
    store: &dyn secrets::SecretStore,
) -> Result<ProviderConfig, Boxed> {
    let env_provider_set = env_non_empty(&env, "ZERITH_PROVIDER").is_some();
    let env_base_url_set = env_non_empty(&env, "ZERITH_BASE_URL").is_some();
    let env_model = env_non_empty(&env, "ZERITH_MODEL");
    let env_api_key = env_non_empty(&env, secrets::API_KEY_ENV);

    let mut cfg = base_env_cfg;
    let has_settings_routing = !settings.accounts.is_empty()
        || non_empty(settings.provider.as_deref()).is_some()
        || non_empty(settings.routing.main_model.as_deref()).is_some()
        || !settings.providers.is_empty()
        || non_empty(settings.base_url.as_deref()).is_some();

    if !env_provider_set && !env_base_url_set && has_settings_routing {
        let mut base_cfg = cfg.clone();
        if let Some(prov) = non_empty(settings.provider.as_deref()) {
            let kind = ProviderKind::parse(prov);
            let mut pcfg = ProviderConfig::new(kind);
            pcfg.name = if kind == ProviderKind::Custom {
                prov.to_string()
            } else {
                kind.as_str().to_string()
            };
            if let Some(entry) = settings
                .provider_entry(prov)
                .or_else(|| settings.provider_entry(&pcfg.name))
            {
                apply_provider_entry(&mut pcfg, entry);
            }
            if let Some(url) = non_empty(settings.base_url.as_deref()) {
                pcfg.base_url = url.to_string();
            }
            if cfg.api_key.is_some() {
                pcfg.api_key = cfg.api_key.clone();
            }
            if cfg.default_model.is_some() {
                pcfg.default_model = cfg.default_model.clone();
            }
            if cfg.account_id.is_some() {
                pcfg.account_id = cfg.account_id.clone();
            }
            base_cfg = pcfg;
        } else {
            if let Some(entry) = settings.provider_entry(&base_cfg.name) {
                apply_provider_entry(&mut base_cfg, entry);
            }
            if let Some(url) = non_empty(settings.base_url.as_deref()) {
                base_cfg.base_url = url.to_string();
            }
        }

        let target_model = env_model
            .as_deref()
            .or_else(|| non_empty(settings.routing.main_model.as_deref()))
            .or_else(|| non_empty(settings.model.as_deref()))
            .or_else(|| non_empty(base_cfg.default_model.as_deref()))
            .unwrap_or("");

        let account_fallback;
        let gateway_target =
            if target_model.is_empty() && non_empty(settings.provider.as_deref()).is_none() {
                let mut enabled: Vec<&AccountEntry> = effective_accounts(settings)
                    .into_iter()
                    .filter(|a| a.enabled)
                    .collect();
                enabled.sort_by_key(|a| a.priority);
                if let Some(acc) = enabled.first() {
                    account_fallback = format!("{}:", acc.id);
                    account_fallback.as_str()
                } else {
                    target_model
                }
            } else {
                target_model
            };

        let (gw, resolved_model) =
            zerith_runtime::resolve_gateway(settings, gateway_target, &base_cfg);
        if let Some(cand) = gw.candidates.into_iter().next() {
            cfg = cand.config;
            if cfg.default_model.is_none() && !resolved_model.trim().is_empty() {
                cfg.default_model = Some(resolved_model);
            }
        } else {
            cfg = base_cfg;
        }
    }

    if cfg.default_model.is_none() {
        cfg.default_model = non_empty(settings.routing.main_model.as_deref())
            .or_else(|| non_empty(settings.model.as_deref()))
            .map(str::to_string);
    }

    if let Some(k) = env_api_key {
        cfg.api_key = Some(k);
    } else if cfg.api_key.is_none() && cfg.kind.needs_key() {
        cfg.api_key = resolve_config_key(settings, &cfg, &env, store)?;
    }

    Ok(cfg)
}

pub(crate) fn resolve_model(
    cli_model: Option<String>,
    settings: &Settings,
    cfg: &ProviderConfig,
) -> Option<String> {
    resolve_model_with_env(cli_model, settings, cfg, |k| std::env::var(k).ok())
}

fn resolve_model_with_env(
    cli_model: Option<String>,
    settings: &Settings,
    cfg: &ProviderConfig,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    cli_model
        .and_then(|s| non_empty(Some(&s)).map(str::to_string))
        .or_else(|| env_non_empty(&env, "ZERITH_MODEL"))
        .or_else(|| non_empty(settings.routing.main_model.as_deref()).map(str::to_string))
        .or_else(|| non_empty(cfg.default_model.as_deref()).map(str::to_string))
        .or_else(|| {
            cfg.known_models
                .iter()
                .find_map(|m| non_empty(Some(m)).map(str::to_string))
        })
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

/// Preço do orçamento: o informado (por milhão) vence; senão o catálogo
/// público do OpenRouter. Um combo usa o modelo mais caro dele, já que o
/// failover pode cair em qualquer um.
fn budget_price(
    manual: (Option<f64>, Option<f64>),
    model: &str,
    settings: &Settings,
    table: impl FnOnce() -> PriceTable,
) -> Result<Price, String> {
    match manual {
        (Some(i), Some(o)) => {
            return Ok(Price {
                input: i / 1e6,
                output: o / 1e6,
                cache_read: None,
                cache_write: None,
            })
        }
        (None, None) => {}
        _ => return Err("--price-in e --price-out vão juntos".into()),
    }
    let table = table();
    let combo = settings.routing.combos.iter().find(|c| c.name == model);
    let names: Vec<&str> = match combo {
        Some(c) => c.models.iter().map(String::as_str).collect(),
        None => vec![model],
    };
    let mut found: Vec<Price> = Vec::new();
    for n in &names {
        match table.lookup(n) {
            Some((_, p)) => found.push(p),
            None => {
                return Err(format!(
                    "--max-budget-usd: sem preço para {n} no catálogo do OpenRouter \
                     (sem rede e sem cópia, ou modelo fora do catálogo). Informe \
                     --price-in/--price-out (ou ZERITH_PRICE_IN/ZERITH_PRICE_OUT)."
                ))
            }
        }
    }
    found
        .into_iter()
        .max_by(|a, b| (a.input + a.output).total_cmp(&(b.input + b.output)))
        .ok_or_else(|| format!("--max-budget-usd: combo {model} sem modelos"))
}

pub fn run(paths: &Paths, args: &[String]) -> Result<(), Boxed> {
    let h = parse_headless(args)?;
    let schema = h.json_schema.as_deref().map(schema::load).transpose()?;
    let mut settings = load_settings(paths);
    let contrib = zerith_plugins::Library::open(&paths.root().join("plugins"))
        .and_then(|l| l.contributions())
        .ok();
    if let Some(c) = &contrib {
        c.apply_hooks(&mut settings);
    }
    // Rides the window's billion-context proxy when it is open; else direct.
    zerith_runtime::bili::attach(paths.root(), settings.context.billion_context);
    let cfg = provider_config(&settings)?;
    let model = resolve_model(h.model.clone(), &settings, &cfg)
        .ok_or("sem modelo: use --model, routing.main_model ou ZERITH_MODEL")?;
    let prices = match h.max_budget_usd {
        Some(_) => Some(budget_price(
            (
                price(h.price_in, "ZERITH_PRICE_IN"),
                price(h.price_out, "ZERITH_PRICE_OUT"),
            ),
            &model,
            &settings,
            || pricing::load(&MediaSettings::catalog_dir(paths)),
        )?),
        None => None,
    };
    let client = Client::new(cfg)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let session_id = select_session(&store, &h.session, &h.prompt)?;

    let workspace = std::env::current_dir()?;
    let mut policy = headless_policy(&settings, &workspace, &h)?;
    let mut tools = Registry::new();
    let mut mcp = McpRegistry::load(paths, Some(&workspace));
    let mut guards = None;
    let remote_label = if let Some(raw) = h.remote.as_deref() {
        let target = zerith_tools::remote::parse_remote_target(raw, &settings.remote.targets)?;
        tools.register(zerith_tools::remote::remote_shell_tool(
            target,
            raw.to_string(),
        ));
        Some(raw.to_string())
    } else {
        None
    };
    if !h.no_tools {
        if let Some(c) = &contrib {
            mcp.extend(c.mcp.iter().map(|(_, s)| s.clone()).collect());
        }
        for note in &mcp.notes {
            eprintln!("zerith: {note}");
        }
        let done = mcp.register_into(&mut tools);
        for e in &done.errors {
            eprintln!("zerith: {e}");
        }
        let g =
            zerith_runtime::guards::Guards::new(settings.clone(), &mut policy, session_id.clone());
        tools.register(g.enter_plan_mode_tool());
        tools.register(g.exit_plan_mode_tool());
        guards = Some(std::sync::Arc::new(g));
        // Create skills/agents, as in the window.
        let have: Vec<String> = tools.specs().iter().map(|s| s.name.clone()).collect();
        for t in zerith_tools::authoring_tools(zerith_tools::Authoring {
            paths: paths.clone(),
            project_dir: Some(workspace.clone()),
            tools: have,
            mode: policy.mode().settings_name().to_string(),
        }) {
            tools.register(t);
        }
    }
    // `@server:resource` in the prompt becomes inline context.
    let expanded = if h.no_tools || mcp.is_empty() {
        h.prompt.clone()
    } else {
        let (text, errors) = mcp.expand_mentions(&h.prompt);
        for e in &errors {
            eprintln!("zerith: {e}");
        }
        text
    };
    let prompt = match remote_label {
        Some(raw) if !h.no_tools => format!(
            "[alvo remoto ativo: {raw} — use a ferramenta remote_shell para executar comandos nesse host/container]\n\n{expanded}"
        ),
        _ => expanded,
    };

    let memory = SqliteMemory::open(paths.db())?;
    let scope = Scope::workspace(&workspace);
    let memory = (!settings.memory.paused).then_some(&memory as &dyn MemoryBackend);
    let skills = Library::open(paths.skills()).ok().map(|l| {
        if let Some(c) = &contrib {
            l.with_extra(c.skill_dirs())
        } else {
            l
        }
    });

    let cancel = Cancel::default();
    signals::install(cancel.clone());

    let extra_agents = contrib.as_ref().map(|c| c.agents.as_slice()).unwrap_or(&[]);
    let agent_def = match h.agent.as_deref() {
        Some(name) => Some(
            zerith_core::agents::find_with_extra(paths, Some(&workspace), extra_agents, name)
                .ok_or_else(|| format!("agente desconhecido: {name} (veja `zerith agents`)"))?,
        ),
        None => None,
    };

    let format = h.format;
    let spent = Cell::new(0f64);
    let over_budget = Cell::new(false);
    // Um turno; com `--goal`, um por rodada (as opções são refeitas a cada um).
    let run_once = |prompt: &str, sink: &mut dyn FnMut(&RunEvent)| {
        let mut opts = AgentOptions::for_profile(zerith_runtime::active_profile());
        opts.spill_dir = Some(paths.root().join("spill"));
        // Same pre-turn summary as the window: a long `--resume` chain would
        // otherwise only be trimmed under pressure, mid-turn.
        match zerith_runtime::compact::auto_compact_routed(
            &store,
            &settings,
            &client,
            &session_id,
            &model,
            opts.history,
            &cancel,
        ) {
            Ok(Some(r)) => eprintln!(
                "zerith: histórico resumido ({} → {} tokens)",
                r.tokens_before, r.tokens_after
            ),
            Ok(None) => {}
            Err(e) => eprintln!("zerith: resumo automático falhou: {e}"),
        }
        if let Some(g) = &guards {
            opts.tools = Some(&tools);
            opts.policy = Some(&policy);
            // O mesmo Guards em todas as rodadas: o gate de plano guarda o
            // modo anterior.
            opts.middleware.push(Box::new(SharedGuards(g.clone())));
        }
        if let Some(n) = h.max_turns {
            opts.max_steps = n;
        }
        opts.reasoning_effort = h.effort.clone();
        opts.orchestrator = h.orchestrator;
        run_agent_turn(
            &store,
            &client,
            TurnContext {
                memory,
                scope: Some(&scope),
                skills: skills.as_ref(),
                settings: Some(&settings),
                agent: agent_def.as_ref(),
                ..TurnContext::default()
            },
            opts,
            &session_id,
            &model,
            prompt,
            &cancel,
            |e| {
                sink(&e);
                if let (
                    RunEvent::Usage {
                        input,
                        output,
                        cache_read,
                        cache_write,
                        ..
                    },
                    Some(p),
                    Some(max),
                ) = (&e, prices, h.max_budget_usd)
                {
                    spent.set(
                        spent.get()
                            + p.cost(&Tokens {
                                prompt: *input,
                                completion: *output,
                                cache_read: *cache_read,
                                cache_write: *cache_write,
                            }),
                    );
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
                let Some(line) = stream_event_json(e) else {
                    return;
                };
                println!("{line}");
            },
        )
    };

    if h.goal {
        let goal = GoalRun {
            objective: &prompt,
            format,
            settings: &settings,
            client: &client,
            model: &model,
            workspace: &workspace,
            cancel: &cancel,
        };
        let outcome = match run_goal(&goal, run_once) {
            Err(_) if over_budget.get() => Err(Box::new(ExitWith(
                1,
                format!(
                    "orçamento excedido: US$ {:.4} de US$ {:.4}",
                    spent.get(),
                    h.max_budget_usd.unwrap_or(0.0)
                ),
            )) as Boxed),
            o => o,
        };
        end_session(&settings, &session_id, &workspace);
        return outcome;
    }

    let mut digest = zerith_runtime::learn::TurnDigest::new(&prompt);
    let result = run_once(&prompt, &mut |e| digest.observe(e));

    let outcome = (|| -> Result<(), Boxed> {
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

        let stopped_at_limit = report.stop_reason == "max_steps";
        // Skip synchronous learning on incomplete turns that hit max_steps so
        // headless runs return immediately without polluting memory/skills.
        if !stopped_at_limit {
            digest.answer = report.text.clone();
            let learned = learn_after(
                &settings,
                &store,
                &client,
                &session_id,
                &model,
                memory.map(|m| (m, &scope)),
                skills.as_ref(),
                &digest,
                &cancel,
            );
            if format == OutputFormat::StreamJson && !learned.is_empty() {
                println!(
                    "{}",
                    serde_json::json!({"type": "learned", "items": learned})
                );
            }
        }
        let structured = schema
            .as_ref()
            .map(|s| match schema::extract_json(&report.text) {
                Some(v) => schema::validate(s, &v).map(|()| v),
                None => Err(vec!["a resposta não contém JSON".to_string()]),
            });
        let usage = build_usage_json(&report);
        let cost = prices.map(|p| {
            p.cost(&Tokens {
                prompt: report.prompt_tokens,
                completion: report.completion_tokens,
                cache_read: report.cache_read_tokens,
                cache_write: report.cache_write_tokens,
            })
        });
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
    })();

    end_session(&settings, &session_id, &workspace);
    outcome
}

fn end_session(settings: &Settings, session_id: &str, workspace: &std::path::Path) {
    let end = zerith_core::hooks::run_hooks(
        zerith_core::hooks::HookEvent::SessionEnd,
        &zerith_core::hooks::HookPayload {
            session_id: session_id.to_string(),
            cwd: workspace.to_path_buf(),
            ..Default::default()
        },
        settings,
    );
    for e in &end.errors {
        eprintln!("zerith: {e}");
    }
}

/// Um `Guards` compartilhado entre os turnos de um `--goal`.
struct SharedGuards(std::sync::Arc<zerith_runtime::guards::Guards>);

impl zerith_runtime::Middleware for SharedGuards {
    fn before(&self, call: &mut zerith_runtime::ToolInvocation) -> zerith_runtime::Flow {
        self.0.before(call)
    }
    fn after(&self, call: &zerith_runtime::ToolInvocation, out: &mut zerith_runtime::ToolOutcome) {
        self.0.after(call, out)
    }
}

struct GoalRun<'a> {
    objective: &'a str,
    format: OutputFormat,
    settings: &'a Settings,
    client: &'a Client,
    model: &'a str,
    workspace: &'a std::path::Path,
    cancel: &'a Cancel,
}

/// `zerith -p --goal "<objetivo>"`: turnos até o juiz dar o objetivo como
/// cumprido ou bloqueado ([`zerith_runtime::goal`]). Uma linha por rodada no
/// stderr; o veredito final no stdout (texto) ou no campo `goal` (JSON).
/// Sai com 0 só em `done`.
fn run_goal(
    g: &GoalRun<'_>,
    run_once: impl Fn(&str, &mut dyn FnMut(&RunEvent)) -> Result<TurnReport, RunError>,
) -> Result<(), Boxed> {
    use zerith_runtime::goal;
    let judge = goal::ClientJudge::new(Some(g.settings), g.client, g.model);
    eprintln!("zerith: objetivo ativo (juiz: {})", judge.model);
    let mut last: Option<TurnReport> = None;
    let mut failure: Option<RunError> = None;
    let out = goal::run(
        g.objective,
        &judge,
        g.cancel,
        goal::MAX_ROUNDS,
        |_, prompt| {
            let mut ev = goal::Evidence::new(g.workspace);
            match run_once(prompt, &mut |e| ev.observe(e)) {
                Ok(r) => {
                    let answer = r.text.clone();
                    last = Some(r);
                    Ok(goal::Round {
                        answer,
                        evidence: ev,
                    })
                }
                Err(e) => {
                    let msg = e.to_string();
                    failure = Some(e);
                    Err(msg)
                }
            }
        },
        |line| {
            eprintln!("{line}");
            if g.format == OutputFormat::StreamJson {
                println!("{}", serde_json::json!({"type": "goal", "text": line}));
            }
        },
    );
    if let (goal::End::Failed, Some(e)) = (out.end, failure) {
        return Err(e.into());
    }
    let text = last.as_ref().map_or("", |r| r.text.as_str());
    match g.format {
        OutputFormat::Text => {
            println!("{text}");
            println!("{}", out.line());
        }
        OutputFormat::Json | OutputFormat::StreamJson => {
            let mut obj = serde_json::json!({
                "session_id": last.as_ref().map(|r| r.session_id.clone()),
                "result": text,
                "goal": {
                    "verdict": out.end.label(),
                    "reason": out.reason,
                    "rounds": out.rounds,
                },
            });
            if g.format == OutputFormat::StreamJson {
                obj["type"] = "result".into();
            }
            println!("{obj}");
        }
    }
    match out.end {
        goal::End::Done => Ok(()),
        goal::End::Cancelled => Err(Box::new(ExitWith(
            signals::received().unwrap_or(143),
            "cancelado".into(),
        ))),
        _ => Err(Box::new(ExitWith(1, out.line()))),
    }
}

fn opts_steps(h: &Headless) -> usize {
    h.max_turns
        .unwrap_or_else(|| zerith_runtime::active_profile().limits().max_steps)
}

/// Runs `zerith_runtime::learn` after a CLI turn when `[learning]` is on.
/// Logs one stderr line either way (`aprender: …` / `aprendi: …`) so a
/// headless run shows whether a review happened; returns the items as JSON.
#[allow(clippy::too_many_arguments)]
pub fn learn_after(
    settings: &Settings,
    store: &Store,
    client: &Client,
    session_id: &str,
    model: &str,
    memory: Option<(&dyn MemoryBackend, &zerith_memory::Scope)>,
    skills: Option<&Library>,
    digest: &zerith_runtime::learn::TurnDigest,
    cancel: &Cancel,
) -> Vec<serde_json::Value> {
    use zerith_runtime::learn;
    if !settings.learning.enabled {
        return Vec::new();
    }
    if learn::gate(digest).is_empty() {
        eprintln!("zerith: aprender: turno sem sinal, nenhuma revisão");
        return Vec::new();
    }
    let Some((rc, rm)) = learn::route(settings, model, client.config()) else {
        eprintln!("zerith: aprender: sem modelo para a revisão");
        return Vec::new();
    };
    let pass = learn::Pass {
        memory,
        skills,
        session_id,
    };
    let reviewer = learn::ClientReviewer {
        client: &rc,
        model: &rm,
        record: Some((store, session_id)),
    };
    let out = learn::after_turn(digest, &pass, &reviewer, cancel);
    eprintln!(
        "zerith: aprender: revisão com {rm} (sinais: {}){}",
        out.signals.join("; "),
        out.error
            .as_ref()
            .map(|e| format!(", erro: {e}"))
            .unwrap_or_default()
    );
    for r in &out.refused {
        eprintln!("zerith: aprender: recusado: {r}");
    }
    for l in &out.learned {
        let what = l
            .name
            .as_deref()
            .map_or(l.text.clone(), |n| format!("{n} — {}", l.text));
        eprintln!("zerith: aprendi ({} {}): {what}", l.kind, l.op);
    }
    if out.learned.is_empty() {
        eprintln!("zerith: aprender: nada novo para guardar");
    }
    out.learned.iter().map(|l| l.to_json()).collect()
}

fn stream_event_json(e: RunEvent) -> Option<serde_json::Value> {
    Some(match e {
        RunEvent::Delta(t) => serde_json::json!({"type": "delta", "text": t}),
        RunEvent::Reasoning(t) => serde_json::json!({"type": "reasoning", "text": t}),
        RunEvent::Done { stored } => {
            serde_json::json!({"type": "done", "message_id": stored})
        }
        RunEvent::Usage {
            input,
            output,
            cache_read,
            cache_write,
            ttft_ms,
        } => {
            serde_json::json!({
                "type": "usage",
                "input": input,
                "output": output,
                "cache_read": cache_read,
                "cache_write": cache_write,
                "ttft_ms": ttft_ms,
            })
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
        RunEvent::Route {
            account_id,
            provider,
            model,
            attempts,
        } => serde_json::json!({
            "type": "route",
            "account_id": account_id,
            "provider": provider,
            "model": model,
            "attempts": attempts,
        }),
        RunEvent::TokenSaved {
            tool,
            before_bytes,
            after_bytes,
            filter,
        } => serde_json::json!({
            "type": "token_saved",
            "tool": tool,
            "before_bytes": before_bytes,
            "after_bytes": after_bytes,
            "filter": filter,
        }),
        RunEvent::ToolProgress {
            tool_call_id,
            tool,
            chunk,
        } => serde_json::json!({
            "type": "tool_progress",
            "tool_call_id": tool_call_id,
            "tool": tool,
            "chunk": chunk,
        }),
        RunEvent::Steered { text } => serde_json::json!({
            "type": "steered",
            "text": text,
        }),
        RunEvent::SkillsLoaded { names } => serde_json::json!({
            "type": "skills_loaded",
            "names": names,
        }),
        RunEvent::Subagent {
            id,
            name,
            state,
            summary,
            model,
        } => serde_json::json!({
            "type": "subagent",
            "id": id,
            "name": name,
            "state": state,
            "summary": summary,
            "model": model,
        }),
        _ => return None,
    })
}

fn build_usage_json(report: &TurnReport) -> serde_json::Value {
    let mut usage = serde_json::json!({
        "prompt_tokens": report.prompt_tokens,
        "completion_tokens": report.completion_tokens,
        "saved_tokens": report.saved_tokens,
        "saved_rtk_tokens": report.saved_rtk_tokens,
        "saved_ponytail_tokens": report.saved_ponytail_tokens,
        "saved_caveman_tokens": report.saved_caveman_tokens,
    });
    if let Some(acc) = &report.account_id {
        usage["account_id"] = serde_json::Value::String(acc.clone());
    }
    if let Some(m) = &report.model_used {
        usage["model_used"] = serde_json::Value::String(m.clone());
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zerith_core::secrets::{SecretError, SecretStore};

    struct MemSecrets(HashMap<String, String>);

    impl SecretStore for MemSecrets {
        fn get(&self, name: &str) -> Result<String, SecretError> {
            self.0
                .get(name)
                .cloned()
                .ok_or_else(|| SecretError::NotFound(name.to_string()))
        }

        fn set(&self, _name: &str, _value: &str) -> Result<(), SecretError> {
            Ok(())
        }

        fn delete(&self, _name: &str) -> Result<(), SecretError> {
            Ok(())
        }
    }

    fn h(args: &[&str]) -> Headless {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_headless(&v).unwrap()
    }

    #[test]
    fn budget_price_prefers_flags_then_catalog_and_takes_the_dearest_combo_model() {
        let table = || {
            PriceTable::from_json(&serde_json::json!({"data": [
                {"id": "anthropic/claude-opus-5.5", "pricing": {"prompt": "0.000004", "completion": "0.00002"}},
                {"id": "deepseek/deepseek-chat", "pricing": {"prompt": "0.0000003", "completion": "0.0000012"}}
            ]}))
        };
        let mut s = Settings::default();
        let manual = budget_price((Some(2.0), Some(8.0)), "x", &s, || unreachable!()).unwrap();
        assert_eq!((manual.input, manual.output), (2e-6, 8e-6));
        assert!(budget_price((Some(2.0), None), "x", &s, table).is_err());
        let p = budget_price((None, None), "claude-opus-5-5", &s, table).unwrap();
        assert_eq!(p.output, 0.00002);
        let err = budget_price((None, None), "modelo-local", &s, table).unwrap_err();
        assert!(err.contains("--price-in"), "{err}");
        assert!(budget_price((None, None), "deepseek-chat", &s, PriceTable::default).is_err());
        s.routing.combos.push(zerith_core::ComboEntry {
            name: "rapido".into(),
            models: vec!["deepseek-chat".into(), "conta:claude-opus-5-5".into()],
            ..Default::default()
        });
        let c = budget_price((None, None), "rapido", &s, table).unwrap();
        assert_eq!(c.input, 0.000004);
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

    #[test]
    fn provider_config_picks_gateway_candidate_and_keyring_secret() {
        if std::env::var("ZERITH_API_KEY").is_ok() {
            return;
        }
        let mut settings = Settings {
            accounts: vec![
                AccountEntry {
                    id: "ds-disabled".into(),
                    provider: "deepseek".into(),
                    priority: 1,
                    enabled: false,
                    ..Default::default()
                },
                AccountEntry {
                    id: "ds-main".into(),
                    provider: "deepseek".into(),
                    base_url: Some("https://ds.example/v1".into()),
                    default_model: Some("deepseek-chat".into()),
                    api_key_ref: Some("ds_work_key".into()),
                    priority: 10,
                    enabled: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        settings.routing.main_model = Some("deepseek-chat".into());

        let secrets = MemSecrets(HashMap::from([(
            "ds_work_key".to_string(),
            "sk-ds-secret".to_string(),
        )]));

        let cfg =
            provider_config_with(&settings, ProviderConfig::default(), |_| None, &secrets).unwrap();
        assert_eq!(cfg.kind, ProviderKind::DeepSeek);
        assert_eq!(cfg.base_url, "https://ds.example/v1");
        assert_eq!(cfg.account_id.as_deref(), Some("ds-main"));
        assert_eq!(cfg.default_model.as_deref(), Some("deepseek-chat"));
        assert_eq!(cfg.api_key.as_deref(), Some("sk-ds-secret"));
    }

    #[test]
    fn provider_config_uses_settings_provider_and_env_takes_precedence() {
        if std::env::var("ZERITH_API_KEY").is_ok() {
            return;
        }
        let settings = Settings {
            provider: Some("anthropic".into()),
            providers: vec![ProviderEntry {
                name: "anthropic".into(),
                base_url: Some("https://claude-proxy.example/v1".into()),
                default_model: Some("claude-sonnet-4".into()),
                api_key_ref: Some("claude_ref".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let secrets = MemSecrets(HashMap::from([(
            "claude_ref".to_string(),
            "sk-ant-123".to_string(),
        )]));

        let cfg =
            provider_config_with(&settings, ProviderConfig::default(), |_| None, &secrets).unwrap();
        assert_eq!(cfg.kind, ProviderKind::Anthropic);
        assert_eq!(cfg.protocol, Protocol::Anthropic);
        assert_eq!(cfg.base_url, "https://claude-proxy.example/v1");
        assert_eq!(cfg.default_model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(cfg.api_key.as_deref(), Some("sk-ant-123"));

        // Env forcing ZERITH_PROVIDER overrides settings.provider / accounts.
        let mut forced = ProviderConfig::new(ProviderKind::Ollama);
        forced.default_model = None;
        let mut settings_with_main = settings.clone();
        settings_with_main.routing.main_model = Some("llama3.2".into());
        let cfg_env = provider_config_with(
            &settings_with_main,
            forced,
            |k| (k == "ZERITH_PROVIDER").then(|| "ollama".to_string()),
            &secrets,
        )
        .unwrap();
        assert_eq!(cfg_env.kind, ProviderKind::Ollama);
        assert_eq!(cfg_env.default_model.as_deref(), Some("llama3.2"));
    }

    #[test]
    fn resolve_model_respects_routing_main_model_before_default() {
        let mut settings = Settings::default();
        settings.routing.main_model = Some("combo-fast".into());
        let cfg = ProviderConfig {
            default_model: Some("gpt-4o-mini".into()),
            ..Default::default()
        };

        assert_eq!(
            resolve_model_with_env(Some("explicit-m".into()), &settings, &cfg, |_| None).as_deref(),
            Some("explicit-m")
        );
        assert_eq!(
            resolve_model_with_env(None, &settings, &cfg, |k| {
                (k == "ZERITH_MODEL").then(|| "env-m".to_string())
            })
            .as_deref(),
            Some("env-m")
        );
        assert_eq!(
            resolve_model_with_env(None, &settings, &cfg, |_| None).as_deref(),
            Some("combo-fast")
        );
        settings.routing.main_model = None;
        assert_eq!(
            resolve_model_with_env(None, &settings, &cfg, |_| None).as_deref(),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn stream_json_and_usage_json_include_route_and_token_saver_fields() {
        let route = stream_event_json(RunEvent::Route {
            account_id: "acc-1".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            attempts: 2,
        })
        .unwrap();
        assert_eq!(route["type"], "route");
        assert_eq!(route["account_id"], "acc-1");
        assert_eq!(route["provider"], "openai");
        assert_eq!(route["model"], "gpt-4o");
        assert_eq!(route["attempts"], 2);

        let saved = stream_event_json(RunEvent::TokenSaved {
            tool: "shell".into(),
            before_bytes: 2000,
            after_bytes: 400,
            filter: Some("git-diff".into()),
        })
        .unwrap();
        assert_eq!(saved["type"], "token_saved");
        assert_eq!(saved["tool"], "shell");
        assert_eq!(saved["before_bytes"], 2000);
        assert_eq!(saved["after_bytes"], 400);
        assert_eq!(saved["filter"], "git-diff");

        let child = stream_event_json(RunEvent::Subagent {
            id: "sub-1".into(),
            name: "executor: corrigir testes".into(),
            state: "running".into(),
            summary: "passo 2 de 40 · edit_file".into(),
            model: "claude-sonnet-5-5".into(),
        })
        .expect("subagents must reach stream-json");
        assert_eq!(child["type"], "subagent");
        assert_eq!(child["state"], "running");
        assert_eq!(child["summary"], "passo 2 de 40 · edit_file");

        let report = TurnReport {
            session_id: "s1".into(),
            message_id: 7,
            text: "ok".into(),
            reasoning: None,
            prompt_tokens: 100,
            completion_tokens: 40,
            saved_rtk_tokens: 20,
            saved_ponytail_tokens: 15,
            saved_caveman_tokens: 10,
            saved_tokens: 45,
            account_id: Some("acc-1".into()),
            model_used: Some("gpt-4o".into()),
            ..Default::default()
        };
        let usage = build_usage_json(&report);
        assert_eq!(usage["prompt_tokens"], 100);
        assert_eq!(usage["completion_tokens"], 40);
        assert_eq!(usage["saved_tokens"], 45);
        assert_eq!(usage["saved_rtk_tokens"], 20);
        assert_eq!(usage["saved_ponytail_tokens"], 15);
        assert_eq!(usage["saved_caveman_tokens"], 10);
        assert_eq!(usage["account_id"], "acc-1");
        assert_eq!(usage["model_used"], "gpt-4o");
    }
}
