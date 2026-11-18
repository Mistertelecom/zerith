//! The `Contas` tab: gateway accounts, failover by priority and quota.

use rune::{col, el, row, text, El};
use rune_kit::Switch;
use zerith_core::settings::AuthMode;
use zerith_providers::OAuthTool;

use super::actions::{
    clear_cooldown, delete_account, new_account, now_ms, oauth_login, save_account, select_account,
    toggle_account_enabled,
};
use super::notice_line;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, hint, labelled, Tone};
use crate::wire;
use crate::Shell;

pub fn view(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let now = now_ms();
    let usage_accounts = p
        .usage
        .ready()
        .map(|sum| sum.accounts.as_slice())
        .unwrap_or(&[]);

    let mut list = col()
        .key("account-list")
        .role("list")
        .label("Contas para revezar")
        .w(if u.compact { 10_000.0 } else { 260.0 })
        .max_w(if u.compact { 580.0 } else { 260.0 })
        .padding(14.0)
        .gap(8.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(eyebrow("Contas cadastradas", u));

    if p.accounts.is_empty() {
        list = list.child(
            col()
                .w_full()
                .padding(12.0)
                .gap(6.0)
                .radius(ui::R_SM)
                .fill(u.surface)
                .border(1.0, u.line)
                .child(
                    text("Nenhuma conta ainda")
                        .font(ui::SMALL)
                        .color(u.content),
                )
                .child(
                    text("Com várias contas do mesmo provedor, o Zerith troca sozinho quando uma atinge a cota.")
                        .wrap()
                        .font(ui::LABEL)
                        .color(u.muted_text),
                ),
        );
    } else {
        for (i, acc) in p.accounts.iter().enumerate() {
            let on = p.selected_account == Some(i);
            let id_toggle = acc.id.clone();
            let id_cooldown = acc.id.clone();
            let in_cooldown = usage_accounts
                .iter()
                .find(|st| st.account_id == acc.id)
                .is_some_and(|st| st.cooldown_until > now);
            let mut card = col()
                .key(format!("acc-{i}"))
                .w_full()
                .py(8.0)
                .px(10.0)
                .gap(6.0)
                .radius(ui::R_SM)
                .fill(if on { u.selected } else { u.surface })
                .border(1.0, if on { u.accent.alpha(0.45) } else { u.line })
                .hover(u.hover)
                .role("button")
                .label(format!("Conta {} ({})", acc.id, acc.provider))
                .checked(on)
                .focusable(true)
                .on_press(move |s: &mut Shell| select_account(s, i))
                .child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child({
                            let mut info = col()
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(2.0)
                                .child(
                                    text(acc.id.clone())
                                        .font(ui::BODY)
                                        .color(u.content)
                                        .truncate(),
                                )
                                .child(
                                    text(format!("{} · prioridade {}", acc.provider, acc.priority))
                                        .font(ui::LABEL)
                                        .color(u.faint)
                                        .truncate(),
                                );
                            // An OAuth account says where its credential stands: the
                            // tool and the minutes left, or that it is missing.
                            if acc.auth == AuthMode::Oauth {
                                let (line, colour) = match wire::oauth_status(&acc.id) {
                                    Some((tool, mins)) => (
                                        format!("{} · OAuth · expira em {mins} min", tool.as_str()),
                                        u.ok,
                                    ),
                                    None => (
                                        format!(
                                            "{} · OAuth · sem credencial",
                                            acc.oauth_tool.as_deref().unwrap_or("oauth")
                                        ),
                                        u.warn,
                                    ),
                                };
                                info =
                                    info.child(text(line).font(ui::LABEL).color(colour).truncate());
                            }
                            info
                        })
                        .child(
                            Switch::new(acc.enabled)
                                .label(format!("Ativar conta {}", acc.id))
                                .hide_label()
                                .on_toggle(move |s: &mut Shell| {
                                    toggle_account_enabled(s, id_toggle.clone());
                                })
                                .build(&kit),
                        ),
                );
            if in_cooldown {
                card = card.child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(widgets::dot(u.warn))
                        .child(text("Em cooldown").font(ui::LABEL).color(u.warn).grow(1.0))
                        .child(button(
                            format!("acc-clear-cd-{i}"),
                            "Limpar cooldown",
                            Tone::Plain,
                            true,
                            u,
                            move |s, _| clear_cooldown(s, id_cooldown.clone()),
                        )),
                );
            }
            list = list.child(card);
        }
    }

    let af = &p.account_form;
    let selected_id = p
        .selected_account
        .and_then(|i| p.accounts.get(i))
        .map(|a| a.id.clone());

    let mut actions = row().w_full().gap(8.0).center_y().child(button(
        "acc-save",
        "Salvar conta",
        Tone::Primary,
        true,
        u,
        |s, _| save_account(s),
    ));
    if let Some(sel_id) = selected_id {
        let cd_id = sel_id.clone();
        let del_id = sel_id;
        actions = actions
            .child(button(
                "acc-clear-cooldown",
                "Limpar cooldown",
                Tone::Plain,
                true,
                u,
                move |s, _| clear_cooldown(s, cd_id.clone()),
            ))
            .child(el().grow(1.0))
            .child(button(
                "acc-delete",
                "Remover",
                Tone::Danger,
                true,
                u,
                move |s, _| delete_account(s, del_id.clone()),
            ));
    }

    let form = col()
        .key("account-form")
        .grow(1.0)
        .min_w(0.0)
        .max_w(580.0)
        .padding(18.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .gap(12.0)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .flex_wrap()
                .child(
                    labelled(
                        "ID da conta",
                        field(
                            "acc-id",
                            &af.id,
                            "ID da conta",
                            "ex.: anthropic-trabalho",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.id,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Provedor",
                        field(
                            "acc-provider",
                            &af.provider,
                            "Provedor",
                            "ex.: anthropic",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.provider,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(
                    labelled(
                        "Prioridade",
                        field(
                            "acc-priority",
                            &af.priority,
                            "Prioridade (menor número roda primeiro)",
                            "0",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.priority,
                            |i| i,
                        ),
                        u,
                    )
                    .w(110.0),
                )
                .child(
                    labelled(
                        "Cota de tokens",
                        field(
                            "acc-quota",
                            &af.quota_tokens,
                            "Cota de tokens na janela",
                            "opcional, ex.: 500000",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.quota_tokens,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Janela (horas)",
                        field(
                            "acc-window",
                            &af.window_hours,
                            "Janela em horas",
                            "5",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.window_hours,
                            |i| i,
                        ),
                        u,
                    )
                    .w(100.0),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(
                    labelled(
                        "Modelo padrão",
                        field(
                            "acc-model",
                            &af.default_model,
                            "Modelo padrão da conta",
                            "opcional, ex.: claude-sonnet-4",
                            None,
                            u,
                            |s: &mut Shell| &mut s.providers.account_form.default_model,
                            |i| i,
                        ),
                        u,
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    labelled(
                        "Estado",
                        Switch::new(af.enabled)
                            .label(if af.enabled { "Ativa" } else { "Pausada" })
                            .on_toggle(|s: &mut Shell| {
                                s.providers.account_form.enabled =
                                    !s.providers.account_form.enabled;
                            })
                            .build(&kit),
                        u,
                    )
                    .w(130.0),
                ),
        )
        .child(
            col()
                .w_full()
                .gap(8.0)
                .child(eyebrow("Entrar com assinatura (OAuth)", u))
                .child(hint(
                    "Claude Code, Codex e Antigravity entram pelo navegador; a credencial fica no cofre do sistema e o token nunca vai para o settings.toml. O ID da conta acima é o nome usado.",
                    u,
                ))
                .child({
                    let mut cards = row().w_full().gap(8.0).flex_wrap();
                    for (key, label, sub, tool) in [
                        ("oauth-claude-code", "Claude Code", "assinatura Anthropic", OAuthTool::ClaudeCode),
                        ("oauth-codex", "Codex", "assinatura ChatGPT", OAuthTool::Codex),
                        ("oauth-antigravity", "Antigravity", "conta Google", OAuthTool::Antigravity),
                    ] {
                        cards = cards.child(
                            row()
                                .key(key)
                                .role("button")
                                .label(format!("Entrar com {label}"))
                                .focusable(true)
                                .h(52.0)
                                .pl(8.0)
                                .pr(14.0)
                                .gap(10.0)
                                .center_y()
                                .radius(ui::R_MD)
                                .fill(u.layer2)
                                .border(1.0, u.line_strong)
                                .hover(u.hover)
                                .press(u.active)
                                .on_press(move |s: &mut Shell| oauth_login(s, tool))
                                .child(crate::brand::logo(label, 36.0, &s.images, u))
                                .child(
                                    col()
                                        .gap(1.0)
                                        .child(text(label).font(ui::SMALL).color(u.content))
                                        .child(text(sub).font(ui::LABEL).color(u.faint)),
                                ),
                        );
                    }
                    cards
                }),
        )
        .child(labelled(
            "Chave de API",
            field(
                "acc-key",
                &af.key,
                "Chave de API da conta",
                "deixe vazio para manter a chave atual no cofre",
                None,
                u,
                |s: &mut Shell| &mut s.providers.account_form.key,
                |i| i,
            ),
            u,
        ))
        .child(actions)
        .child(notice_line(&p.account_notice, u));

    col()
        .w_full()
        .max_w(860.0)
        .pb(16.0)
        .gap(14.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(2.0)
                        .child(eyebrow("Contas para revezar", u))
                        .child(hint(
                            "Várias chaves do mesmo provedor: quando uma esgota a cota, a próxima assume. Guardado em [[accounts]].",
                            u,
                        )))
                .child(button(
                    "acc-new",
                    "Nova conta",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| new_account(s),
                )),
        )
        .child(widgets::split(list, form, 20.0, u))
}
