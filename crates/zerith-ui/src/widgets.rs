//! The handful of controls every screen shares. Each one reads its colours
//! from `Ui` and announces a role and a label, so a screen built from these is
//! keyboard- and screen-reader-usable without remembering to be.

use rune::{col, el, icon, row, rule, text, text_input, Color, El, Icon, ScrollSpec, TextEdit, Tx};
use rune_kit::Loader;

use crate::ui::{self, Ui};
use crate::Shell;

/// Something a worker produces. The four states every screen must draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }
    pub fn is_idle(&self) -> bool {
        matches!(self, Load::Idle)
    }
    pub fn is_loading(&self) -> bool {
        matches!(self, Load::Loading)
    }
}

impl<T, E: std::fmt::Display> From<Result<T, E>> for Load<T> {
    fn from(r: Result<T, E>) -> Self {
        match r {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e.to_string()),
        }
    }
}

/// A section header: 11 pt, muted.
pub fn eyebrow(label: impl Into<String>, u: &Ui) -> El {
    text(label.into()).font(ui::LABEL).color(u.muted_text)
}

/// A square icon button. `label` is what a screen reader announces, so it is
/// never optional here.
pub fn icon_button(
    key: impl Into<String>,
    label: impl Into<String>,
    ic: &'static Icon,
    u: &Ui,
    f: impl Fn(&mut Shell) + 'static,
) -> El {
    el().key(key.into())
        .role("button")
        .label(label.into())
        .focusable(true)
        .size(28.0, 28.0)
        .radius(ui::R_SM)
        .hover(u.hover)
        .press(u.active)
        .on_press(move |s: &mut Shell| f(s))
        .center()
        .child(icon(ic, 16.0).color(u.muted_text))
}

/// The one accent control of the composer.
pub fn primary_icon(
    key: &'static str,
    label: &'static str,
    ic: &'static Icon,
    u: &Ui,
    f: impl Fn(&mut Shell) + 'static,
) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .size(28.0, 28.0)
        .radius(8.0)
        .fill(u.primary)
        .hover(u.primary_press)
        .press(u.primary_press)
        .on_press(move |s: &mut Shell| f(s))
        .center()
        .child(icon(ic, 16.0).color(u.inverted))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Filled with the accent. One per screen at most.
    Primary,
    /// A muted fill with a hairline: the default.
    Plain,
    /// Plain, with the label in the danger colour.
    Danger,
}

/// A text button, 28 pt tall. Disabled buttons drop their handler and leave
/// the Tab order, so a pending action cannot be pressed twice.
pub fn button(
    key: impl Into<String>,
    label: impl Into<String>,
    tone: Tone,
    enabled: bool,
    u: &Ui,
    f: impl Fn(&mut Shell, &mut Tx) + 'static,
) -> El {
    let label = label.into();
    let (fill, ink, hover) = match tone {
        Tone::Primary => (u.primary, u.inverted, u.primary_press),
        Tone::Plain => (Color::TRANSPARENT, u.secondary, u.hover),
        Tone::Danger => (Color::TRANSPARENT, u.danger, u.hover),
    };
    let mut b = row()
        .key(key.into())
        .role("button")
        .label(label.clone())
        .h(28.0)
        .px(12.0)
        .center()
        .radius(ui::R_SM)
        .fill(fill)
        .child(text(label).font(ui::SMALL).color(ink));
    if tone != Tone::Primary {
        b = b.border(1.0, u.line);
    }
    if enabled {
        b.focusable(true)
            .hover(hover)
            .press(u.active)
            .on_press_tx(move |s: &mut Shell, tx: &mut Tx| f(s, tx))
    } else {
        b.disabled(true).focusable(false).opacity(0.5)
    }
}

/// A single-line field in the composer's look: muted fill, hairline, 10 pt
/// radius. `get` hands Rune the `TextEdit` to type into; `wire_input` adds
/// submit/change handlers to the inner input.
#[allow(clippy::too_many_arguments)]
pub fn field(
    key: impl Into<String>,
    edit: &TextEdit,
    label: impl Into<String>,
    placeholder: impl Into<String>,
    leading: Option<&'static Icon>,
    u: &Ui,
    get: impl for<'a> Fn(&'a mut Shell) -> &'a mut TextEdit + 'static,
    wire_input: impl FnOnce(El) -> El,
) -> El {
    let input = text_input(edit, ui::BODY)
        .key(key.into())
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(u.content)
        .label(label.into())
        .placeholder(placeholder.into(), u.faint)
        .on_edit(get);
    let input = wire_input(input);
    let mut r = row()
        .w_full()
        .h(32.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(10.0)
        .fill(u.layer2)
        .border(
            1.0,
            if edit.focused {
                u.accent.alpha(0.75)
            } else {
                u.line_strong
            },
        );
    if let Some(i) = leading {
        r = r.child(icon(i, 16.0).color(u.faint));
    }
    r.child(input)
}

/// A labelled field stacked over its control, for forms.
pub fn labelled(label: &'static str, control: El, u: &Ui) -> El {
    col()
        .w_full()
        .gap(6.0)
        .child(eyebrow(label, u))
        .child(control)
}

/// A settings row: name and explanation on the left, control on the right,
/// a hairline under it. Rows, not boxes. In a narrow window the control moves
/// under the text instead of squeezing it.
pub fn setting(label: &str, detail: &str, control: El, u: &Ui) -> El {
    setting_full(label, detail, "", control, false, u)
}

/// [`setting`] with the real setting name in a small muted hint under the
/// explanation, and an option to always stack the control under the text (for
/// wide controls such as a five-way selector).
pub fn setting_full(
    label: &str,
    detail: &str,
    hint_text: &str,
    control: El,
    stacked: bool,
    u: &Ui,
) -> El {
    let mut words = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .child(text(label.to_string()).font(ui::BODY).color(u.content))
        .child(
            text(detail.to_string())
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text)
                .selectable(true),
        );
    if !hint_text.is_empty() {
        words = words.child(hint(hint_text, u));
    }
    let body = if stacked || u.compact {
        col()
            .w_full()
            .py(12.0)
            .gap(10.0)
            .child(words)
            .child(control)
    } else {
        row()
            .w_full()
            .py(12.0)
            .gap(16.0)
            .center_y()
            .child(words)
            .child(control)
    };
    col().w_full().child(body)
}

/// A small muted line for the real name of a setting or a file format. Never
/// a title: the title says what it does, this says what it is called on disk.
pub fn hint(msg: impl Into<String>, u: &Ui) -> El {
    text(msg.into())
        .wrap()
        .font(ui::LABEL)
        .color(u.faint)
        .selectable(true)
}

/// A path or setting key in code style: muted fill, mono, one line. The full
/// text is the tooltip when it is cut.
pub fn code_line(msg: impl Into<String>, u: &Ui) -> El {
    let msg = msg.into();
    row()
        .w_full()
        .h(24.0)
        .px(8.0)
        .center_y()
        .radius(ui::R_XS)
        .fill(u.muted)
        .border(1.0, u.line)
        .min_w(0.0)
        .clip_soft(0.0)
        .child(
            text(msg.clone())
                .font(ui::MONO)
                .color(u.muted_text)
                .truncate()
                .shrink()
                .min_w(0.0)
                .selectable(true)
                .help_when_truncated(msg),
        )
}

/// Page-level horizontal navigation tabs, visually distinct from form segmented controls.
pub fn tabs(
    labels: &[&str],
    idx: usize,
    label: &str,
    u: &Ui,
    on_pick: impl Fn(&mut Shell, usize) + Clone + 'static,
) -> El {
    let mut bar = row()
        .role("tablist")
        .label(label.to_string())
        .focus_group()
        .gap(4.0)
        .center_y();
    let n = labels.len();
    for (i, title) in labels.iter().enumerate() {
        let on = i == idx;
        let pick = on_pick.clone();
        bar = bar.child(
            col()
                .key(format!("tab-{i}-{title}"))
                .role("tab")
                .label((*title).to_string())
                .position_in_set(i + 1, n)
                .checked(on)
                .focusable(true)
                .focus_selected(on)
                .px(12.0)
                .pt(6.0)
                .pb(4.0)
                .gap(4.0)
                .items_center()
                .radius(ui::R_SM)
                .fill(if on { u.muted } else { Color::TRANSPARENT })
                .border(
                    1.0,
                    if on {
                        u.line_strong
                    } else {
                        Color::TRANSPARENT
                    },
                )
                .hover(u.hover)
                .press(u.active)
                .on_press(move |s: &mut Shell| pick(s, i))
                .child(text((*title).to_string()).font(ui::SMALL).color(if on {
                    u.content
                } else {
                    u.muted_text
                }))
                .child(el().w(24.0).h(2.0).radius(1.0).fill(if on {
                    u.accent
                } else {
                    Color::TRANSPARENT
                })),
        );
    }
    col()
        .w_full()
        .gap(8.0)
        .child(bar)
        .child(rule(1.0, u.line).w_full())
}

/// A tab body that scrolls when it must, and says so: a scroll bar over the
/// right edge and a soft fade where content continues.
pub fn scroll_body(key: &'static str, body: El, u: &Ui) -> El {
    let viewport = body
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(14.0)
        .w_full()
        .grow(1.0)
        .min_h(0.0);
    rune_kit::scroll_bar(&ui::kit(u), &format!("{key}-area"), key, viewport)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
}

/// Two blocks side by side, or stacked in a narrow window.
pub fn split(left: El, right: El, gap: f32, u: &Ui) -> El {
    if u.compact {
        col().w_full().gap(gap.min(14.0)).child(left).child(right)
    } else {
        row()
            .w_full()
            .gap(gap)
            .items_start()
            .child(left)
            .child(right)
    }
}

/// A 13 pt progress ring: the kit's loader (a ring with a turning arc), which
/// stays clean at small sizes and holds still under reduced motion.
pub fn spinner(color: Color, size: f32, u: &Ui) -> El {
    Loader::new().size(size).color(color).build(&ui::kit(u))
}

/// A section of a settings-like page: eyebrow, then rows split by hairlines.
pub fn section(title: &'static str, rows: Vec<El>, u: &Ui) -> El {
    let n = rows.len();
    let mut c = col().w_full().gap(0.0).child(eyebrow(title, u).mb(2.0));
    for (i, r) in rows.into_iter().enumerate() {
        c = c.child(r);
        if i + 1 < n {
            c = c.child(rule(1.0, u.line).w_full());
        }
    }
    c
}

/// A 7 pt status dot. Colour alone never carries meaning: callers put the
/// state in words next to it.
pub fn dot(color: rune::Color) -> El {
    el().size(7.0, 7.0).radius(3.5).fill(color).a11y_hidden()
}

/// A rounded suggestion chip.
pub fn chip(
    key: impl Into<String>,
    label: &'static str,
    u: &Ui,
    f: impl Fn(&mut Shell) + 'static,
) -> El {
    row()
        .key(key.into())
        .role("button")
        .label(label)
        .focusable(true)
        .h(30.0)
        .px(12.0)
        .center()
        .radius(15.0)
        .border(1.0, u.line)
        .hover(u.hover)
        .press(u.active)
        .on_press(move |s: &mut Shell| f(s))
        .child(text(label).font(ui::SMALL).color(u.secondary))
}

/// A quiet pill label: model names, counts, tags.
pub fn pill(label: impl Into<String>, u: &Ui) -> El {
    row()
        .h(20.0)
        .px(8.0)
        .center()
        .radius(10.0)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(
            text(label.into())
                .font(ui::LABEL)
                .color(u.muted_text)
                .truncate(),
        )
}

/// Centered message for the empty, loading and error states. One headline,
/// one sentence, an optional action: nothing decorative.
pub fn notice(title: impl Into<String>, body: impl Into<String>, u: &Ui) -> El {
    col()
        .w_full()
        .py(48.0)
        .gap(6.0)
        .items_center()
        .child(text(title.into()).font(ui::TITLE).color(u.content))
        .child(
            text(body.into())
                .wrap()
                .max_w(380.0)
                .font(ui::SMALL)
                .color(u.muted_text)
                .selectable(true),
        )
}

pub fn loading(what: impl Into<String>, u: &Ui) -> El {
    row()
        .w_full()
        .py(48.0)
        .gap(8.0)
        .center()
        .role("status")
        .label(what.into())
        .child(spinner(u.faint, 14.0, u))
        .child(text("Carregando…").font(ui::SMALL).color(u.muted_text))
}

/// An inline error line: danger dot, then the message, selectable so the user
/// can copy it into a bug report.
pub fn error_line(msg: impl Into<String>, u: &Ui) -> El {
    let msg = msg.into();
    row()
        .w_full()
        .gap(8.0)
        .center_y()
        .role("alert")
        .label(msg.clone())
        .child(dot(u.danger))
        .child(
            text(msg)
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .selectable(true)
                .grow(1.0)
                .min_w(0.0),
        )
}

/// The sheet's page frame: title row, hairline, scrolling body.
pub fn page(title: impl Into<String>, actions: El, body: El, u: &Ui) -> El {
    col()
        .w_full()
        .h_full()
        .gap(12.0)
        .child(
            row()
                .w_full()
                .h(28.0)
                .center_y()
                .gap(8.0)
                .child(
                    text(title.into())
                        .font(ui::TITLE)
                        .color(u.content)
                        .grow(1.0)
                        .truncate(),
                )
                .child(actions),
        )
        .child(rule(1.0, u.line).w_full())
        .child(body.grow(1.0).min_h(0.0))
}
