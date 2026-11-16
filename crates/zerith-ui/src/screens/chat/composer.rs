//! The composer: input row, `/` and `@` popups, and the footer (model,
//! agent, permission mode, context meter, turn cost).

use rune::{col, el, row, text, text_input, Align, El, Key, KeyEvent, Side, Tx};
use rune_kit::{Placement, PopupItem, PopupList, Select};

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
        Load::Loading => "carregando modelos…",
        Load::Failed(_) => "modelos indisponíveis",
        _ => "escolher modelo",
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
    if let Some(m) = meter(s, u) {
        foot = foot.child(m);
    }
    if let Some(k) = cost(s, u) {
        foot = foot.child(k);
    }
    foot.px(2.0)
}

/// The `/` and `@` popups, anchored above the input frame.
fn popup(s: &Shell, u: &Ui, layer: &mut Vec<El>) {
    let Some(a) = actions::active(s) else { return };
    if s.chat.draft.focused.then_some(()).is_none() {
        return;
    }
    let rows = actions::rows(s, &a);
    let k = ui::kit(u);
    let (key, label, empty, items): (&str, &str, String, Vec<PopupItem>) = match &rows {
        Rows::Slash(v) => (
            "slash",
            "Comandos",
            "Nenhum comando".into(),
            v.iter()
                .map(|c| PopupItem::new(c.name, format!("/{}", c.name)).description(c.detail))
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
                    PopupItem::new(p.clone(), name.to_string()).description(dir)
                })
                .collect(),
        ),
    };
    let n = items.len();
    let trigger = a.trigger;
    let a2 = a.clone();
    PopupList::<Shell>::new(key, BOX)
        .anchored(BOX, Side::Top, Align::Start, 6.0)
        .width(if trigger == Trigger::Slash {
            420.0
        } else {
            480.0
        })
        .max_height(260.0)
        .items(items)
        .active((n > 0).then(|| s.chat.popup_active.min(n - 1)))
        .empty_message(empty)
        .label(label)
        .on_active(|s: &mut Shell, i| s.chat.popup_active = i)
        .on_choose(move |s: &mut Shell, id: &str, _: &mut Tx| match trigger {
            Trigger::Slash => actions::run_command(s, id),
            Trigger::File => actions::insert_file(s, &a2, id),
        })
        .build(&k, layer);
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
        || "1234567nk".chars().any(|c| ev.is_command(c));
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
                .border(
                    1.0,
                    if c.draft.focused {
                        u.line_strong
                    } else {
                        u.line
                    },
                )
                .child(field)
                .child(action),
        )
        .child(footer(s, u, layer))
}
