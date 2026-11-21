//! `zerith accounts`: add, list, toggle, reset cooldown and remove accounts.

use super::*;

pub(super) fn parse_wire_protocol(raw: &str) -> Result<WireProtocol, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(WireProtocol::OpenAi),
        "anthropic" => Ok(WireProtocol::Anthropic),
        other => Err(format!(
            "--protocol inválido: {other} (use openai ou anthropic)"
        )),
    }
}

pub(super) fn flag_val(rest: &[String], i: usize, flag: &str) -> Result<String, String> {
    rest.get(i + 1)
        .cloned()
        .ok_or_else(|| format!("{flag} precisa de um valor"))
}

pub(super) fn flag_non_empty(rest: &[String], i: usize, flag: &str) -> Result<String, String> {
    let v = flag_val(rest, i, flag)?;
    let trimmed = v.trim().to_string();
    if trimmed.is_empty() {
        return Err(format!("{flag} não pode ser vazio"));
    }
    Ok(trimmed)
}

pub(super) fn parse_account_add(rest: &[String]) -> Result<AccountEntry, String> {
    let mut entry = AccountEntry::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            "--label" => {
                entry.label = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--model" => {
                entry.default_model = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--url" => {
                entry.base_url = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--priority" => {
                let raw = flag_val(rest, i, a)?;
                entry.priority = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--priority inválido: {raw}"))?;
                i += 2;
            }
            "--quota-tokens" => {
                let raw = flag_val(rest, i, a)?;
                entry.quota_tokens = Some(
                    raw.trim()
                        .parse()
                        .map_err(|_| format!("--quota-tokens inválido: {raw}"))?,
                );
                i += 2;
            }
            "--quota-requests" => {
                let raw = flag_val(rest, i, a)?;
                entry.quota_requests = Some(
                    raw.trim()
                        .parse()
                        .map_err(|_| format!("--quota-requests inválido: {raw}"))?,
                );
                i += 2;
            }
            "--window-hours" => {
                let raw = flag_val(rest, i, a)?;
                entry.window_hours = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--window-hours inválido: {raw}"))?;
                i += 2;
            }
            "--key-ref" => {
                entry.api_key_ref = Some(flag_non_empty(rest, i, a)?);
                i += 2;
            }
            "--protocol" => {
                let raw = flag_val(rest, i, a)?;
                entry.protocol = Some(parse_wire_protocol(&raw)?);
                i += 2;
            }
            s if s.starts_with("--") => return Err(format!("argumento desconhecido: {s}")),
            s => {
                positional.push(s.to_string());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err("accounts add precisa de <id> <provider>".into());
    }
    if positional.len() > 2 {
        return Err(format!("argumento inesperado: {}", positional[2]));
    }
    let id = positional[0].trim();
    let provider = positional[1].trim();
    if id.is_empty() {
        return Err("id da conta não pode ser vazio".into());
    }
    if provider.is_empty() {
        return Err("provedor da conta não pode ser vazio".into());
    }
    entry.id = id.to_string();
    entry.provider = provider.to_string();
    Ok(entry)
}

pub(super) fn require_account_id<'a>(
    arg: Option<&'a String>,
    sub: &str,
) -> Result<&'a str, String> {
    arg.map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("accounts {sub} precisa de um <id>"))
}

pub fn format_accounts_list(settings: &Settings) -> String {
    let accounts = effective_accounts(settings);
    if accounts.is_empty() {
        return "(nenhuma conta configurada; use `zerith accounts add <id> <provider>`)"
            .to_string();
    }
    let mut out = String::new();
    for (idx, acc) in accounts.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{:<16} {:<12} {:<10} prio={:<4} modelo={:<18} cota_tok={:<10} cota_req={:<8} janela={}h  url={}",
            acc.id,
            acc.provider,
            if acc.enabled { "ativa" } else { "desativada" },
            acc.priority,
            acc.default_model.as_deref().unwrap_or("-"),
            acc.quota_tokens
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            acc.quota_requests
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            acc.window_hours,
            acc.base_url.as_deref().unwrap_or("-"),
        ));
        if let Some(ref key_ref) = acc.api_key_ref {
            out.push_str(&format!("  key_ref={key_ref}"));
        }
        if let Some(ref label) = acc.label {
            out.push_str(&format!("  label={label:?}"));
        }
    }
    out
}

pub fn accounts_cmd(paths: &Paths, args: &[String]) -> Res {
    match args.get(1).map(String::as_str) {
        None | Some("list") => {
            let settings = load_settings(paths);
            println!("{}", format_accounts_list(&settings));
        }
        Some("add") => {
            let entry = parse_account_add(args.get(2..).unwrap_or(&[]))?;
            let id = entry.id.clone();
            let secret_name = entry
                .api_key_ref
                .as_deref()
                .unwrap_or(&entry.id)
                .to_string();
            let mut replaced = false;
            Settings::save_user(paths, |s| {
                if let Some(slot) = s.accounts.iter_mut().find(|a| a.id == id) {
                    *slot = entry.clone();
                    replaced = true;
                } else {
                    s.accounts.push(entry.clone());
                }
            })?;
            println!(
                "conta {id} {} em {}",
                if replaced { "atualizada" } else { "adicionada" },
                paths.settings().display()
            );
            println!(
                "aviso: salve a chave desta conta no cofre com `zerith secret set {secret_name}`"
            );
        }
        Some("remove") => {
            let id = require_account_id(args.get(2), "remove")?;
            let mut removed = false;
            Settings::save_user(paths, |s| {
                let before = s.accounts.len();
                s.accounts.retain(|a| a.id != id);
                removed = s.accounts.len() != before;
            })?;
            if !removed {
                return Err(format!(
                    "conta '{id}' não encontrada em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!("conta {id} removida");
        }
        Some(sub @ ("enable" | "disable")) => {
            let id = require_account_id(args.get(2), sub)?;
            let enable = sub == "enable";
            let mut found = false;
            Settings::save_user(paths, |s| {
                for a in &mut s.accounts {
                    if a.id == id {
                        a.enabled = enable;
                        found = true;
                    }
                }
            })?;
            if !found {
                return Err(format!(
                    "conta '{id}' não encontrada em {}",
                    paths.settings().display()
                )
                .into());
            }
            println!(
                "conta {id} {}",
                if enable { "ativada" } else { "desativada" }
            );
        }
        Some("reset-cooldown") => {
            let id = require_account_id(args.get(2), "reset-cooldown")?;
            paths.ensure()?;
            Store::open(paths.db())?.clear_account_cooldown(id)?;
            println!("cooldown da conta {id} limpo");
        }
        Some(other) => return Err(format!("comando desconhecido: accounts {other}").into()),
    }
    Ok(())
}
