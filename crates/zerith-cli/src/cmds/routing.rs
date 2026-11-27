//! `zerith routing` and `zerith tokensaver`: model roles, combos and savers.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoutingField {
    Main,
    Subagent,
    Explore,
    Plan,
    Compact,
    Decision,
}

impl RoutingField {
    fn label(self) -> &'static str {
        match self {
            Self::Main => "main_model",
            Self::Subagent => "subagent_model",
            Self::Explore => "explore_model",
            Self::Plan => "plan_model",
            Self::Compact => "compact_model",
            Self::Decision => "decision_model",
        }
    }

    fn slot(self, r: &mut RoutingSettings) -> &mut Option<String> {
        match self {
            Self::Main => &mut r.main_model,
            Self::Subagent => &mut r.subagent_model,
            Self::Explore => &mut r.explore_model,
            Self::Plan => &mut r.plan_model,
            Self::Compact => &mut r.compact_model,
            Self::Decision => &mut r.decision_model,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum RoutingAction {
    Show,
    SetField {
        field: RoutingField,
        value: Option<String>,
    },
    SetRole {
        agent: String,
        model: Option<String>,
    },
    SetCombo {
        name: String,
        models: Vec<String>,
    },
    RemoveCombo {
        name: String,
    },
}

pub(super) fn parse_model_or_clear(raw: &str, what: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{what} não pode ser vazio (use `clear` para limpar)"
        ));
    }
    if trimmed.eq_ignore_ascii_case("clear") || trimmed.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

pub(super) fn parse_routing(args: &[String]) -> Result<RoutingAction, String> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Ok(RoutingAction::Show);
    };
    match sub {
        "show" => {
            if let Some(extra) = args.get(2) {
                return Err(format!("argumento inesperado em routing show: {extra}"));
            }
            Ok(RoutingAction::Show)
        }
        "main" | "subagent" | "explore" | "plan" | "compact" | "decision" => {
            let field = match sub {
                "main" => RoutingField::Main,
                "subagent" => RoutingField::Subagent,
                "explore" => RoutingField::Explore,
                "plan" => RoutingField::Plan,
                "compact" => RoutingField::Compact,
                "decision" => RoutingField::Decision,
                _ => unreachable!(),
            };
            let raw = args
                .get(2)
                .ok_or_else(|| format!("routing {sub} precisa de <modelo> (ou `clear`)"))?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            Ok(RoutingAction::SetField {
                field,
                value: parse_model_or_clear(raw, &format!("routing {sub}"))?,
            })
        }
        "role" => {
            let agent = args
                .get(2)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("routing role precisa de <agente> <modelo|clear>")?;
            let raw = args
                .get(3)
                .ok_or("routing role precisa de <modelo> (ou `clear`)")?;
            if let Some(extra) = args.get(4) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            Ok(RoutingAction::SetRole {
                agent: agent.to_string(),
                model: parse_model_or_clear(raw, "modelo da role")?,
            })
        }
        "combo" => match args.get(2).map(String::as_str) {
            Some("remove") => {
                let name = args
                    .get(3)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or("routing combo remove precisa de <nome>")?;
                if let Some(extra) = args.get(4) {
                    return Err(format!("argumento inesperado: {extra}"));
                }
                Ok(RoutingAction::RemoveCombo {
                    name: name.to_string(),
                })
            }
            Some(raw_name) => {
                let name = raw_name.trim();
                if name.is_empty() {
                    return Err("nome do combo não pode ser vazio".into());
                }
                let tail = args.get(3..).unwrap_or(&[]);
                if tail.is_empty() {
                    return Err("routing combo precisa de <nome> <m1,m2,...>".into());
                }
                let joined = tail.join(",");
                let models: Vec<String> = joined
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if models.is_empty() {
                    return Err(
                        "routing combo precisa de pelo menos um modelo em <m1,m2,...>".into(),
                    );
                }
                Ok(RoutingAction::SetCombo {
                    name: name.to_string(),
                    models,
                })
            }
            None => Err("routing combo precisa de <nome> <m1,m2,...> ou `remove <nome>`".into()),
        },
        other => Err(format!("comando desconhecido: routing {other}")),
    }
}

pub fn format_routing(r: &RoutingSettings) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "main_model      {}\n",
        r.main_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "subagent_model  {}\n",
        r.subagent_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "explore_model   {}\n",
        r.explore_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "plan_model      {}\n",
        r.plan_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "compact_model   {}\n",
        r.compact_model.as_deref().unwrap_or("(padrão)")
    ));
    out.push_str(&format!(
        "decision_model  {}\n",
        r.decision_model.as_deref().unwrap_or("(padrão)")
    ));
    if r.role_models.is_empty() {
        out.push_str("role_models     (nenhum)\n");
    } else {
        out.push_str("role_models:\n");
        for (agent, model) in &r.role_models {
            out.push_str(&format!("  {agent:<16} -> {model}\n"));
        }
    }
    let mut seen = HashSet::new();
    let mut combos = Vec::new();
    for c in r.combos.iter().rev() {
        let name = c.name.trim();
        if !name.is_empty() && seen.insert(name) {
            combos.push(c);
        }
    }
    combos.reverse();
    if combos.is_empty() {
        out.push_str("combos          (nenhum)");
    } else {
        out.push_str("combos:\n");
        for (idx, c) in combos.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&format!("  {:<16} -> {}", c.name, c.models.join(", ")));
        }
    }
    out
}

pub fn routing_cmd(paths: &Paths, args: &[String]) -> Res {
    match parse_routing(args)? {
        RoutingAction::Show => {
            let settings = load_settings(paths);
            println!("{}", format_routing(&settings.routing));
        }
        RoutingAction::SetField { field, value } => {
            Settings::save_user(paths, |s| {
                *field.slot(&mut s.routing) = value.clone();
            })?;
            match value {
                Some(m) => println!("routing.{} definido para {m}", field.label()),
                None => println!("routing.{} limpo", field.label()),
            }
        }
        RoutingAction::SetRole { agent, model } => {
            Settings::save_user(paths, |s| match &model {
                Some(m) => {
                    s.routing.role_models.insert(agent.clone(), m.clone());
                }
                None => {
                    s.routing.role_models.remove(&agent);
                }
            })?;
            match model {
                Some(m) => println!("routing.role_models.{agent} definido para {m}"),
                None => println!("routing.role_models.{agent} removido"),
            }
        }
        RoutingAction::SetCombo { name, models } => {
            let entry = ComboEntry {
                name: name.clone(),
                models: models.clone(),
                subagent_model: None,
            };
            let mut replaced = false;
            Settings::save_user(paths, |s| {
                if let Some(slot) = s.routing.combos.iter_mut().find(|c| c.name == name) {
                    *slot = entry.clone();
                    replaced = true;
                } else {
                    s.routing.combos.push(entry.clone());
                }
            })?;
            println!(
                "combo {name} {}: {}",
                if replaced { "atualizado" } else { "criado" },
                models.join(", ")
            );
        }
        RoutingAction::RemoveCombo { name } => {
            let mut removed = false;
            Settings::save_user(paths, |s| {
                let before = s.routing.combos.len();
                s.routing.combos.retain(|c| c.name != name);
                removed = s.routing.combos.len() != before;
            })?;
            if !removed {
                return Err(format!(
                    "combo '{name}' não encontrado em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!("combo {name} removido");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- tokensaver

#[derive(Debug, PartialEq)]
pub(super) enum TokenSaverAction {
    Show,
    SetPonytail(PonytailLevel),
    SetCaveman(CavemanLevel),
    SetRtk(bool),
}

pub(super) fn parse_tokensaver(args: &[String]) -> Result<TokenSaverAction, String> {
    match args.get(1).map(String::as_str) {
        None | Some("show") => {
            if let Some(extra) = args.get(2) {
                return Err(format!("argumento inesperado em tokensaver show: {extra}"));
            }
            Ok(TokenSaverAction::Show)
        }
        Some("ponytail") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver ponytail precisa de <off|lite|full|ultra>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let level = PonytailLevel::parse(raw).ok_or_else(|| {
                format!("nível ponytail inválido: {raw} (use off, lite, full ou ultra)")
            })?;
            Ok(TokenSaverAction::SetPonytail(level))
        }
        Some("caveman") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver caveman precisa de <off|lite|full|ultra|wenyan>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let level = CavemanLevel::parse(raw).ok_or_else(|| {
                format!("nível caveman inválido: {raw} (use off, lite, full, ultra ou wenyan)")
            })?;
            Ok(TokenSaverAction::SetCaveman(level))
        }
        Some("rtk") => {
            let raw = args
                .get(2)
                .ok_or("tokensaver rtk precisa de <on|off|true|false>")?;
            if let Some(extra) = args.get(3) {
                return Err(format!("argumento inesperado: {extra}"));
            }
            let enabled = match raw.trim().to_ascii_lowercase().as_str() {
                "on" | "true" => true,
                "off" | "false" => false,
                _ => {
                    return Err(format!(
                        "valor rtk inválido: {raw} (use on, off, true ou false)"
                    ))
                }
            };
            Ok(TokenSaverAction::SetRtk(enabled))
        }
        Some(other) => Err(format!("comando desconhecido: tokensaver {other}")),
    }
}

pub fn format_tokensaver_show(ts: &TokenSaverSettings, summary: Option<&UsageSummary>) -> String {
    let mut out = String::new();
    out.push_str(&format!("ponytail  {}\n", ts.ponytail.as_str()));
    out.push_str(&format!("caveman   {}\n", ts.caveman.as_str()));
    out.push_str(&format!(
        "rtk       {}\n",
        if ts.rtk_enabled { "on" } else { "off" }
    ));
    match summary {
        Some(s) => {
            out.push_str("\nEstatísticas acumuladas (últimas 24h):\n");
            out.push_str(&format_token_saver_savings(ts, s));
        }
        None => {
            out.push_str("\nEstatísticas acumuladas (últimas 24h): (sem banco de uso ainda)");
        }
    }
    out
}

pub fn tokensaver_cmd(paths: &Paths, args: &[String]) -> Res {
    match parse_tokensaver(args)? {
        TokenSaverAction::Show => {
            let settings = load_settings(paths);
            let summary = if paths.db().exists() {
                Store::open(paths.db())
                    .and_then(|s| s.usage_summary(24))
                    .ok()
            } else {
                None
            };
            println!(
                "{}",
                format_tokensaver_show(&settings.token_saver, summary.as_ref())
            );
        }
        TokenSaverAction::SetPonytail(level) => {
            Settings::save_user(paths, |s| s.token_saver.ponytail = level)?;
            println!("token_saver.ponytail definido para {}", level.as_str());
        }
        TokenSaverAction::SetCaveman(level) => {
            Settings::save_user(paths, |s| s.token_saver.caveman = level)?;
            println!("token_saver.caveman definido para {}", level.as_str());
        }
        TokenSaverAction::SetRtk(enabled) => {
            Settings::save_user(paths, |s| s.token_saver.rtk_enabled = enabled)?;
            println!(
                "token_saver.rtk_enabled definido para {}",
                if enabled {
                    "ativo (true)"
                } else {
                    "desligado (false)"
                }
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod decision_routing_tests {
    use super::*;

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_and_formats_decision_model_role() {
        assert_eq!(
            parse_routing(&args(&[
                "routing",
                "decision",
                "openrouter/microsoft/microsoft-decision-1.1",
            ])),
            Ok(RoutingAction::SetField {
                field: RoutingField::Decision,
                value: Some("openrouter/microsoft/microsoft-decision-1.1".into()),
            })
        );
        assert_eq!(
            parse_routing(&args(&["routing", "decision", "clear"])),
            Ok(RoutingAction::SetField {
                field: RoutingField::Decision,
                value: None,
            })
        );
        let mut r = RoutingSettings::default();
        assert!(format_routing(&r).contains("decision_model  (padrão)"));
        r.decision_model = Some("openrouter/microsoft/microsoft-decision-1.1".into());
        assert!(format_routing(&r)
            .contains("decision_model  openrouter/microsoft/microsoft-decision-1.1"));
    }
}
