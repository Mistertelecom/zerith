//! What the turn remembers: facts grouped by kind, searchable, editable.

use rune::{col, el, row, text, El, GrowthSpec, ScrollSpec, TextEdit, Tx};
use rune_kit::Switch;
use zerith_memory::{Fact, FactKind, MemoryBackend, SqliteMemory};

use crate::ui::{self, Ui};
use crate::widgets::{self, eyebrow, field, icon_button, notice, Load};
use crate::{wire, Shell};

/// Facts read per load. Bounded so a huge memory never stalls a frame.
pub const FACT_PAGE: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Header(FactKind),
    Fact(Fact),
}

pub struct MemoryState {
    pub facts: Load<Vec<Fact>>,
    pub query: TextEdit,
    /// Ids recall returned for the last query, best first.
    pub hits: Option<Vec<i64>>,
    pub editing: Option<(i64, TextEdit)>,
    pub paused: bool,
    pub notice: Option<Result<String, String>>,
    pub items: Vec<Item>,
}

impl MemoryState {
    pub fn new(paused: bool) -> Self {
        Self {
            facts: Load::Idle,
            query: TextEdit::new(""),
            hits: None,
            editing: None,
            paused,
            notice: None,
            items: Vec::new(),
        }
    }

    pub fn rebuild(&mut self) {
        self.items.clear();
        let Some(facts) = self.facts.ready() else {
            return;
        };
        let shown: Vec<&Fact> = match &self.hits {
            Some(ids) => ids
                .iter()
                .filter_map(|id| facts.iter().find(|f| f.id == *id))
                .collect(),
            None => facts.iter().collect(),
        };
        for kind in KINDS {
            let group: Vec<&Fact> = shown.iter().copied().filter(|f| f.kind == kind).collect();
            if group.is_empty() {
                continue;
            }
            self.items.push(Item::Header(kind));
            self.items
                .extend(group.into_iter().cloned().map(Item::Fact));
        }
    }
}

const KINDS: [FactKind; 6] = [
    FactKind::Instruction,
    FactKind::Preference,
    FactKind::Fact,
    FactKind::Decision,
    FactKind::Project,
    FactKind::Entity,
];

fn kind_label(k: FactKind) -> &'static str {
    match k {
        FactKind::Preference => "Preferências",
        FactKind::Fact => "Fatos",
        FactKind::Decision => "Decisões",
        FactKind::Project => "Projetos",
        FactKind::Instruction => "Instruções",
        FactKind::Entity => "Entidades",
    }
}

pub fn load(s: &mut Shell) {
    s.memory.facts = Load::Loading;
    let db = s.db().to_path_buf();
    s.work(
        move || {
            SqliteMemory::open(&db)
                .and_then(|m| m.list(&wire::memory_scope(), FACT_PAGE))
                .map_err(|e| e.to_string())
        },
        |s, r| {
            s.memory.facts = r.into();
            s.memory.rebuild();
        },
    );
}

/// Flips the database pause switch on a worker; the switch shows the new
/// state at once and goes back if the write fails.
pub fn toggle_pause(s: &mut Shell) {
    let to = !s.memory.paused;
    s.memory.paused = to;
    let db = s.db().to_path_buf();
    s.work(
        move || wire::set_memory_paused(&db, to),
        move |s, r| {
            if let Err(e) = r {
                s.memory.paused = !to;
                s.memory.notice = Some(Err(format!("Não consegui pausar: {e}")));
            }
        },
    );
}

pub fn search(s: &mut Shell) {
    let q = s.memory.query.text.trim().to_string();
    if q.is_empty() {
        s.memory.hits = None;
        s.memory.rebuild();
        return;
    }
    let db = s.db().to_path_buf();
    s.work(
        move || {
            SqliteMemory::open(&db)
                .and_then(|m| m.recall(&wire::memory_scope(), &q, 100))
                .map_err(|e| e.to_string())
        },
        |s, r| {
            match r {
                Ok(hits) => s.memory.hits = Some(hits.into_iter().map(|h| h.fact.id).collect()),
                Err(e) => s.memory.notice = Some(Err(format!("Busca falhou: {e}"))),
            }
            s.memory.rebuild();
        },
    );
}

pub fn forget(s: &mut Shell, id: i64) {
    match SqliteMemory::open(s.db()).and_then(|m| m.forget(id)) {
        Ok(()) => {
            if let Load::Ready(v) = &mut s.memory.facts {
                v.retain(|f| f.id != id);
            }
            s.memory.notice = Some(Ok(format!("Fato {id} apagado")));
        }
        Err(e) => s.memory.notice = Some(Err(format!("Não consegui apagar: {e}"))),
    }
    s.memory.rebuild();
}

pub fn save_edit(s: &mut Shell) {
    let Some((id, edit)) = s.memory.editing.take() else {
        return;
    };
    let new = edit.text.trim().to_string();
    if new.is_empty() {
        return;
    }
    match SqliteMemory::open(s.db()).and_then(|m| m.update(id, &new)) {
        Ok(()) => {
            if let Load::Ready(v) = &mut s.memory.facts {
                if let Some(f) = v.iter_mut().find(|f| f.id == id) {
                    f.text = new;
                }
            }
            s.memory.notice = Some(Ok("Fato atualizado".into()));
        }
        Err(e) => s.memory.notice = Some(Err(format!("Não consegui salvar: {e}"))),
    }
    s.memory.rebuild();
}

fn fact_row(f: &Fact, editing: Option<&TextEdit>, u: &Ui) -> El {
    let id = f.id;
    if let Some(edit) = editing {
        return row()
            .w_full()
            .py(4.0)
            .gap(6.0)
            .center_y()
            .child(
                field(
                    format!("fact-edit-{id}"),
                    edit,
                    "Editar fato",
                    "",
                    None,
                    u,
                    |s: &mut Shell| &mut s.memory.editing.as_mut().expect("editing").1,
                    |i| i.on_submit(|s: &mut Shell, _: &mut Tx| save_edit(s)),
                )
                .grow(1.0)
                .min_w(0.0),
            )
            .child(icon_button(
                format!("fact-save-{id}"),
                "Salvar",
                ui::icon_check(),
                u,
                save_edit,
            ))
            .child(icon_button(
                format!("fact-cancel-{id}"),
                "Cancelar edição",
                ui::icon_close(),
                u,
                |s| s.memory.editing = None,
            ));
    }
    let text_for_edit = f.text.clone();
    row()
        .w_full()
        .min_h(36.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(8.0)
        .hover(u.hover)
        .child(
            text(f.text.clone())
                .wrap()
                .font(ui::BODY)
                .color(u.content)
                .selectable(true)
                .grow(1.0)
                .min_w(0.0),
        )
        .child(if f.confidence == zerith_memory::Confidence::Inferred {
            widgets::pill("inferido", u)
        } else {
            el()
        })
        .child(icon_button(
            format!("fact-e-{id}"),
            format!("Editar fato {id}"),
            ui::icon_edit(),
            u,
            move |s| {
                let mut e = TextEdit::new(text_for_edit.clone());
                e.focused = true;
                s.memory.editing = Some((id, e));
            },
        ))
        .child(icon_button(
            format!("fact-d-{id}"),
            format!("Apagar fato {id}"),
            ui::icon_trash(),
            u,
            move |s| s.forget(id),
        ))
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let m = &s.memory;
    let count = m.facts.ready().map_or(0, Vec::len);
    let status = format!(
        "SQLite · {} · {} fato(s){}",
        s.db().display(),
        count,
        if count >= FACT_PAGE {
            " (primeiros)"
        } else {
            ""
        }
    );
    let top = row()
        .w_full()
        .gap(14.0)
        .center_y()
        .child(
            field(
                "memory-q",
                &m.query,
                "Buscar na memória",
                "Buscar na memória; Enter usa a mesma busca do turno",
                Some(ui::icon_search()),
                u,
                |s: &mut Shell| &mut s.memory.query,
                |i| {
                    i.on_submit(|s: &mut Shell, _: &mut Tx| search(s))
                        .on_input_change(|s: &mut Shell, _: &mut Tx| {
                            if s.memory.query.text.trim().is_empty() && s.memory.hits.is_some() {
                                s.memory.hits = None;
                                s.memory.rebuild();
                            }
                        })
                },
            )
            .grow(1.0)
            .min_w(0.0),
        )
        .child(
            Switch::new(m.paused)
                .label("Pausar memória")
                .on_toggle(|s: &mut Shell| {
                    toggle_pause(s);
                })
                .build(&ui::kit(u)),
        );
    let mut head = col().w_full().gap(8.0).child(top).child(
        text(status)
            .font(ui::LABEL)
            .color(u.faint)
            .truncate()
            .selectable(true),
    );
    if m.paused {
        head = head.child(
            text("Pausada: novos turnos não consultam nem gravam fatos.")
                .font(ui::LABEL)
                .color(u.warn),
        );
    }
    match &m.notice {
        Some(Ok(t)) => head = head.child(text(t.clone()).font(ui::LABEL).color(u.muted_text)),
        Some(Err(t)) => head = head.child(widgets::error_line(t.clone(), u)),
        None => {}
    }

    let body = match &m.facts {
        Load::Idle | Load::Loading => widgets::loading("Carregando memória", u),
        Load::Failed(e) => col()
            .w_full()
            .gap(12.0)
            .child(widgets::error_line(
                format!(
                    "Memória indisponível: {}",
                    crate::folders::with_cause(&s.root(), e)
                ),
                u,
            ))
            .child(widgets::button(
                "memory-retry",
                "Tentar de novo",
                widgets::Tone::Plain,
                true,
                u,
                |s, _| s.load_facts(),
            )),
        Load::Ready(_) if m.items.is_empty() => {
            if m.hits.is_some() {
                notice("Nada encontrado", "Nenhum fato corresponde à busca.", u)
            } else {
                notice(
                    "Memória vazia",
                    "Diga \"lembre que …\" numa conversa para guardar um fato.",
                    u,
                )
            }
        }
        Load::Ready(_) => {
            let uu = *u;
            col()
                .w_full()
                .child(
                    col()
                        .key("facts")
                        .w_full()
                        .role("list")
                        .label("Fatos")
                        .growth(GrowthSpec::default())
                        .virtual_rows(
                            m.items.len(),
                            36.0,
                            |s: &Shell, i| match &s.memory.items[i] {
                                Item::Header(k) => format!("k-{}", k.as_str()).into(),
                                Item::Fact(f) => format!("f-{}", f.id).into(),
                            },
                            move |s: &Shell, i| match &s.memory.items[i] {
                                Item::Header(k) => row()
                                    .w_full()
                                    .pt(if i == 0 { 4.0 } else { 18.0 })
                                    .pb(6.0)
                                    .px(10.0)
                                    .child(eyebrow(kind_label(*k), &uu)),
                                Item::Fact(f) => {
                                    let editing = s
                                        .memory
                                        .editing
                                        .as_ref()
                                        .filter(|(id, _)| *id == f.id)
                                        .map(|(_, e)| e);
                                    fact_row(f, editing, &uu)
                                }
                            },
                        ),
                )
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
        }
    };
    widgets::page(
        "Memória",
        widgets::icon_button("memory-reload", "Recarregar", ui::icon_refresh(), u, |s| {
            s.load_facts()
        }),
        col()
            .w_full()
            .gap(14.0)
            .child(head)
            .child(body.grow(1.0).min_h(0.0)),
        u,
    )
}
