//! `zerith usage`: per-account, per-model and token-saver report.

use super::*;

pub(super) fn parse_usage_hours(args: &[String]) -> Result<u32, String> {
    let mut hours = 24u32;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--hours" {
            let raw = args
                .get(i + 1)
                .ok_or("--hours precisa de um número de horas (ex: 24 ou 0)")?;
            hours = raw
                .trim()
                .parse()
                .map_err(|_| format!("--hours inválido: {raw} (use um inteiro >= 0)"))?;
            i += 2;
        } else if let Some(raw) = a.strip_prefix("--hours=") {
            hours = raw
                .trim()
                .parse()
                .map_err(|_| format!("--hours inválido: {raw} (use um inteiro >= 0)"))?;
            i += 1;
        } else {
            return Err(format!("argumento desconhecido: {a}"));
        }
    }
    Ok(hours)
}

pub(super) fn pct_share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 * 100.0) / total as f64
    }
}

pub(super) fn format_limit_col(used: u64, limit: Option<u64>) -> String {
    match limit {
        Some(q) => format!("{used}/{q}"),
        None => used.to_string(),
    }
}

pub(super) fn account_status_and_remaining(
    enabled: bool,
    cooldown_until: i64,
    now: i64,
    used_tokens: u64,
    used_reqs: u64,
    quota_tokens: Option<u64>,
    quota_requests: Option<u64>,
) -> (String, String) {
    let tok_pct = quota_tokens.map(|q| {
        if q == 0 {
            0.0
        } else {
            (q.saturating_sub(used_tokens) as f64 * 100.0) / q as f64
        }
    });
    let req_pct = quota_requests.map(|q| {
        if q == 0 {
            0.0
        } else {
            (q.saturating_sub(used_reqs) as f64 * 100.0) / q as f64
        }
    });
    let remaining = match (tok_pct, req_pct) {
        (Some(t), Some(r)) => format!("{:.1}%", t.min(r)),
        (Some(t), None) => format!("{t:.1}%"),
        (None, Some(r)) => format!("{r:.1}%"),
        (None, None) => "-".to_string(),
    };

    let exhausted = quota_tokens.is_some_and(|q| used_tokens >= q)
        || quota_requests.is_some_and(|q| used_reqs >= q);
    let status = if !enabled {
        "desativada".to_string()
    } else if cooldown_until > now {
        let secs = (cooldown_until - now).saturating_add(999) / 1000;
        format!("cooldown {secs} s")
    } else if exhausted {
        "cota esgotada".to_string()
    } else {
        "ativa".to_string()
    };
    (status, remaining)
}

pub fn format_token_saver_savings(ts: &TokenSaverSettings, summary: &UsageSummary) -> String {
    let total = summary.total_saved_tokens;
    let rtk = summary.total_saved_rtk_tokens;
    let ponytail = summary.total_saved_ponytail_tokens;
    let caveman = summary.total_saved_caveman_tokens;
    let gross = summary
        .total_prompt_tokens
        .saturating_add(summary.total_completion_tokens)
        .saturating_add(total);
    let pct_bruto = if gross == 0 {
        0.0
    } else {
        (total as f64 * 100.0) / gross as f64
    };

    let rtk_tag = format!("[{}]:", if ts.rtk_enabled { "ativo" } else { "desligado" });
    let pt_tag = format!("[{}]:", ts.ponytail.as_str());
    let cv_tag = format!("[{}]:", ts.caveman.as_str());

    let mut out = String::new();
    out.push_str("Token Saver — Economia por Protocolo:\n");
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "RTK (compressão de ferramentas)",
        rtk_tag,
        rtk,
        pct_share(rtk, total),
    ));
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "Ponytail (lazy senior dev)",
        pt_tag,
        ponytail,
        pct_share(ponytail, total),
    ));
    out.push_str(&format!(
        "  {:<32} {:<13} {:>8} tokens  ({:.1}% da economia)\n",
        "Caveman (respostas concisas)",
        cv_tag,
        caveman,
        pct_share(caveman, total),
    ));
    out.push_str(&format!(
        "  Total economizado: {total} tokens (-{pct_bruto:.1}% sobre o consumo bruto)"
    ));
    out
}

pub fn format_usage_report(
    settings: &Settings,
    summary: &UsageSummary,
    hours: u32,
    now: i64,
) -> String {
    let mut out = String::new();
    let window_label = if hours == 0 {
        "todo histórico".to_string()
    } else {
        format!("últimas {hours}h")
    };
    match (&summary.active_account, &summary.active_model) {
        (Some(acc), Some(model)) => {
            out.push_str(&format!(
                "conta ativa: {acc}   modelo em uso: {model}   (janela: {window_label})\n\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "conta ativa: (nenhum registro ainda)   (janela: {window_label})\n\n"
            ));
        }
    }

    // Cross-reference configured accounts (preserving those with 0 usage) and DB stats.
    struct Row {
        id: String,
        provider: String,
        priority: String,
        status: String,
        tokens: String,
        reqs: String,
        remaining: String,
    }

    let configured = effective_accounts(settings);
    let mut configured_ids: HashSet<&str> = HashSet::with_capacity(configured.len());
    let mut rows: Vec<Row> = Vec::new();

    for acc in configured {
        let id = acc.id.trim();
        configured_ids.insert(id);
        let stats: Option<&AccountWindowStats> =
            summary.accounts.iter().find(|s| s.account_id == id);
        let used_tokens = stats.map_or(0, |s| s.total_tokens);
        let used_reqs = stats.map_or(0, |s| s.requests);
        let cooldown_until = stats.map_or(0, |s| s.cooldown_until);
        let provider = if !acc.provider.trim().is_empty() {
            acc.provider.trim().to_string()
        } else {
            stats
                .map(|s| s.provider.trim())
                .filter(|p| !p.is_empty())
                .unwrap_or("-")
                .to_string()
        };
        let (status, remaining) = account_status_and_remaining(
            acc.enabled,
            cooldown_until,
            now,
            used_tokens,
            used_reqs,
            acc.quota_tokens,
            acc.quota_requests,
        );
        rows.push(Row {
            id: id.to_string(),
            provider,
            priority: acc.priority.to_string(),
            status,
            tokens: format_limit_col(used_tokens, acc.quota_tokens),
            reqs: format_limit_col(used_reqs, acc.quota_requests),
            remaining,
        });
    }

    for s in &summary.accounts {
        if configured_ids.contains(s.account_id.as_str()) {
            continue;
        }
        let provider = if s.provider.trim().is_empty() {
            "-".to_string()
        } else {
            s.provider.clone()
        };
        let (status, remaining) = account_status_and_remaining(
            true,
            s.cooldown_until,
            now,
            s.total_tokens,
            s.requests,
            None,
            None,
        );
        rows.push(Row {
            id: s.account_id.clone(),
            provider,
            priority: "-".to_string(),
            status,
            tokens: s.total_tokens.to_string(),
            reqs: s.requests.to_string(),
            remaining,
        });
    }

    out.push_str("Contas & Cotas:\n");
    if rows.is_empty() {
        out.push_str("  (nenhuma conta configurada ou registrada)\n\n");
    } else {
        out.push_str(&format!(
            "  {:<18} {:<12} {:>5}  {:<15} {:<19} {:<12} {:>8}\n",
            "id", "provider", "prio", "status", "tokens", "reqs", "restante"
        ));
        for r in &rows {
            out.push_str(&format!(
                "  {:<18} {:<12} {:>5}  {:<15} {:<19} {:<12} {:>8}\n",
                r.id, r.provider, r.priority, r.status, r.tokens, r.reqs, r.remaining
            ));
        }
        out.push('\n');
    }

    out.push_str("Modelos & Delegação:\n");
    if summary.models.is_empty() {
        out.push_str("  (nenhum modelo registrado nesta janela)\n\n");
    } else {
        out.push_str(&format!(
            "  {:<26} {:<12} {:<10} {:>6} {:>10} {:>10} {:>10}\n",
            "model", "provider", "role_kind", "reqs", "prompt", "completion", "total"
        ));
        for m in &summary.models {
            out.push_str(&format!(
                "  {:<26} {:<12} {:<10} {:>6} {:>10} {:>10} {:>10}\n",
                m.model,
                m.provider,
                m.role_kind,
                m.requests,
                m.prompt_tokens,
                m.completion_tokens,
                m.total_tokens
            ));
        }
        out.push('\n');
    }

    out.push_str(&format_token_saver_savings(&settings.token_saver, summary));
    out
}

pub fn usage_cmd(paths: &Paths, args: &[String]) -> Res {
    let hours = parse_usage_hours(args)?;
    paths.ensure()?;
    let store = Store::open(paths.db())?;
    let settings = load_settings(paths);
    let summary = store.usage_summary(hours)?;
    println!(
        "{}",
        format_usage_report(&settings, &summary, hours, now_ms())
    );
    Ok(())
}
