//! The composer: input row, `/` and `@` popups, and the footer (model,
//! agent, permission mode, context meter, turn cost).

use rune::{col, el, row, text, text_input, El, GrowthSpec, Key, KeyEvent, Tx};
use rune_kit::{popover, Placement, Select};

use super::actions::{self, Rows};
use super::{toggle_agents, toggle_models, Shell};
use crate::agentwork::{self, Fill, PermissionMode, Trigger};
use crate::ui::{self, Ui};
use crate::widgets::{self, Load};

/// Anchor of both popups and of the field's frame.
const BOX: &str = "composer-box";

fn model_select(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let mut options: Vec<String> = match &c.models {
        Load::Ready(m) => m.clone(),
        _ => Vec::new(),
    };
    if !c.model.is_empty() && !options.contains(&c.model) {
        options.insert(0, c.model.clone());
    }
    let placeholder = match &c.models {
        _ if !super::provider_usable(s) => "conecte um provedor",
        Load::Loading => "carregando modelos…",
        Load::Failed(_) => "modelos indisponíveis",
        _ => "sem modelo",
    };
    Select::new("model", &options)
        .label("Modelo")
        .placeholder(placeholder)
        .current((!c.model.is_empty()).then_some(c.model.as_str()))
        .open(c.model_menu && !options.is_empty())
        .placement(Placement::TopStart)
        .on_toggle(toggle_models)
        .on_close(|s: &mut Shell| s.chat.model_menu = false)
        .on_choose(|s: &mut Shell, v: &str, _: &mut Tx| {
            s.chat.model = v.to_string();
            s.chat.model_menu = false;
            s.chat.price = crate::wire::model_price(&s.providers.active(), v);
        })
        .build(&ui::kit(u), layer)
}

const DEFAULT_AGENT: &str = "Agente padrão";

fn agent_select(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let mut options = vec![DEFAULT_AGENT.to_string()];
    if let Load::Ready(v) = &s.agents.list {
        options.extend(v.iter().map(|a| a.name.clone()));
    }
    if let Some(a) = &s.chat.agent {
        if !options.contains(a) {
            options.push(a.clone());
        }
    }
    Select::new("agent", &options)
        .label("Agente")
        .current(Some(s.chat.agent.as_deref().unwrap_or(DEFAULT_AGENT)))
        .open(s.chat.agent_menu)
        .placement(Placement::TopStart)
        .on_toggle(toggle_agents)
        .on_close(|s: &mut Shell| s.chat.agent_menu = false)
        .on_choose(|s: &mut Shell, v: &str, _: &mut Tx| {
            s.chat.agent = (v != DEFAULT_AGENT).then(|| v.to_string());
            s.chat.agent_menu = false;
        })
        .build(&ui::kit(u), layer)
}

/// The mode chip: the current mode, a popup to pick another. Shift+Tab in the
/// field cycles it too.
fn mode_select(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let labels: Vec<&str> = PermissionMode::ALL.iter().map(|m| m.label()).collect();
    let details: Vec<Option<&str>> = PermissionMode::ALL
        .iter()
        .map(|m| Some(m.detail()))
        .collect();
    let current = s.chat.permission_mode();
    Select::new("mode", &labels)
        .details(&details)
        .label("Modo de permissão (Shift+Tab alterna)")
        .current(Some(current.label()))
        .open(s.chat.mode_menu)
        .placement(Placement::TopStart)
        .on_toggle(|s: &mut Shell| s.chat.mode_menu = !s.chat.mode_menu)
        .on_close(|s: &mut Shell| s.chat.mode_menu = false)
        .on_choose(|s: &mut Shell, v: &str, _: &mut Tx| {
            if let Some(m) = PermissionMode::ALL.iter().find(|m| m.label() == v) {
                actions::set_mode(s, *m);
            }
        })
        .build(&ui::kit(u), layer)
}

/// Tokens in the window: a 3 pt bar and "42k / 128k". Amber past 80 %.
fn meter(s: &Shell, u: &Ui) -> Option<El> {
    let m = s.chat.meter(s.providers.active().context_limit)?;
    let ink = match m.fill() {
        Fill::Ok => u.muted_text,
        Fill::Warn => u.warn,
        Fill::Full => u.danger,
    };
    let mut r = row()
        .key("context")
        .role("progressbar")
        .label(format!("Contexto: {}", m.label()))
        .gap(8.0)
        .center_y();
    if let Some(f) = m.fraction() {
        r = r.range_value(f as f64 * 100.0, 0.0, 100.0).child(
            row()
                .w(48.0)
                .h(3.0)
                .radius(1.5)
                .fill(u.layer3)
                .child(el().w(48.0 * f).h(3.0).radius(1.5).fill(ink))
                .a11y_hidden(),
        );
    }
    Some(r.child(text(m.label()).font(ui::LABEL).color(ink).a11y_hidden()))
}

/// The last turn's cost, only when the price of the model is known.
fn cost(s: &Shell, u: &Ui) -> Option<El> {
    let c = agentwork::turn_cost(s.chat.usage?, s.chat.price)?;
    Some(
        text(agentwork::format_cost(c))
            .key("cost")
            .font(ui::LABEL)
            .color(u.faint),
    )
}

fn footer(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let mut foot = row()
        .w_full()
        .h(28.0)
        .gap(4.0)
        .center_y()
        .child(model_select(s, u, layer))
        .child(agent_select(s, u, layer))
        .child(mode_select(s, u, layer));
    // While the empty screen already displays the setup card (e.g. missing API
    // key), repeating a raw HTTP 401 line in the composer footer only adds noise.
    let suppress_model_err = c.lines.is_empty() && !super::provider_usable(s);
    if !suppress_model_err {
        if let Load::Failed(e) = &c.models {
            foot = foot.child(
                text(e.clone())
                    .font(ui::LABEL)
                    .color(u.faint)
                    .truncate()
                    .shrink()
                    .min_w(0.0),
            );
        }
    }
    foot = foot.child(el().grow(1.0));
    if !c.status.is_empty() {
        foot = foot.child(
            text(c.status.clone())
                .role("status")
                .font(ui::LABEL)
                .color(u.faint)
                .truncate()
                .shrink()
                .min_w(0.0),
        );
    }
    if c.compacting {
        foot = foot.child(text("Compactando…").font(ui::LABEL).color(u.muted_text));
    }
    if c.saved_tokens > 0 {
        foot = foot.child(widgets::pill(
            format!("-{} tok", agentwork::compact_count(c.saved_tokens)),
            u,
        ));
    }
    if let Some(m) = meter(s, u) {
        foot = foot.child(m);
    }
    if let Some(k) = cost(s, u) {
        foot = foot.child(k);
    }
    foot.px(2.0)
}

/// One row of a popup: a command or a file.
struct PopRow {
    id: String,
    label: String,
    detail: String,
}

/// The accessible key of option `id` in popup `key`.
fn option_key(key: &str, id: &str) -> String {
    format!("{key}-option-{id}")
}

/// What the `/` or `@` popup shows right now, if it is open.
struct Pop {
    key: &'static str,
    label: &'static str,
    empty: String,
    rows: Vec<PopRow>,
    active: Option<usize>,
    trigger: Trigger,
    anchor: agentwork::Active,
}

fn pop(s: &Shell) -> Option<Pop> {
    let a = actions::active(s)?;
    s.chat.draft.focused.then_some(())?;
    let (key, label, empty, rows): (&str, &str, String, Vec<PopRow>) = match actions::rows(s, &a) {
        Rows::Slash(v) => (
            "slash",
            "Comandos",
            "Nenhum comando".into(),
            v.iter()
                .map(|c| PopRow {
                    id: c.name.to_string(),
                    label: format!("/{}", c.name),
                    detail: c.detail.to_string(),
                })
                .collect(),
        ),
        Rows::Files(v) => (
            "files",
            "Arquivos",
            match &s.chat.files {
                Load::Loading => "Listando arquivos…".into(),
                Load::Failed(e) => format!("Não consegui listar: {e}"),
                _ => "Nenhum arquivo".into(),
            },
            v.iter()
                .map(|p| {
                    let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
                    PopRow {
                        id: p.clone(),
                        label: name.to_string(),
                        detail: dir.to_string(),
                    }
                })
                .collect(),
        ),
    };
    let n = rows.len();
    Some(Pop {
        key: if key == "slash" { "slash" } else { "files" },
        label: if label == "Comandos" {
            "Comandos"
        } else {
            "Arquivos"
        },
        empty,
        active: (n > 0).then(|| s.chat.popup_active.min(n - 1)),
        trigger: a.trigger,
        rows,
        anchor: a,
    })
}

/// The `/` and `@` popups, anchored above the input frame.
///
/// A combobox popup: the field keeps keyboard focus (Up, Down, Enter, Tab and
/// Esc are handled in `on_field_key`), the container is a labelled listbox and
/// each row an option, and the field points at the highlighted option with
/// `active_descendant`, so a screen reader reads the row as it moves. Built
/// from the kit's public popover pieces rather than `PopupList`, whose
/// container announces itself as an unnamed button.
fn popup(s: &Shell, u: &Ui, layer: &mut Vec<El>) {
    let Some(p) = pop(s) else { return };
    let k = ui::kit(u);
    let n = p.rows.len();
    let mut rows = col()
        .key("rows")
        .w_full()
        .growth(GrowthSpec::default())
        .virtualize(28.0)
        .reveal_selected(true);
    for (i, r) in p.rows.iter().enumerate() {
        let on = p.active == Some(i);
        let (trigger, anchor, id) = (p.trigger, p.anchor.clone(), r.id.clone());
        let mut item = row()
            .key(option_key(p.key, &r.id))
            .role("option")
            .label(r.label.clone())
            .description(r.detail.clone())
            .position_in_set(i + 1, n)
            .focus_selected(on)
            .checked(on)
            .w_full()
            .h(28.0)
            .px(8.0)
            .gap(8.0)
            .center_y()
            .radius(ui::R_XS)
            .fill(if on {
                u.hover
            } else {
                rune::Color::TRANSPARENT
            })
            .on_hover(move |s: &mut Shell| s.chat.popup_active = i)
            .on_press_tx(move |s: &mut Shell, _: &mut Tx| match trigger {
                Trigger::Slash => actions::run_command(s, &id),
                Trigger::File => actions::insert_file(s, &anchor, &id),
            })
            .child(
                text(r.label.clone())
                    .font(k.ty.ui_compact)
                    .color(u.content)
                    .shrink()
                    .truncate()
                    .a11y_hidden(),
            );
        if !r.detail.is_empty() {
            item = item.child(
                text(r.detail.clone())
                    .font(k.ty.ui_compact)
                    .color(u.muted_text)
                    .grow(1.0)
                    .shrink()
                    .truncate()
                    .a11y_hidden(),
            );
        }
        rows = rows.child(item);
    }
    let empty = n == 0;
    let viewport = popover::fit_scroll(rows)
        .key("viewport")
        .role("listbox")
        .label(p.label)
        .h((n as f32 * 28.0).min(260.0))
        .min_h(if empty { 0.0 } else { 28.0 })
        .clip_soft(0.0);
    let list = col()
        .key("list")
        .preserve_focus_on_press(true)
        .w_full()
        .shrink()
        .min_h(0.0)
        .child(
            rune_kit::scroll_bar(&k, &format!("{}-scroll-area", p.key), "viewport", viewport)
                .w_full()
                .shrink()
                .min_h(0.0),
        )
        .child(
            el().key("empty")
                .h(if empty { 28.0 } else { 0.0 })
                .px(8.0)
                .center_y()
                .clip_soft(0.0)
                .opacity(if empty { 1.0 } else { 0.0 })
                .child(text(p.empty).font(k.ty.ui_compact).color(u.muted_text)),
        );
    let width = if p.trigger == Trigger::Slash {
        420.0
    } else {
        480.0
    };
    // `group`, not a button: the surface is a container, only rows act.
    let content = col()
        .key(format!("{}-content", p.key))
        .role("group")
        .label(p.label)
        .preserve_focus_on_press(true)
        .w(width)
        .padding(4.0)
        .instant_changes()
        .child(list);
    layer.push(popover::anchored(
        &k,
        popover::raised(&k, content, k.radius.md),
        BOX,
        Placement::TopStart,
        6.0,
    ));
}

/// Key of the highlighted option, for the field's `active_descendant`.
fn active_option(s: &Shell) -> String {
    pop(s)
        .and_then(|p| p.active.map(|i| option_key(p.key, &p.rows[i].id)))
        .unwrap_or_default()
}

/// Keys the field must see before its editor does: the popup's navigation,
/// Shift+Tab for the mode, and the window's chords while a turn runs.
fn on_field_key(s: &mut Shell, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Tab && ev.mods.shift {
        actions::cycle_mode(s);
        return true;
    }
    if let Some(a) = actions::active(s) {
        let open = !actions::rows(s, &a).is_empty()
            || (a.trigger == Trigger::File && !s.chat.files.is_idle());
        match ev.key {
            Key::Down if open => {
                actions::popup_move(s, 1);
                return true;
            }
            Key::Up if open => {
                actions::popup_move(s, -1);
                return true;
            }
            Key::Enter | Key::Tab if actions::popup_accept(s) => return true,
            Key::Escape => {
                actions::popup_close(s);
                return true;
            }
            _ => {}
        }
    }
    // Typing `@` is what asks for the file list: once, off the UI thread.
    if matches!(&ev.key, Key::Text(t) if t == "@") {
        actions::ensure_files(s);
    }
    if matches!(ev.key, Key::Text(_) | Key::Backspace | Key::Delete) {
        s.chat.popup_active = 0;
    }
    let global = (ev.key == Key::Escape
        && (s.chat.cancel.is_some() || s.chat.model_menu || s.chat.agent_menu || s.chat.mode_menu))
        || "12345678nk".chars().any(|c| ev.is_command(c));
    global && crate::on_key(s, ev, tx)
}

pub fn composer(s: &Shell, u: &Ui, layer: &mut Vec<El>) -> El {
    let c = &s.chat;
    let field = text_input(&c.draft, ui::BODY)
        .key("draft")
        .grow(1.0)
        .min_w(0.0)
        .max_lines(8)
        .color(u.content)
        .label("Mensagem para o Zerith")
        .description("Digite / para comandos e @ para arquivos")
        .placeholder("Pergunte algo, / comandos, @ arquivos", u.faint)
        .active_descendant(active_option(s))
        .on_edit(|s: &mut Shell| &mut s.chat.draft)
        .on_submit(|s: &mut Shell, _: &mut Tx| super::send(s))
        .on_input_key(on_field_key);
    let action = if c.busy {
        widgets::icon_button("stop", "Parar o turno (Esc)", ui::icon_stop(), u, |s| {
            s.stop()
        })
    } else {
        widgets::primary_icon("send", "Enviar a mensagem", ui::icon_send(), u, super::send)
    };
    popup(s, u, layer);
    let border_color = match c.permission_mode() {
        PermissionMode::Plan => {
            if c.draft.focused {
                u.accent
            } else {
                u.accent.alpha(0.55)
            }
        }
        PermissionMode::Bypass => {
            if c.draft.focused {
                u.warn
            } else {
                u.warn.alpha(0.55)
            }
        }
        _ => {
            if c.draft.focused {
                u.line_strong
            } else {
                u.line
            }
        }
    };
    col()
        .key("composer")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .key(BOX)
                .w_full()
                .min_h(48.0)
                .pl(16.0)
                .pr(10.0)
                .py(9.0)
                .gap(8.0)
                .center_y()
                .radius(ui::R_LG)
                .fill(u.muted)
                .border(1.0, border_color)
                .child(field)
                .child(action),
        )
        .child(footer(s, u, layer))
}
