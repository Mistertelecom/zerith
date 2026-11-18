//! The `Uso` tab: quota per account and what token saving spared.

use std::collections::HashSet;

use rune::{col, el, icon, row, text, El};
use rune_kit::{Badge, BadgeVariant};
use zerith_sessions::AccountWindowStats;

use super::actions::{clear_cooldown, load_usage, now_ms};
use crate::agentwork;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, hint, Load, Tone};
use crate::Shell;

struct UsageRow<'a> {
    idx: usize,
    account_id: &'a str,
    provider: &'a str,
    enabled: bool,
    quota_tokens: Option<u64>,
    stats: Option<&'a AccountWindowStats>,
    now: i64,
}

fn account_usage_row(r: UsageRow<'_>, u: &Ui) -> El {
    let UsageRow {
        idx,
        account_id,
        provider,
        enabled,
        quota_tokens,
        stats,
        now,
    } = r;
    let consumed = stats.map_or(0, |st| st.total_tokens);
    let requests = stats.map_or(0, |st| st.requests);
    let cooldown_until = stats.map_or(0, |st| st.cooldown_until);
    let in_cooldown = cooldown_until > now;

    let usage_label = match quota_tokens {
        Some(q) => {
            let pct = ((consumed as f64 / q as f64) * 100.0).round() as u64;
            format!(
                "{} / {} tok ({pct}%) · {} req",
                agentwork::compact_count(consumed),
                agentwork::compact_count(q),
                requests
            )
        }
        None => format!(
            "{} tok · {} req",
            agentwork::compact_count(consumed),
            requests
        ),
    };

    let mut right = row().gap(8.0).center_y();
    if let Some(q) = quota_tokens.filter(|&q| q > 0) {
        let frac = (consumed as f32 / q as f32).min(1.0);
        let bar_ink = if frac > 0.95 {
            u.danger
        } else if frac > 0.80 {
            u.warn
        } else {
            u.accent
        };
        right = right.child(
            row()
                .w(68.0)
                .h(4.0)
                .radius(2.0)
                .fill(u.layer3)
                .child(el().w(68.0 * frac).h(4.0).radius(2.0).fill(bar_ink)),
        );
    }
    right = right.child(text(usage_label).font(ui::LABEL).color(u.secondary));

    if in_cooldown {
        let rem_s = ((cooldown_until - now) / 1000).max(1);
        let cd_id = account_id.to_string();
        right = right
            .child(widgets::dot(u.warn))
            .child(
                text(format!("Cooldown ({rem_s}s)"))
                    .font(ui::LABEL)
                    .color(u.warn),
            )
            .child(button(
                format!("usage-clear-cd-{idx}"),
                "Limpar cooldown",
                Tone::Plain,
                true,
                u,
                move |s, _| clear_cooldown(s, cd_id.clone()),
            ));
    } else if !enabled {
        right = right
            .child(widgets::dot(u.idle))
            .child(text("Pausada").font(ui::LABEL).color(u.faint));
    } else {
        right = right
            .child(widgets::dot(u.ok))
            .child(text("Disponível").font(ui::LABEL).color(u.muted_text));
    }

    row()
        .key(format!("usage-acc-{idx}"))
        .w_full()
        .py(8.0)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(1.0)
                .child(
                    text(account_id.to_string())
                        .font(ui::BODY)
                        .color(u.content)
                        .truncate(),
                )
                .child(
                    text(provider.to_string())
                        .font(ui::LABEL)
                        .color(u.faint)
                        .truncate(),
                ),
        )
        .child(right)
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let header = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(eyebrow("Consumo nas últimas 24 horas", u))
                .child(hint(
                    "Quanto cada conta gastou da cota e quanto o modo de economia poupou.",
                    u,
                )),
        )
        .child(button(
            "usage-refresh",
            if p.usage.is_loading() {
                "Atualizando…"
            } else {
                "Atualizar"
            },
            Tone::Plain,
            !p.usage.is_loading(),
            u,
            |s, _| load_usage(s),
        ));

    let body = match &p.usage {
        Load::Idle | Load::Loading => widgets::loading("Carregando cotas e consumo", u),
        Load::Failed(e) => widgets::error_line(format!("Não consegui ler o uso: {e}"), u),
        Load::Ready(sum) => {
            let now = now_ms();
            let active_pair = s
                .chat
                .active_route
                .clone()
                .or_else(|| sum.active_account.clone().zip(sum.active_model.clone()));
            let active_row = match active_pair {
                Some((acc, model)) => row()
                    .w_full()
                    .px(12.0)
                    .py(10.0)
                    .radius(ui::R_SM)
                    .fill(u.muted)
                    .border(1.0, u.line)
                    .gap(8.0)
                    .center_y()
                    .child(widgets::dot(u.ok))
                    .child(
                        text(format!("Ativo agora: conta {acc}"))
                            .font(ui::SMALL)
                            .color(u.content),
                    )
                    .child(Badge::new(model).variant(BadgeVariant::Subtle).build(&kit)),
                None => row()
                    .w_full()
                    .px(12.0)
                    .py(10.0)
                    .radius(ui::R_SM)
                    .fill(u.muted)
                    .border(1.0, u.line)
                    .gap(8.0)
                    .center_y()
                    .child(widgets::dot(u.idle))
                    .child(
                        text("Ativo agora: nenhuma rota registrada nas últimas 24h")
                            .font(ui::SMALL)
                            .color(u.muted_text),
                    ),
            };

            let mut seen = HashSet::new();
            let mut rows = col().w_full().gap(6.0);
            let mut idx = 0usize;
            for acc in &p.accounts {
                seen.insert(acc.id.clone());
                let st = sum.accounts.iter().find(|x| x.account_id == acc.id);
                rows = rows.child(account_usage_row(
                    UsageRow {
                        idx,
                        account_id: &acc.id,
                        provider: &acc.provider,
                        enabled: acc.enabled,
                        quota_tokens: acc.quota_tokens,
                        stats: st,
                        now,
                    },
                    u,
                ));
                idx += 1;
            }
            for st in &sum.accounts {
                if !seen.insert(st.account_id.clone()) {
                    continue;
                }
                let prov = if st.provider.is_empty() {
                    "—"
                } else {
                    st.provider.as_str()
                };
                rows = rows.child(account_usage_row(
                    UsageRow {
                        idx,
                        account_id: &st.account_id,
                        provider: prov,
                        enabled: true,
                        quota_tokens: None,
                        stats: Some(st),
                        now,
                    },
                    u,
                ));
                idx += 1;
            }
            if idx == 0 {
                rows = rows.child(
                    text("Nenhum consumo ou conta registrada na janela de 24h.")
                        .font(ui::SMALL)
                        .color(u.muted_text),
                );
            }

            let consumed_total = sum
                .total_prompt_tokens
                .saturating_add(sum.total_completion_tokens);
            let saver_line = format!(
                "Consumo (24h): {} tok ({} req) · Economia de tokens: {} tok (saídas de ferramentas: {} · código enxuto: {} · respostas curtas: {})",
                agentwork::compact_count(consumed_total),
                sum.total_requests,
                agentwork::compact_count(sum.total_saved_tokens),
                agentwork::compact_count(sum.total_saved_rtk_tokens),
                agentwork::compact_count(sum.total_saved_ponytail_tokens),
                agentwork::compact_count(sum.total_saved_caveman_tokens),
            );

            col()
                .w_full()
                .gap(12.0)
                .child(active_row)
                .child(rows)
                .child(
                    row()
                        .w_full()
                        .px(12.0)
                        .py(10.0)
                        .gap(8.0)
                        .center_y()
                        .radius(ui::R_SM)
                        .fill(u.muted)
                        .border(1.0, u.line)
                        .child(icon(ui::icon_settings(), 14.0).color(u.accent_text))
                        .child(
                            text(saver_line)
                                .wrap()
                                .font(ui::SMALL)
                                .color(u.secondary)
                                .selectable(true)
                                .grow(1.0)
                                .min_w(0.0),
                        ),
                )
        }
    };

    col()
        .key("usage-section")
        .w_full()
        .max_w(780.0)
        .pb(16.0)
        .gap(14.0)
        .child(header)
        .child(body)
}
