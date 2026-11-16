//! The transcript: messages, tool one-liners, subagent rows and notes.
//!
//! Calm by construction: no boxes around messages, hairlines instead of
//! frames, one muted fill for the things that hold code or text to read.

use rune::{col, el, icon, row, rule, text, El, GrowthSpec, Icon, ScrollSpec};
use rune_kit::Markdown;

use super::{Line, Shell};
use crate::agentwork::{
    format_duration, turn_changes, DiffKind, DiffSummary, Item, SubState, Subagent, ToolRow,
    ToolState, TurnChanges,
};
use crate::ui::{self, Ui};
use crate::widgets::{self, button, Tone};

/// A mark for a state: spinner, check or cross, 14 pt, never colour alone
/// (callers also put the state in the row's label).
pub fn spinner(u: &Ui, color: rune::Color) -> El {
    rune_kit::Spinner::new()
        .size(13.0)
        .color(color)
        .build(&ui::kit(u))
}

fn tool_icon(name: &str) -> &'static Icon {
    match name {
        "shell" => ui::icon_terminal(),
        "read_file" | "write_file" => ui::icon_file(),
        "edit_file" => ui::icon_edit(),
        "grep" | "glob" | "memory_recall" => ui::icon_search(),
        "list_dir" => ui::icon_folder(),
        _ => ui::icon_tool(),
    }
}

/// A disclosure row: chevron, content, optional trailing mark. 28 pt, the
/// whole row presses, focus ring from the window.
fn disclosure(
    key: String,
    label: String,
    open: bool,
    can_open: bool,
    index: usize,
    u: &Ui,
    content: Vec<El>,
) -> El {
    let mut r = row()
        .key(key)
        .role("button")
        .label(label)
        .expanded(open)
        .w_full()
        .min_h(28.0)
        .px(6.0)
        .gap(8.0)
        .center_y()
        .radius(ui::R_SM);
    if can_open {
        r = r
            .focusable(true)
            .hover(u.hover)
            .press(u.active)
            .on_press(move |s: &mut Shell| s.toggle_open(index));
    }
    for c in content {
        r = r.child(c);
    }
    r
}

fn chevron(open: bool, can_open: bool, u: &Ui) -> El {
    if !can_open {
        return el().size(14.0, 14.0);
    }
    icon(
        if open {
            ui::icon_chevron_down()
        } else {
            ui::icon_chevron_right()
        },
        14.0,
    )
    .color(u.faint)
}

/// The panel code and output sit in: layer fill, hairline, soft corners.
fn code_panel(u: &Ui) -> El {
    col()
        .w_full()
        .py(6.0)
        .radius(ui::R_MD)
        .clip_rounded(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
}

fn diff_panel(d: &DiffSummary, u: &Ui) -> El {
    let mut panel = code_panel(u);
    for (i, r) in d.rows.iter().enumerate() {
        let (fill, mark, ink) = match r.kind {
            DiffKind::Add => (u.diff_add, "+", u.diff_add_ink),
            DiffKind::Del => (u.diff_del, "−", u.diff_del_ink),
            DiffKind::Ctx => (rune::Color::TRANSPARENT, " ", u.faint),
        };
        let num = r
            .line_no
            .map(|n| format!("{n:>3}"))
            .unwrap_or_else(|| "   ".into());
        panel = panel.child(
            row()
                .key(format!("dl-{i}"))
                .w_full()
                .px(10.0)
                .gap(8.0)
                .fill(fill)
                .child(
                    el().w(28.0)
                        .child(text(num).font(ui::MONO).color(u.faint))
                        .a11y_hidden(),
                )
                .child(
                    // A fixed cell, so context lines (a blank mark) align.
                    el().w(10.0)
                        .child(text(mark).font(ui::MONO).color(ink))
                        .a11y_hidden(),
                )
                .child(
                    text(if r.text.is_empty() {
                        " ".to_string()
                    } else {
                        r.text.clone()
                    })
                    .wrap()
                    .font(ui::MONO)
                    .color(u.content)
                    .selectable(true)
                    .grow(1.0)
                    .min_w(0.0),
                ),
        );
    }
    if d.more > 0 {
        panel = panel.child(
            text(format!("+{} linhas", d.more))
                .font(ui::LABEL)
                .color(u.faint)
                .px(10.0)
                .pt(4.0),
        );
    }
    panel.role("group").label(format!(
        "Alteração: {} linhas adicionadas, {} removidas",
        d.adds, d.dels
    ))
}

/// One tool call as a one-liner: icon, tool, short argument, duration. A diff
/// or live running output opens by default; finished output opens on demand.
fn tool_row(index: usize, t: &ToolRow, toggled: bool, u: &Ui) -> El {
    let has_body = t.diff.is_some() || !t.preview.is_empty();
    let open = has_body
        && ((t.diff.is_some() || (t.state == ToolState::Running && !t.preview.is_empty()))
            != toggled);
    let state_word = match t.state {
        ToolState::Running => "em execução",
        ToolState::Ok => "concluída",
        ToolState::Failed => "falhou",
    };
    let tail = match t.state {
        ToolState::Running => spinner(u, u.accent),
        ToolState::Failed => icon(ui::icon_cross(), 14.0).color(u.danger),
        ToolState::Ok => match (&t.diff, t.millis) {
            (Some(d), _) => text(format!("+{} −{}", d.adds, d.dels))
                .font(ui::LABEL)
                .color(u.faint),
            (None, Some(ms)) => text(format_duration(ms)).font(ui::LABEL).color(u.faint),
            _ => el(),
        },
    };
    let head = disclosure(
        format!("tool-{}", t.id),
        format!("{} {}, {state_word}", t.name, t.arg),
        open,
        has_body,
        index,
        u,
        vec![
            chevron(open, has_body, u),
            icon(tool_icon(&t.name), 14.0).color(u.faint),
            text(t.name.clone()).font(ui::SMALL).color(u.secondary),
            text(t.arg.clone())
                .font(ui::MONO)
                .color(if t.state == ToolState::Failed {
                    u.danger
                } else {
                    u.muted_text
                })
                .truncate()
                .grow(1.0)
                .min_w(0.0),
            tail,
        ],
    );
    let mut c = col().w_full().pb(2.0).child(head);
    if open {
        let body = match (&t.diff, t.preview.is_empty()) {
            (Some(d), _) => diff_panel(d, u),
            (None, _) => code_panel(u).child(
                text(t.preview.clone())
                    .wrap()
                    .max_lines(10)
                    .font(ui::MONO)
                    .color(if t.state == ToolState::Failed {
                        u.danger
                    } else {
                        u.secondary
                    })
                    .selectable(true)
                    .px(10.0),
            ),
        };
        c = c.child(
            row()
                .w_full()
                .pl(28.0)
                .pt(2.0)
                .pb(6.0)
                .child(body.grow(1.0)),
        );
    }
    c
}

fn sub_mark(state: SubState, u: &Ui) -> El {
    match state {
        SubState::Running => spinner(u, u.accent),
        SubState::Done => icon(ui::icon_check(), 14.0).color(u.done),
        SubState::Error => icon(ui::icon_cross(), 14.0).color(u.danger),
    }
}

/// A subagent: name, state, one-line summary; opens to the whole summary.
/// Parallel ones are consecutive rows, each its own hairline card.
fn subagent_row(index: usize, a: &Subagent, open: bool, u: &Ui) -> El {
    let state_word = match a.state {
        SubState::Running => "em execução",
        SubState::Done => "concluído",
        SubState::Error => "com erro",
    };
    let has_body = !a.summary.trim().is_empty();
    let first = a.summary.lines().next().unwrap_or_default().to_string();
    let head = disclosure(
        format!("sub-{}", a.id),
        format!("Subagente {}, {state_word}", a.name),
        open,
        has_body,
        index,
        u,
        vec![
            sub_mark(a.state, u),
            text(a.name.clone()).font(ui::TITLE).color(u.content),
            text(if first.is_empty() {
                state_word.to_string()
            } else {
                first
            })
            .font(ui::SMALL)
            .color(if a.state == SubState::Error {
                u.danger
            } else {
                u.muted_text
            })
            .truncate()
            .grow(1.0)
            .min_w(0.0),
            chevron(open, has_body, u),
        ],
    );
    let mut card = col()
        .w_full()
        .radius(ui::R_MD)
        .border(1.0, u.line)
        .py(2.0)
        .child(head);
    if open && has_body {
        card = card.child(
            text(a.summary.clone())
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .selectable(true)
                .px(34.0)
                .pb(10.0)
                .pt(2.0),
        );
    }
    col().w_full().pb(8.0).child(card)
}

fn note_row(msg: &str, u: &Ui) -> El {
    row()
        .w_full()
        .pb(18.0)
        .gap(12.0)
        .center_y()
        .role("separator")
        .label(msg.to_string())
        .child(rule(1.0, u.line).grow(1.0))
        .child(text(msg.to_string()).font(ui::LABEL).color(u.faint))
        .child(rule(1.0, u.line).grow(1.0))
}

fn thinking_row(index: usize, content: &str, open: bool, u: &Ui) -> El {
    let mut c = col().w_full().pb(8.0).child(disclosure(
        format!("think-{index}"),
        "Raciocínio".into(),
        open,
        true,
        index,
        u,
        vec![
            chevron(open, true, u),
            text("Raciocínio").font(ui::SMALL).color(u.muted_text),
        ],
    ));
    if open {
        c = c.child(
            text(content.to_string())
                .wrap()
                .font(ui::SMALL)
                .color(u.muted_text)
                .selectable(true)
                .pl(28.0)
                .pt(2.0)
                .pb(6.0),
        );
    }
    c
}

pub fn line_row(
    index: usize,
    line: &Line,
    u: &Ui,
    live: bool,
    open: bool,
    rewind: bool,
    changes: Option<TurnChanges>,
) -> El {
    if let Some(item) = &line.item {
        return match item {
            Item::Tool(t) => tool_row(index, t, open, u),
            Item::Subagent(a) => subagent_row(index, a, open, u),
            Item::Note(n) => note_row(n, u),
        };
    }
    match line.role {
        "user" => {
            let mut c = col().w_full().pb(20.0).items_end().gap(4.0).child(
                col()
                    .max_w(560.0)
                    .py(10.0)
                    .px(14.0)
                    .radius(ui::R_LG)
                    .fill(u.muted)
                    .child(
                        text(line.content.clone())
                            .wrap()
                            .font(ui::BODY)
                            .color(u.content)
                            .selectable(true),
                    ),
            );
            if rewind {
                let mut tail = row().gap(8.0).center_y();
                if let Some(ch) = changes.filter(|ch| ch.files > 0) {
                    tail = tail.child(widgets::pill(
                        format!("{} arq · +{} −{}", ch.files, ch.adds, ch.dels),
                        u,
                    ));
                }
                tail = tail.child(button(
                    format!("rewind-{index}"),
                    "Rebobinar",
                    Tone::Plain,
                    true,
                    u,
                    |s, _| super::rewind(s),
                ));
                c = c.child(tail);
            }
            c
        }
        "thinking" => thinking_row(index, &line.content, open, u),
        "error" => col()
            .w_full()
            .pb(18.0)
            .child(widgets::error_line(line.content.clone(), u)),
        "assistant" => {
            let body = if line.content.is_empty() {
                "…".to_string()
            } else {
                line.content.clone()
            };
            col().w_full().pb(20.0).child(
                Markdown::new(body)
                    .streaming(live)
                    .selectable(true)
                    .build(&ui::kit(u)),
            )
        }
        _ => col()
            .w_full()
            .pb(18.0)
            .gap(4.0)
            .child(widgets::eyebrow("Sistema", u))
            .child(
                text(line.content.clone())
                    .wrap()
                    .font(ui::BODY)
                    .color(u.content)
                    .selectable(true),
            ),
    }
}

/// The scrolling, virtualized message list.
pub fn list(s: &Shell, u: &Ui) -> El {
    let c = &s.chat;
    let n = c.lines.len();
    let busy = c.busy;
    let uu = *u;
    let mut inner = col().w_full();
    if c.more {
        inner = inner.child(row().w_full().pb(16.0).justify_center().child(button(
            "older",
            "Carregar mensagens anteriores",
            Tone::Plain,
            true,
            u,
            |s, _| super::load_all(s),
        )));
    }
    inner = inner.child(
        col()
            .key("transcript")
            .w_full()
            .growth(GrowthSpec::default())
            .virtual_rows(
                n,
                64.0,
                |_: &Shell, i| format!("line-{i}").into(),
                move |s: &Shell, i| {
                    let line = &s.chat.lines[i];
                    let live = busy && i + 1 == s.chat.lines.len();
                    // Rebobinar sits under the last user message only: the
                    // latest checkpoint is that turn's.
                    let last_user = s.chat.lines.iter().rposition(|l| l.role == "user");
                    let rewind = s.chat.rewind && !s.chat.busy && last_user == Some(i);
                    let changes = rewind.then(|| {
                        turn_changes(s.chat.lines[i + 1..].iter().filter_map(|l| match &l.item {
                            Some(Item::Tool(t)) => Some(t),
                            _ => None,
                        }))
                    });
                    line_row(
                        i,
                        line,
                        &uu,
                        live,
                        s.chat.open.contains(&i),
                        rewind,
                        changes,
                    )
                },
            ),
    );
    inner
        .scroll(ScrollSpec { stick_bottom: true })
        .grow(1.0)
        .min_h(0.0)
}
