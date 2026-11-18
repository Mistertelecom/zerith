//! Drawing of the Media screen: the connection block, then one section
//! (`Imagem` or `Vídeo`) with the current model, the search and the fixed
//! parameters.

use rune::{col, el, row, rule, text, El, Tx};
use rune_kit::{SegmentedControl, Switch};
use zerith_providers::media::{Kind, MediaModel, ParamKind};

use super::actions::{
    close_picker, edit_key, edit_query, load_more, open_picker, save_key, save_params, search_now,
    set_kind, test_key, use_model,
};
use super::params::summary;
use super::state::{kind_label, KeySource, MediaState, Slot};
use crate::mediaout::Images;
use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, hint, pill, Load, Tone};
use crate::Shell;

const KEYS_URL: &str = "https://openrouter.ai/settings/keys";

fn note(msg: &Option<Result<String, String>>, u: &Ui) -> El {
    match msg {
        Some(Ok(m)) => text(m.clone()).wrap().font(ui::LABEL).color(u.muted_text),
        Some(Err(m)) => widgets::error_line(m.clone(), u),
        None => el(),
    }
}

/// The `Conexão` block: key field, state line, honest notes.
fn connection(m: &MediaState, u: &Ui) -> El {
    let (dot, state_word) = match (&m.key_state, &m.check) {
        (_, Load::Failed(e)) => (u.danger, format!("Erro ao testar: {e}")),
        (_, Load::Ready(k)) => (
            u.ok,
            format!(
                "Conectado{}{}",
                if k.label.is_empty() {
                    String::new()
                } else {
                    format!(" como {}", k.label)
                },
                match (k.usage_usd, k.limit_usd) {
                    (Some(us), Some(l)) => format!(" · gasto US$ {us:.2} de US$ {l:.2}"),
                    (Some(us), None) => format!(" · gasto US$ {us:.2}"),
                    _ => String::new(),
                }
            ),
        ),
        (Load::Ready(Some(KeySource::Env)), _) => (
            u.ok,
            "Chave encontrada na variável OPENROUTER_API_KEY".into(),
        ),
        (Load::Ready(Some(KeySource::Keyring)), _) => (u.ok, "chave salva no cofre".into()),
        (Load::Ready(None), _) => (
            u.idle,
            "Sem chave: dá para escolher modelos, não gerar".into(),
        ),
        (Load::Failed(e), _) => (u.danger, format!("Cofre indisponível: {e}")),
        _ => (u.idle, "Verificando…".into()),
    };
    let has_key = matches!(m.key_state, Load::Ready(Some(_)));
    let show_form = !has_key || m.editing_key;

    let status = row()
        .key("media-state")
        .role("status")
        .label(state_word.clone())
        .w_full()
        .gap(8.0)
        .center_y()
        .child(widgets::dot(dot))
        .child(
            text(state_word)
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .grow(1.0)
                .min_w(0.0)
                .selectable(true),
        );

    let mut actions = row().w_full().gap(8.0).center_y().flex_wrap();
    if show_form {
        let kf = field(
            "media-key",
            &m.key,
            "Chave de API do OpenRouter",
            "sk-or-…",
            None,
            u,
            |s: &mut Shell| &mut s.media.key,
            |i| i.on_submit(|s: &mut Shell, _: &mut Tx| save_key(s)),
        )
        .grow(1.0)
        .min_w(180.0);
        actions = actions.child(kf).child(button(
            "media-key-save",
            if m.saving_key {
                "Salvando…"
            } else {
                "Salvar chave"
            },
            Tone::Primary,
            !m.saving_key,
            u,
            |s, _| save_key(s),
        ));
    } else {
        actions = actions.child(button(
            "media-key-change",
            "Trocar chave",
            Tone::Plain,
            true,
            u,
            edit_key,
        ));
    }
    if has_key {
        actions = actions.child(button(
            "media-key-test",
            if m.check.is_loading() {
                "Testando…"
            } else {
                "Testar"
            },
            Tone::Plain,
            !m.check.is_loading(),
            u,
            |s, _| test_key(s),
        ));
    }

    let link = row()
        .gap(6.0)
        .center_y()
        .child(
            text("Crie uma chave em")
                .font(ui::LABEL)
                .color(u.muted_text),
        )
        .child(
            text("openrouter.ai/settings/keys")
                .key("media-keys-link")
                .font(ui::LABEL)
                .color(u.accent_text)
                .href(KEYS_URL)
                .label("Abrir openrouter.ai/settings/keys no navegador")
                .focusable(true),
        );

    col()
        .key("media-connection")
        .w_full()
        .gap(10.0)
        .child(eyebrow("Conexão com o OpenRouter", u))
        .child(status)
        .child(actions)
        .child(note(&m.key_notice, u))
        .child(link)
        .child(hint(
            "Listar modelos não precisa de chave; só gerar precisa. A chave fica no cofre do sistema, nunca em arquivo, e não é mostrada de novo depois de salva.",
            u,
        ))
        .child(
            row()
                .key("media-cost-warning")
                .role("note")
                .label("Gerar imagem e vídeo custa dinheiro")
                .w_full()
                .gap(8.0)
                .px(12.0)
                .py(8.0)
                .radius(ui::R_SM)
                .fill(u.warn_soft)
                .child(
                    text("Cada geração é cobrada pelo OpenRouter na sua conta. Vídeos custam bem mais que imagens; confira o preço antes de escolher.")
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.warn_text)
                        .grow(1.0)
                        .min_w(0.0),
                ),
        )
}

fn price(m: &MediaModel) -> String {
    if m.price_hint.is_empty() {
        "preço não informado".into()
    } else {
        m.price_hint.clone()
    }
}

/// The model in use: name, id, price and `Trocar`.
fn current(slot: &Slot, im: &Images, u: &Ui) -> El {
    let kind = slot.kind;
    let k = kind.as_str();
    let body = match &slot.current {
        _ if !slot.configured() => col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                text("Nenhum modelo escolhido")
                    .font(ui::BODY)
                    .color(u.content),
            )
            .child(
                text(match kind {
                    Kind::Image => "Sem um modelo, o agente não oferece geração de imagem.",
                    Kind::Video => "Sem um modelo, o agente não oferece geração de vídeo.",
                })
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text),
            ),
        Load::Ready(m) => col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                row()
                    .gap(8.0)
                    .center_y()
                    .child(
                        text(m.name.clone())
                            .font(ui::BODY)
                            .color(u.content)
                            .truncate()
                            .shrink()
                            .min_w(0.0),
                    )
                    .child(pill(price(m), u)),
            )
            .child(widgets::code_line(m.id.clone(), u)),
        Load::Failed(e) => col()
            .grow(1.0)
            .min_w(0.0)
            .gap(4.0)
            .child(text(slot.model_id.clone()).font(ui::BODY).color(u.content))
            .child(widgets::error_line(
                format!("Não consegui ler os detalhes: {e}"),
                u,
            )),
        _ => col()
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(text(slot.model_id.clone()).font(ui::BODY).color(u.content))
            .child(text("Lendo detalhes…").font(ui::SMALL).color(u.muted_text)),
    };
    row()
        .key(format!("media-{k}-current"))
        .role("group")
        .label(if slot.configured() {
            format!("Modelo de {} atual: {}", kind_label(kind), slot.model_id)
        } else {
            format!("Modelo de {}: nenhum", kind_label(kind))
        })
        .w_full()
        .gap(12.0)
        .child(if slot.configured() {
            crate::brand::logo(&slot.model_id, 40.0, im, u)
        } else {
            rune::el()
        })
        .px(14.0)
        .py(12.0)
        .center_y()
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(body)
        .child(button(
            format!("media-{k}-change"),
            if slot.picking {
                "Fechar busca"
            } else if slot.configured() {
                "Trocar"
            } else {
                "Escolher modelo"
            },
            if slot.configured() {
                Tone::Plain
            } else {
                Tone::Primary
            },
            true,
            u,
            move |s, tx| {
                if s.media.slot(kind).picking {
                    close_picker(s, kind)
                } else {
                    open_picker(s, kind, tx)
                }
            },
        ))
}

fn result_row(i: usize, m: &MediaModel, is_current: bool, im: &Images, u: &Ui) -> El {
    let k = m.kind.as_str();
    let model = m.clone();
    let desc: String = m
        .description
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(140)
        .collect();
    let mut head = row().gap(8.0).center_y().child(
        text(m.name.clone())
            .font(ui::BODY)
            .color(u.content)
            .truncate()
            .shrink()
            .min_w(0.0),
    );
    head = head.child(pill(price(m), u));
    if is_current {
        head = head.child(pill("em uso", u));
    }
    let mut words = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(1.0)
        .child(head)
        .child(text(m.id.clone()).font(ui::MONO).color(u.faint).truncate());
    if !desc.is_empty() {
        words = words.child(
            text(desc)
                .font(ui::SMALL)
                .color(u.muted_text)
                .truncate()
                .a11y_hidden(),
        );
    }
    row()
        .key(format!("media-{k}-row-{i}"))
        .role("listitem")
        .label(format!(
            "{}, {}{}",
            m.name,
            price(m),
            if is_current { ", em uso" } else { "" }
        ))
        .position_in_set(i + 1, 0)
        .w_full()
        .py(8.0)
        .gap(12.0)
        .center_y()
        .child(crate::brand::logo(&m.id, 32.0, im, u))
        .child(words)
        .child(button(
            format!("media-{k}-use-{i}"),
            if is_current { "Em uso" } else { "Usar" },
            if is_current {
                Tone::Plain
            } else {
                Tone::Primary
            },
            !is_current,
            u,
            move |s, _| use_model(s, model.clone()),
        ))
}

/// Search bar, results and `Carregar mais`.
fn picker(slot: &Slot, im: &Images, u: &Ui) -> El {
    let kind = slot.kind;
    let k = kind.as_str();
    let qkey: &'static str = if kind == Kind::Image {
        "media-image-q"
    } else {
        "media-video-q"
    };
    let bar = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            field(
                qkey,
                &slot.query,
                match kind {
                    Kind::Image => "Buscar modelo de imagem",
                    Kind::Video => "Buscar modelo de vídeo",
                },
                "Buscar por nome ou id (ex.: seedream, veo)",
                Some(ui::icon_search()),
                u,
                move |s: &mut Shell| &mut s.media.slot_mut(kind).query,
                move |i| {
                    i.on_input_change(move |s: &mut Shell, _: &mut Tx| edit_query(s, kind))
                        .on_submit(move |s: &mut Shell, _: &mut Tx| search_now(s, kind))
                },
            )
            .grow(1.0)
            .min_w(0.0),
        )
        .child(button(
            format!("media-{k}-search"),
            "Buscar",
            Tone::Plain,
            !slot.status.is_loading(),
            u,
            move |s, _| search_now(s, kind),
        ));

    let mut list = col()
        .key(format!("media-{k}-results"))
        .role("list")
        .label(format!("Modelos de {}", kind_label(kind)))
        .w_full();
    for (i, m) in slot.rows.iter().enumerate() {
        if i > 0 {
            list = list.child(rule(1.0, u.line).w_full());
        }
        list = list.child(result_row(i, m, m.id == slot.model_id, im, u));
    }

    let state = match &slot.status {
        Load::Loading if slot.rows.is_empty() => widgets::loading("Buscando modelos", u),
        Load::Failed(e) => col()
            .w_full()
            .gap(8.0)
            .child(widgets::error_line(format!("Não consegui buscar: {e}"), u))
            .child(row().child(button(
                format!("media-{k}-retry"),
                "Tentar de novo",
                Tone::Plain,
                true,
                u,
                move |s, _| {
                    if s.media.slot(kind).pages == 0 {
                        search_now(s, kind)
                    } else {
                        load_more(s, kind)
                    }
                },
            ))),
        Load::Ready(()) if slot.rows.is_empty() => widgets::notice(
            "Nenhum modelo encontrado",
            match slot.applied.as_deref() {
                Some(q) if !q.is_empty() => format!("Nada combina com “{q}”. Tente outra palavra."),
                _ => "O catálogo veio vazio.".into(),
            },
            u,
        ),
        _ => el(),
    };

    let mut foot = row().w_full().gap(10.0).center_y();
    if slot.has_more() {
        foot = foot.child(button(
            format!("media-{k}-more"),
            if slot.status.is_loading() {
                "Carregando…"
            } else {
                "Carregar mais"
            },
            Tone::Plain,
            !slot.status.is_loading(),
            u,
            move |s, _| load_more(s, kind),
        ));
    }
    if slot.total > 0 {
        foot = foot.child(
            text(format!("{} de {} modelos", slot.rows.len(), slot.total))
                .role("status")
                .font(ui::LABEL)
                .color(u.muted_text),
        );
    }

    // The list scrolls inside the section so a long catalogue never pushes the
    // parameters out of reach.
    let scroller = col()
        .key(format!("media-{k}-scroll"))
        .w_full()
        .max_h(300.0)
        .child(list)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(12.0);
    col()
        .key(format!("media-{k}-picker"))
        .w_full()
        .gap(10.0)
        .child(bar)
        .child(scroller)
        .child(state)
        .child(foot)
}

/// `Parâmetros fixos`: a control per parameter the model accepts.
fn params_form(slot: &Slot, u: &Ui) -> El {
    let kind = slot.kind;
    let k = kind.as_str();
    let kit = ui::kit(u);
    let head = col()
        .w_full()
        .gap(2.0)
        .child(eyebrow("Parâmetros fixos", u))
        .child(hint(
            "Valem em toda geração deste tipo. O agente escreve só o pedido; não muda estes valores.",
            u,
        ));
    let Some(model) = slot.current.ready() else {
        return col().w_full().gap(8.0).child(head).child(
            text(if slot.configured() {
                "Os parâmetros aparecem quando os detalhes do modelo carregam."
            } else {
                "Escolha um modelo para ver os parâmetros que ele aceita."
            })
            .font(ui::SMALL)
            .color(u.muted_text),
        );
    };
    let mut c = col()
        .key(format!("media-{k}-params"))
        .w_full()
        .gap(12.0)
        .child(head);
    for n in &slot.notes {
        c = c.child(
            row()
                .role("note")
                .label(n.clone())
                .w_full()
                .gap(8.0)
                .px(12.0)
                .py(8.0)
                .radius(ui::R_SM)
                .fill(u.warn_soft)
                .child(
                    text(n.clone())
                        .wrap()
                        .font(ui::SMALL)
                        .color(u.warn_text)
                        .grow(1.0)
                        .min_w(0.0),
                ),
        );
    }
    let mut grid = row().w_full().gap(16.0).flex_wrap();
    for (i, it) in slot.draft.items.iter().enumerate() {
        let name = it.name.clone();
        let control: El = match &it.kind {
            ParamKind::Enum(opts) => {
                let idx = it
                    .choice
                    .as_ref()
                    .and_then(|c| opts.iter().position(|o| o == c));
                let values = opts.clone();
                let seg_labels: Vec<&str> = opts.iter().map(String::as_str).collect();
                if opts.len() <= 5 {
                    SegmentedControl::new(&seg_labels, idx)
                        .label(name.clone())
                        .fit()
                        .on_pick(move |s: &mut Shell, p: usize, _: &mut Tx| {
                            if let Some(it) = s.media.slot_mut(kind).draft.items.get_mut(i) {
                                it.choice = values.get(p).cloned();
                            }
                        })
                        .build(&kit)
                } else {
                    // Many options: a wrapped row of chips, one per value.
                    let mut r = row().gap(6.0).flex_wrap().max_w(420.0);
                    for (p, o) in opts.iter().enumerate() {
                        let on = idx == Some(p);
                        let values = values.clone();
                        r = r.child(
                            row()
                                .key(format!("media-{k}-p{i}-{p}"))
                                .role("radio")
                                .label(format!("{name}: {o}"))
                                .checked(on)
                                .focusable(true)
                                .h(26.0)
                                .px(10.0)
                                .center()
                                .radius(13.0)
                                .border(1.0, if on { u.accent } else { u.line })
                                .fill(if on {
                                    u.accent_soft
                                } else {
                                    rune::Color::TRANSPARENT
                                })
                                .hover(u.hover)
                                .on_press(move |s: &mut Shell| {
                                    if let Some(it) = s.media.slot_mut(kind).draft.items.get_mut(i)
                                    {
                                        it.choice = values.get(p).cloned();
                                    }
                                })
                                .child(text(o.clone()).font(ui::LABEL).color(if on {
                                    u.accent_text
                                } else {
                                    u.secondary
                                })),
                        );
                    }
                    r
                }
            }
            ParamKind::Flag => Switch::new(it.flag.unwrap_or(false))
                .label(name.clone())
                .hide_label()
                .on_toggle(move |s: &mut Shell| {
                    if let Some(it) = s.media.slot_mut(kind).draft.items.get_mut(i) {
                        it.flag = Some(!it.flag.unwrap_or(false));
                    }
                })
                .build(&kit),
            ParamKind::Range { min, max } => field(
                format!("media-{k}-pf{i}"),
                &it.edit,
                format!("{name} (de {min} a {max})"),
                format!("{min} – {max}"),
                None,
                u,
                move |s: &mut Shell| s.media.slot_mut(kind).draft.edit_mut(i),
                |x| x,
            )
            .w(150.0),
            _ => field(
                format!("media-{k}-pf{i}"),
                &it.edit,
                name.clone(),
                "opcional",
                None,
                u,
                move |s: &mut Shell| s.media.slot_mut(kind).draft.edit_mut(i),
                |x| x,
            )
            .w(180.0),
        };
        let caption = match &it.kind {
            ParamKind::Range { min, max } => format!("{name}  ·  {min} a {max}"),
            _ => name.clone(),
        };
        grid = grid.child(
            col()
                .gap(6.0)
                .child(text(caption).font(ui::LABEL).color(u.muted_text))
                .child(control),
        );
    }
    if slot.draft.items.is_empty() {
        c = c.child(
            text("Este modelo não tem parâmetros ajustáveis.")
                .font(ui::SMALL)
                .color(u.muted_text),
        );
    } else {
        c = c.child(grid);
    }
    let dirty = slot.draft.dirty(model, &slot.saved);
    c.child(
        row()
            .key(format!("media-{k}-summary"))
            .role("status")
            .label(format!("Salvo: {}", summary(&slot.saved)))
            .w_full()
            .gap(8.0)
            .center_y()
            .child(text("Salvo:").font(ui::LABEL).color(u.muted_text))
            .child(
                text(summary(&slot.saved))
                    .font(ui::MONO)
                    .color(u.secondary)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0)
                    .selectable(true),
            ),
    )
    .child(
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(button(
                format!("media-{k}-params-save"),
                "Salvar parâmetros",
                Tone::Primary,
                dirty,
                u,
                move |s, _| save_params(s, kind),
            ))
            .child(note(&slot.notice, u)),
    )
}

fn section(slot: &Slot, im: &Images, u: &Ui) -> El {
    let mut c = col()
        .key(format!("media-{}-section", slot.kind.as_str()))
        .w_full()
        .gap(14.0)
        .child(current(slot, im, u));
    if slot.picking {
        c = c.child(picker(slot, im, u));
    }
    c.child(params_form(slot, u))
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let m = &s.media;
    let idx = usize::from(m.kind == Kind::Video);
    let tabs = widgets::tabs(
        &[kind_label(Kind::Image), kind_label(Kind::Video)],
        idx,
        "Tipo de mídia",
        u,
        |s, i| set_kind(s, if i == 0 { Kind::Image } else { Kind::Video }),
    );
    let body = col()
        .key("media-body")
        .w_full()
        .max_w(760.0)
        .gap(20.0)
        .pb(24.0)
        .child(connection(m, u))
        .child(rule(1.0, u.line).w_full())
        .child(tabs)
        .child(section(m.slot(m.kind), &s.images, u));
    widgets::page(
        "Mídia",
        el(),
        widgets::scroll_body("media-scroll", body, u),
        u,
    )
}
