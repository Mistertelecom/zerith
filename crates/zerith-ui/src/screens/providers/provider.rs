//! The `Provedor` tab: the list of endpoints and the form of the selected one.

use rune::{col, el, row, text, El, Tx};
use rune_kit::SegmentedControl;
use zerith_providers::{Probe, Protocol};

use super::actions::{apply, apply_kind_preset, select, test};
use super::notice_line;
use super::state::PRESETS;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, hint, labelled, Load, Tone};
use crate::Shell;

fn probe_line(p: &Load<Probe>, u: &Ui) -> El {
    match p {
        Load::Idle => el(),
        Load::Loading => row()
            .gap(8.0)
            .center_y()
            .role("status")
            .label("Testando conexão")
            .child(widgets::dot(u.warn))
            .child(text("Testando…").font(ui::SMALL).color(u.muted_text)),
        Load::Failed(e) => widgets::error_line(e.clone(), u),
        Load::Ready(p) => {
            let (color, head) = if p.ok {
                (u.ok, format!("Conectado em {} ms", p.latency_ms))
            } else if p.status == 0 {
                (u.danger, format!("Sem resposta ({} ms)", p.latency_ms))
            } else {
                (u.warn, format!("HTTP {} em {} ms", p.status, p.latency_ms))
            };
            let detail: String = p.detail.chars().take(240).collect();
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .role("status")
                .label(format!("{head}. {detail}"))
                .child(widgets::dot(color))
                .child(text(head).font(ui::SMALL).color(u.content))
                .child(
                    text(detail)
                        .font(ui::SMALL)
                        .color(u.muted_text)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0)
                        .selectable(true),
                )
        }
    }
}

/// The endpoints: a column on the left or, in a narrow window, wrapped pills
/// above the form.
fn list(s: &Shell, u: &Ui) -> El {
    let p = &s.providers;
    let mut list = if u.compact {
        row().w_full().gap(6.0).flex_wrap()
    } else {
        col().w_full().gap(2.0)
    }
    .key("provider-list")
    .role("list")
    .label("Provedores");
    for (i, c) in p.list.iter().enumerate() {
        let on = i == p.selected;
        let item = col()
            .key(format!("prov-{i}"))
            .py(8.0)
            .px(10.0)
            .gap(2.0)
            .radius(ui::R_SM)
            .fill(if on { u.selected } else { u.surface })
            .border(1.0, if on { u.accent.alpha(0.45) } else { u.line })
            .hover(u.hover)
            .role("button")
            .label(c.name.clone())
            .checked(on)
            .focusable(true)
            .on_press(move |s: &mut Shell| select(s, i));
        let name = text(c.name.clone()).font(ui::BODY).color(u.content);
        let mark = crate::brand::logo(&c.name, 28.0, &s.images, u);
        let item = if u.compact {
            item.child(row().gap(8.0).center_y().child(mark).child(name))
        } else {
            item.w_full().child(
                row().w_full().gap(10.0).center_y().child(mark).child(
                    col().grow(1.0).min_w(0.0).gap(1.0).child(name).child(
                        text(c.base_url.clone())
                            .font(ui::LABEL)
                            .color(u.faint)
                            .truncate(),
                    ),
                ),
            )
        };
        list = list.child(item);
    }
    let panel = col()
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(eyebrow("Perfis salvos", u))
        .child(list)
        .child(hint(
            "O primeiro pode vir do ambiente (ZERITH_*); os demais vêm do settings.toml.",
            u,
        ));
    if u.compact {
        panel
    } else {
        panel.w(248.0)
    }
}

fn form(s: &Shell, u: &Ui) -> El {
    let kit = ui::kit(u);
    let p = &s.providers;
    let f = &p.form;
    let mut presets = row().w_full().gap(8.0).flex_wrap();
    for preset in PRESETS {
        let kind = preset.kind;
        let on = f.name.text.eq_ignore_ascii_case(kind.as_str());
        presets = presets.child(
            row()
                .key(preset.key)
                .role("button")
                .label(format!("Preencher com {}", preset.label))
                .checked(on)
                .focusable(true)
                .h(40.0)
                .pl(6.0)
                .pr(12.0)
                .gap(8.0)
                .center_y()
                .radius(ui::R_MD)
                .fill(if on { u.selected } else { u.layer2 })
                .border(
                    1.0,
                    if on {
                        u.accent.alpha(0.55)
                    } else {
                        u.line_strong
                    },
                )
                .hover(u.hover)
                .press(u.active)
                .on_press(move |s: &mut Shell| apply_kind_preset(s, kind))
                .child(crate::brand::logo(preset.label, 28.0, &s.images, u))
                .child(text(preset.label).font(ui::SMALL).color(u.content)),
        );
    }
    let proto_idx = match f.protocol {
        Protocol::OpenAi | Protocol::Responses => 0,
        Protocol::Anthropic => 1,
    };
    let name = labelled(
        "Nome",
        field(
            "prov-name",
            &f.name,
            "Nome",
            "anthropic",
            None,
            u,
            |s: &mut Shell| &mut s.providers.form.name,
            |i| i,
        ),
        u,
    )
    .grow(1.0)
    .min_w(0.0);
    let protocol = col().gap(6.0).child(eyebrow("Protocolo", u)).child(
        SegmentedControl::new(&["OpenAI", "Anthropic"], Some(proto_idx))
            .label("Protocolo")
            .fit()
            .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                s.providers.form.protocol = [Protocol::OpenAi, Protocol::Anthropic][i];
            })
            .build(&kit),
    );
    let model = labelled(
        "Modelo padrão",
        field(
            "prov-model",
            &f.model,
            "Modelo padrão",
            f.model_placeholder,
            None,
            u,
            |s: &mut Shell| &mut s.providers.form.model,
            |i| i,
        ),
        u,
    )
    .grow(1.0)
    .min_w(0.0);
    let timeout = labelled(
        "Timeout (ms)",
        field(
            "prov-timeout",
            &f.timeout,
            "Timeout em milissegundos",
            "60000",
            None,
            u,
            |s: &mut Shell| &mut s.providers.form.timeout,
            |i| i,
        ),
        u,
    );
    let timeout = if u.compact { timeout } else { timeout.w(140.0) };

    col()
        .key("provider-form")
        .grow(1.0)
        .min_w(0.0)
        .max_w(580.0)
        .padding(18.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .gap(12.0)
        .child(labelled("Presets", presets, u))
        .child(widgets::split(name, protocol, 12.0, u))
        .child(labelled(
            "URL do endpoint",
            field(
                "prov-url",
                &f.url,
                "URL do endpoint",
                "https://api.anthropic.com/v1",
                None,
                u,
                |s: &mut Shell| &mut s.providers.form.url,
                |i| i,
            ),
            u,
        ))
        .child(widgets::split(model, timeout, 12.0, u))
        .child(labelled(
            "Chave de API",
            field(
                "prov-key",
                &f.key,
                "Chave de API",
                "não definida",
                None,
                u,
                |s: &mut Shell| &mut s.providers.form.key,
                |i| i,
            ),
            u,
        ))
        .child(hint(
            "A chave vai para o cofre do sistema (Credential Manager ou Keychain), nunca para um arquivo.",
            u,
        ))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(button(
                    "prov-test",
                    if p.probe.is_loading() {
                        "Testando…"
                    } else {
                        "Testar conexão"
                    },
                    Tone::Plain,
                    !p.probe.is_loading(),
                    u,
                    |s, _| test(s),
                ))
                .child(button(
                    "prov-apply",
                    "Aplicar",
                    Tone::Primary,
                    true,
                    u,
                    |s, _| apply(s),
                )),
        )
        .child(probe_line(&p.probe, u))
        .child(notice_line(&p.notice, u))
}

pub fn view(s: &Shell, u: &Ui) -> El {
    col()
        .key("provider-tab")
        .w_full()
        .max_w(860.0)
        .pb(16.0)
        .child(widgets::split(list(s, u), form(s, u), 20.0, u))
}
