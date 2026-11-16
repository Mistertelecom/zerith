//! What the buttons and keys of the chat do. Every function is plain state
//! (plus the worker jobs `Shell::work` runs), so tests drive them directly.

use rune::TextEdit;

use super::{decide, new_chat, Decision};
use crate::agentwork::{self, Active, PermissionMode, SlashCommand, Trigger};
use crate::{wire, Screen, Shell};

// ---------------------------------------------------------------- modes

/// Switches the permission mode. The handle is the one the running turn's
/// policy reads, so it applies from the next tool call, mid-turn included.
pub fn set_mode(s: &mut Shell, m: PermissionMode) {
    s.chat.mode.set(m.to_security());
    s.chat.mode_menu = false;
}

/// Shift+Tab.
pub fn cycle_mode(s: &mut Shell) {
    let next = s.chat.permission_mode().next();
    set_mode(s, next);
}

// ----------------------------------------------------------------- plan

/// Splits the plan gate's text: the title line the runtime prefixes is not
/// part of the plan.
pub fn plan_body(summary: &str) -> &str {
    let t = summary.trim();
    t.strip_prefix(agentwork::PLAN_TITLE)
        .map(|r| r.trim_start_matches(':').trim())
        .unwrap_or(t)
}

/// "Editar": the plan becomes editable in place.
pub fn plan_edit(s: &mut Shell) {
    let Some(a) = &s.chat.pending_approval else {
        return;
    };
    let mut e = TextEdit::new(plan_body(&a.summary).to_string());
    e.focused = true;
    s.chat.plan_edit = Some(e);
}

/// Back from editing without keeping the changes.
pub fn plan_edit_cancel(s: &mut Shell) {
    s.chat.plan_edit = None;
}

/// "Aprovar e executar". An edited plan is recorded with the approval so the
/// runtime can be handed the edited text.
pub fn plan_approve(s: &mut Shell) {
    let Some(a) = s.chat.pending_approval.clone() else {
        return;
    };
    if let Some(e) = s.chat.plan_edit.take() {
        if e.text.trim() != plan_body(&a.summary) {
            // TODO(wire): edited plan. Expected: `ApprovalSender::answer_with(id,
            // Approval::AllowOnce, Some(edited_plan))`, which the plan gate
            // swaps into the `exit_plan_mode` result. Until then the approval
            // goes through unchanged and the edit is kept in `plan_edits`.
            s.chat
                .plan_edits
                .push((a.id.clone(), e.text.trim().to_string()));
        }
    }
    decide(s, Decision::Once);
    // The gate leaves `plan` itself; mirror it when the handle did not move.
    if s.chat.permission_mode() == PermissionMode::Plan {
        s.chat.status = "Plano aprovado".into();
    }
}

/// "Continuar planejando": refuses the plan, the model keeps planning.
pub fn plan_continue(s: &mut Shell) {
    s.chat.plan_edit = None;
    decide(s, Decision::Deny);
    s.chat.status = "Continuando o plano".into();
}

// ------------------------------------------------------------- question

pub fn question_pick(s: &mut Shell, q: usize, option: usize) {
    if let Some(state) = &mut s.chat.question {
        state.pick(q, option);
    }
}

// ----------------------------------------------------------- tasks panel

pub fn toggle_tasks(s: &mut Shell) {
    s.chat.tasks_open = !s.chat.tasks_open;
}

// --------------------------------------------------------------- rewind

/// "Rebobinar": restores the latest checkpoint (files and conversation).
pub fn rewind(s: &mut Shell) {
    let Some(id) = s.chat.selected.clone() else {
        return;
    };
    if s.chat.busy {
        return;
    }
    let db = s.db().to_path_buf();
    s.work(
        move || wire::rewind_latest(&db, &id),
        |s, r| match r {
            Ok(msg) => {
                s.chat.status = msg;
                s.load_selected();
            }
            Err(e) => s.chat.status = e,
        },
    );
}

// ---------------------------------------------------------------- popups

/// The `/` or `@` token under the caret, unless Esc closed it for this text.
pub fn active(s: &Shell) -> Option<Active> {
    let c = &s.chat;
    if c.popup_dismissed.as_deref() == Some(c.draft.text.as_str()) {
        return None;
    }
    agentwork::trigger(&c.draft.text, c.draft.range().1)
}

/// The rows the open popup lists.
pub enum Rows {
    Slash(Vec<SlashCommand>),
    Files(Vec<String>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Rows::Slash(v) => v.len(),
            Rows::Files(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Most rows the file popup lists.
pub const FILE_ROWS: usize = 50;

pub fn rows(s: &Shell, a: &Active) -> Rows {
    match a.trigger {
        Trigger::Slash => Rows::Slash(agentwork::filter_slash(&a.query)),
        Trigger::File => Rows::Files(
            s.chat
                .files
                .ready()
                .map(|f| {
                    agentwork::filter_files(f, &a.query, FILE_ROWS)
                        .into_iter()
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        ),
    }
}

/// Lists the working directory once, off the UI thread, the first time `@`
/// is typed. A failure is shown in the popup and retried on the next `@`.
pub fn ensure_files(s: &mut Shell) {
    if matches!(s.chat.files, crate::Load::Loading | crate::Load::Ready(_)) {
        return;
    }
    s.chat.files = crate::Load::Loading;
    s.work(
        || {
            std::env::current_dir()
                .map(|d| agentwork::list_files(&d, 5_000))
                .map_err(|e| e.to_string())
        },
        |s, r| s.chat.files = r.into(),
    );
}

pub fn popup_move(s: &mut Shell, delta: isize) {
    let Some(a) = active(s) else { return };
    let n = rows(s, &a).len();
    if n == 0 {
        return;
    }
    let cur = s.chat.popup_active.min(n - 1) as isize;
    s.chat.popup_active = (cur + delta).rem_euclid(n as isize) as usize;
}

pub fn popup_close(s: &mut Shell) {
    s.chat.popup_dismissed = Some(s.chat.draft.text.clone());
}

/// Enter or Tab on the highlighted row.
pub fn popup_accept(s: &mut Shell) -> bool {
    let Some(a) = active(s) else { return false };
    let rows = rows(s, &a);
    if rows.is_empty() {
        return false;
    }
    let i = s.chat.popup_active.min(rows.len() - 1);
    match rows {
        Rows::Slash(v) => run_command(s, v[i].name),
        Rows::Files(v) => insert_file(s, &a, &v[i]),
    }
    true
}

/// Puts `@path ` where the token was.
pub fn insert_file(s: &mut Shell, a: &Active, path: &str) {
    let d = &mut s.chat.draft;
    d.set_selection(a.start, a.end);
    d.insert(&format!("@{path} "));
    d.focused = true;
    s.chat.popup_active = 0;
}

/// Runs a slash command and clears the draft it came from.
pub fn run_command(s: &mut Shell, name: &str) {
    s.chat.draft = TextEdit::new("");
    s.chat.popup_active = 0;
    s.chat.draft.focused = true;
    match name {
        "compact" => compact(s),
        "context" => {
            s.chat.status = match s.chat.meter(s.providers.active().context_limit) {
                Some(m) => match m.fraction() {
                    Some(f) => format!("Contexto: {} ({:.0}%)", m.label(), f * 100.0),
                    None => format!("Contexto: {}", m.label()),
                },
                None => "Contexto: nenhum turno ainda".into(),
            }
        }
        "plan" => set_mode(s, PermissionMode::Plan),
        "clear" => new_chat(s),
        "model" => {
            s.chat.model_menu = true;
            super::fetch_models(s);
        }
        "resume" => s.navigate(Screen::History),
        "agents" => {
            s.chat.agent_menu = true;
            if s.agents.list.is_idle() {
                s.load_agents();
            }
        }
        "skills" => s.navigate(Screen::Skills),
        "memory" => s.navigate(Screen::Memory),
        _ => {}
    }
}

fn compact(s: &mut Shell) {
    let Some(id) = s.chat.selected.clone() else {
        s.chat.status = "Nada para compactar ainda".into();
        return;
    };
    if s.chat.busy || s.chat.compacting {
        s.chat.status = "Espere o turno terminar para compactar".into();
        return;
    }
    if s.chat.model.is_empty() {
        s.chat.status = "Escolha um modelo antes de compactar".into();
        return;
    }
    s.chat.compacting = true;
    let (db, cfg, model) = (
        s.db().to_path_buf(),
        s.providers.active(),
        s.chat.model.clone(),
    );
    s.work(
        move || wire::compact_now(&db, cfg, &id, &model),
        |s, r| {
            s.chat.compacting = false;
            match r {
                Ok(0) => s.chat.status = "Nada para compactar ainda".into(),
                Ok(n) => {
                    s.chat.lines.push(crate::screens::chat::Line::item(
                        crate::agentwork::Item::Note(format!(
                            "Contexto compactado: {n} mensagens resumidas"
                        )),
                    ));
                    s.chat.status.clear();
                }
                Err(e) => s.chat.status = format!("Não consegui compactar: {e}"),
            }
        },
    );
}
