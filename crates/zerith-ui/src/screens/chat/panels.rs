//! The cards that sit between the transcript and the composer: the task
//! checklist, the permission card, the plan review and the question card.
//! Each holds the turn (or reports it) and each is a labelled group that the
//! keyboard reaches in reading order.

use rune::{col, el, icon, row, rule, text, text_input, El, LiveRegion, ScrollSpec, Tx};
use rune_kit::Markdown;

use super::actions;
use super::transcript::spinner;
use super::{Approval, Decision, Shell};
use crate::agentwork::{self, task_progress, QuestionState, TaskItem, TaskState};
use crate::ui::{self, Ui};
use crate::widgets::{button, pill, Tone};

/// The flat card all four share: layer fill, hairline, 12 radius.
fn card(key: &'static str, role: &'static str, label: String, u: &Ui) -> El {
    col()
        .key(key)
        .role(role)
        .label(label)
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
}

// ---------------------------------------------------------------- tasks

fn task_mark(t: &TaskItem, u: &Ui) -> El {
    match t.state {
        TaskState::Pending => el()
            .size(14.0, 14.0)
            .center()
            .child(el().size(8.0, 8.0).radius(4.0).border(1.0, u.faint))
            .a11y_hidden(),
        TaskState::InProgress => spinner(u, u.accent),
        TaskState::Completed => icon(ui::icon_check(), 14.0).color(u.ok),
    }
}

fn task_row(i: usize, t: &TaskItem, u: &Ui) -> El {
    let state_word = match t.state {
        TaskState::Pending => "pendente",
        TaskState::InProgress => "em andamento",
        TaskState::Completed => "concluída",
    };
    let shown = t.shown().to_string();
    let done = t.state == TaskState::Completed;
    let mut label = text(shown.clone())
        .font(ui::SMALL)
        .color(match t.state {
            TaskState::Completed => u.faint,
            TaskState::InProgress => u.content,
            TaskState::Pending => u.secondary,
        })
        .grow(1.0)
        .min_w(0.0)
        .wrap();
    if done {
        label = label.strike(0, shown.len(), None);
    }
    row()
        .key(format!("task-{i}"))
        .role("listitem")
        .label(format!("{shown}, {state_word}"))
        .w_full()
        .gap(10.0)
        .center_y()
        .py(3.0)
        .child(task_mark(t, u))
        .child(label)
}

/// The live checklist above the composer. Collapsed it is one line
/// ("2 de 5 tarefas"); open it lists every task, scrolling past six.
pub fn tasks(s: &Shell, u: &Ui) -> Option<El> {
    let c = &s.chat;
    if c.tasks.is_empty() {
        return None;
    }
    let (done, total) = task_progress(&c.tasks);
    let current = c
        .tasks
        .iter()
        .find(|t| t.state == TaskState::InProgress)
        .map(|t| t.shown().to_string());
    let open = c.tasks_open;
    let title = format!("{done} de {total} tarefas");
    let head = row()
        .key("tasks-toggle")
        .role("button")
        .label(format!("Tarefas: {title}"))
        .expanded(open)
        .focusable(true)
        .w_full()
        .h(32.0)
        .px(14.0)
        .gap(8.0)
        .center_y()
        .hover(u.hover)
        .press(u.active)
        .on_press(|s: &mut Shell| actions::toggle_tasks(s))
        .child(icon(ui::icon_list(), 14.0).color(u.faint))
        .child(text(title).font(ui::SMALL).color(u.secondary))
        .child(
            text(if open {
                String::new()
            } else {
                current.unwrap_or_default()
            })
            .font(ui::SMALL)
            .color(u.muted_text)
            .truncate()
            .grow(1.0)
            .min_w(0.0),
        )
        .child(
            icon(
                if open {
                    ui::icon_chevron_down()
                } else {
                    ui::icon_chevron_right()
                },
                14.0,
            )
            .color(u.faint),
        );
    let mut panel = col()
        .key("tasks")
        .role("region")
        .label("Tarefas do agente")
        .live_region(LiveRegion::Polite)
        .w_full()
        .radius(ui::R_MD)
        .clip_rounded(ui::R_MD)
        .border(1.0, u.line)
        .child(head);
    if open {
        let mut list = col()
            .role("list")
            .label("Tarefas")
            .w_full()
            .px(14.0)
            .pb(10.0)
            .gap(0.0);
        for (i, t) in c.tasks.iter().enumerate() {
            list = list.child(task_row(i, t, u));
        }
        panel = panel.child(rule(1.0, u.line).w_full()).child(
            list.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .max_h(180.0)
            .pt(6.0),
        );
    }
    Some(panel)
}

// ----------------------------------------------------------- permission

pub fn approval_card(a: &Approval, u: &Ui) -> El {
    card(
        "approval",
        "alertdialog",
        format!("Permissão pedida: {}", a.title),
        u,
    )
    .child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(icon(ui::icon_shield(), 16.0).color(u.warn))
            .child(
                text(a.title.clone())
                    .font(ui::TITLE)
                    .color(u.content)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            )
            .child(pill(a.tool.clone(), u)),
    )
    .child(
        text(a.summary.clone())
            .wrap()
            .font(ui::MONO)
            .color(u.secondary)
            .selectable(true),
    )
    .child(
        row()
            .w_full()
            .gap(8.0)
            .child(button(
                "approve-once",
                "Permitir uma vez",
                Tone::Primary,
                true,
                u,
                |s, _| s.decide(Decision::Once),
            ))
            .child(button(
                "approve-session",
                "Permitir na sessão",
                Tone::Plain,
                true,
                u,
                |s, _| s.decide(Decision::Session),
            ))
            .child(el().grow(1.0))
            .child(button("deny", "Negar", Tone::Danger, true, u, |s, _| {
                s.decide(Decision::Deny)
            })),
    )
}

// ----------------------------------------------------------------- plan

/// The plan gate's card: the plan in Markdown, or in an editor after
/// "Editar", with the three ways out.
pub fn plan_card(s: &Shell, a: &Approval, u: &Ui) -> El {
    let editing = s.chat.plan_edit.as_ref();
    let body = actions::plan_body(&a.summary).to_string();
    let content = match editing {
        Some(edit) => text_input(edit, ui::MONO)
            .key("plan-edit")
            .w_full()
            .min_h(140.0)
            .color(u.content)
            .label("Plano (editável)")
            .on_edit(|s: &mut Shell| s.chat.plan_edit.as_mut().expect("editing"))
            .into_card(u),
        None => col()
            .w_full()
            .max_h(320.0)
            .child(Markdown::new(body).selectable(true).build(&ui::kit(u)))
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .key("plan-body"),
    };
    let mut actions_row = row().w_full().gap(8.0).center_y().child(button(
        "plan-approve",
        "Aprovar e executar",
        Tone::Primary,
        true,
        u,
        |s, _| actions::plan_approve(s),
    ));
    actions_row = if editing.is_some() {
        actions_row.child(button(
            "plan-edit-cancel",
            "Descartar edição",
            Tone::Plain,
            true,
            u,
            |s, _| actions::plan_edit_cancel(s),
        ))
    } else {
        actions_row.child(button(
            "plan-edit-open",
            "Editar",
            Tone::Plain,
            true,
            u,
            |s, _| actions::plan_edit(s),
        ))
    };
    actions_row = actions_row.child(el().grow(1.0)).child(button(
        "plan-continue",
        "Continuar planejando",
        Tone::Plain,
        true,
        u,
        |s, _| actions::plan_continue(s),
    ));
    card(
        "plan",
        "alertdialog",
        format!(
            "{}: aprove, edite ou continue planejando",
            agentwork::PLAN_TITLE
        ),
        u,
    )
    .border(1.0, u.accent.alpha(0.35))
    .child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(icon(ui::icon_list(), 16.0).color(u.accent_text))
            .child(
                text(agentwork::PLAN_TITLE)
                    .font(ui::TITLE)
                    .color(u.content)
                    .grow(1.0),
            )
            .child(pill(
                if editing.is_some() {
                    "editando"
                } else {
                    "somente leitura"
                },
                u,
            )),
    )
    .child(content)
    .child(actions_row)
}

/// Gives the plan editor the field look inside the card.
trait IntoCard {
    fn into_card(self, u: &Ui) -> El;
}

impl IntoCard for El {
    fn into_card(self, u: &Ui) -> El {
        col()
            .w_full()
            .px(12.0)
            .py(10.0)
            .radius(ui::R_SM)
            .fill(u.surface)
            .border(1.0, u.line_strong)
            .child(self)
    }
}

// ------------------------------------------------------------- question

fn option_row(q: usize, o: usize, text_: &str, multi: bool, on: bool, u: &Ui) -> El {
    // Radio for one answer, square box for several: the mark is drawn here so
    // it follows the window's accent and hairlines.
    let mark = if multi {
        el().size(16.0, 16.0)
            .radius(ui::R_XS)
            .center()
            .fill(if on {
                u.accent
            } else {
                rune::Color::TRANSPARENT
            })
            .border(1.0, if on { u.accent } else { u.line_strong })
            .child(if on {
                icon(ui::icon_check(), 12.0).color(rune::Color::WHITE)
            } else {
                el()
            })
    } else {
        el().size(16.0, 16.0)
            .radius(8.0)
            .center()
            .border(1.0, if on { u.accent } else { u.line_strong })
            .child(if on {
                el().size(8.0, 8.0).radius(4.0).fill(u.accent)
            } else {
                el()
            })
    };
    row()
        .key(format!("q{q}-o{o}"))
        .role(if multi { "checkbox" } else { "radio" })
        .label(text_.to_string())
        .checked(on)
        .focusable(true)
        .w_full()
        .min_h(32.0)
        .px(8.0)
        .gap(10.0)
        .center_y()
        .radius(ui::R_SM)
        .fill(if on {
            u.accent_soft
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(u.hover)
        .press(u.active)
        .on_press(move |s: &mut Shell| actions::question_pick(s, q, o))
        .child(mark)
        .child(
            text(text_.to_string())
                .wrap()
                .font(ui::BODY)
                .color(u.content)
                .grow(1.0)
                .min_w(0.0)
                .a11y_hidden(),
        )
}

pub fn question_card(q: &QuestionState, u: &Ui) -> El {
    let mut c = card("question", "dialog", "Pergunta do agente".into(), u).child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(icon(ui::icon_question(), 16.0).color(u.accent_text))
            .child(
                text("O agente precisa de uma resposta")
                    .font(ui::TITLE)
                    .color(u.content),
            ),
    );
    for (qi, question) in q.set.questions.iter().enumerate() {
        let mut group = col()
            .key(format!("q{qi}"))
            .role(if question.multi {
                "group"
            } else {
                "radiogroup"
            })
            .label(question.text.clone())
            .focus_group()
            .w_full()
            .gap(2.0);
        if !question.header.is_empty() {
            group = group.child(
                text(question.header.clone())
                    .font(ui::LABEL)
                    .color(u.muted_text),
            );
        }
        group = group.child(
            text(question.text.clone())
                .wrap()
                .font(ui::BODY)
                .color(u.content)
                .selectable(true)
                .pb(4.0),
        );
        if question.multi {
            group = group.child(
                text("Escolha uma ou mais")
                    .font(ui::LABEL)
                    .color(u.faint)
                    .pb(4.0),
            );
        }
        for (oi, opt) in question.options.iter().enumerate() {
            group = group.child(option_row(
                qi,
                oi,
                opt,
                question.multi,
                q.is_picked(qi, oi),
                u,
            ));
        }
        c = c.child(group);
    }
    c.child(
        row()
            .w_full()
            .gap(8.0)
            .child(button(
                "question-submit",
                "Responder",
                Tone::Primary,
                q.ready(),
                u,
                |s, _: &mut Tx| super::events::answer_question(s),
            ))
            .child(el().grow(1.0))
            .child(button(
                "question-skip",
                "Pular",
                Tone::Plain,
                true,
                u,
                |s, _| super::events::skip_question(s),
            )),
    )
}
