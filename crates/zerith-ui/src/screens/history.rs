//! Every conversation, searchable, grouped by day.

use rune::{col, el, row, text, El, GrowthSpec, LocalTime, ScrollSpec, TextEdit, Tx};
use rune_kit::SegmentedControl;

use crate::ui::{self, Ui};
use crate::widgets::{self, button, eyebrow, field, icon_button, notice, Load, Tone};
use crate::{wire, Row, Shell};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    Native,
    Hermes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

impl Bucket {
    pub fn label(self) -> &'static str {
        match self {
            Bucket::Today => "Hoje",
            Bucket::Yesterday => "Ontem",
            Bucket::ThisWeek => "Esta semana",
            Bucket::Older => "Mais antigas",
        }
    }
}

/// Days since the epoch on the local calendar, from a `LocalTime`.
fn day_number(t: LocalTime) -> i64 {
    // Howard Hinnant's days_from_civil.
    let (y, m, d) = (t.year as i64, t.month as i64, t.day as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Which group a timestamp falls in, on a clock `offset` seconds east of UTC.
/// Pure, so the grouping is testable without the machine's time zone.
pub fn day_bucket(at_ms: i64, now_ms: i64, offset: i32) -> Bucket {
    let today = day_number(LocalTime::at_offset(now_ms, offset));
    let day = day_number(LocalTime::at_offset(at_ms, offset));
    match today - day {
        i64::MIN..=0 => Bucket::Today,
        1 => Bucket::Yesterday,
        2..=6 => Bucket::ThisWeek,
        _ => Bucket::Older,
    }
}

/// Rows grouped by bucket, in bucket order, keeping each group's input order.
pub fn group_by_day<'a>(rows: &[&'a Row], now_ms: i64, offset: i32) -> Vec<(Bucket, Vec<&'a Row>)> {
    let mut out: Vec<(Bucket, Vec<&Row>)> = Vec::new();
    for r in rows {
        let b = day_bucket(r.updated_at, now_ms, offset);
        match out.iter_mut().find(|(k, _)| *k == b) {
            Some((_, v)) => v.push(r),
            None => out.push((b, vec![r])),
        }
    }
    out.sort_by_key(|(b, _)| *b);
    out
}

/// The filter, as a pure function: title substring or a content hit, source,
/// archived.
pub fn filter_rows<'a>(
    rows: &'a [Row],
    query: &str,
    hits: Option<&[String]>,
    source: SourceFilter,
    archived: bool,
) -> Vec<&'a Row> {
    let q = query.trim().to_lowercase();
    rows.iter()
        .filter(|r| r.archived == archived)
        .filter(|r| match source {
            SourceFilter::All => true,
            SourceFilter::Native => r.source == "native",
            SourceFilter::Hermes => r.source == "hermes",
        })
        .filter(|r| {
            q.is_empty()
                || r.title.to_lowercase().contains(&q)
                || hits.is_some_and(|h| h.contains(&r.id))
        })
        .collect()
}

/// One entry of the flattened, virtualized list: a day header or a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Header(Bucket),
    Session(Row),
}

#[derive(Default)]
pub struct HistoryState {
    pub query: TextEdit,
    pub source: Option<SourceFilter>,
    pub archived: bool,
    /// Session ids whose messages matched the last content search.
    pub hits: Load<(String, Vec<String>)>,
    /// What the list shows now, flattened for `virtual_rows`.
    pub items: Vec<Item>,
    pub notice: Option<Result<String, String>>,
}

impl HistoryState {
    pub fn source(&self) -> SourceFilter {
        self.source.unwrap_or(SourceFilter::All)
    }

    /// Recomputes the visible list from the loaded sessions.
    pub fn rebuild(&mut self, sessions: &[Row]) {
        let q = self.query.text.clone();
        let hits = match &self.hits {
            Load::Ready((for_q, ids)) if *for_q == q => Some(ids.as_slice()),
            _ => None,
        };
        let rows = filter_rows(sessions, &q, hits, self.source(), self.archived);
        let now = now_ms();
        let offset = LocalTime::at(now).offset;
        self.items.clear();
        for (b, group) in group_by_day(&rows, now, offset) {
            self.items.push(Item::Header(b));
            self.items
                .extend(group.into_iter().map(|r| Item::Session(r.clone())));
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Runs the content search. Titles filter as you type; Enter also searches
/// message text, on a worker.
pub fn search(s: &mut Shell) {
    let q = s.history.query.text.trim().to_string();
    if q.is_empty() {
        s.history.hits = Load::Idle;
        s.history.rebuild(&s.sessions);
        return;
    }
    s.history.hits = Load::Loading;
    let db = s.db().to_path_buf();
    s.work(
        move || (q.clone(), wire::search_sessions(&db, &q, 500)),
        |s, (q, r)| {
            s.history.hits = r.map(|ids| (q, ids)).into();
            s.history.rebuild(&s.sessions);
        },
    );
}

pub fn export(s: &mut Shell, id: String) {
    let db = s.db().to_path_buf();
    s.work(
        move || wire::export_session(&db, &id),
        |s, r| {
            s.history.notice = Some(match r {
                Ok(p) => Ok(format!("Exportado para {}", p.display())),
                Err(e) => Err(format!("Não consegui exportar: {e}")),
            })
        },
    );
}

pub fn archive(s: &mut Shell, id: String, to: bool) {
    let db = s.db().to_path_buf();
    let id2 = id.clone();
    s.work(
        move || wire::set_archived(&db, &id2, to),
        move |s, r| match r {
            Ok(()) => {
                if let Some(row) = s.sessions.iter_mut().find(|r| r.id == id) {
                    row.archived = to;
                }
                if to && s.chat.selected.as_deref() == Some(id.as_str()) {
                    s.chat.selected = None;
                    s.load_selected();
                }
                s.history.rebuild(&s.sessions);
                s.history.notice = Some(Ok(if to { "Arquivada" } else { "Restaurada" }.into()));
            }
            Err(e) => s.history.notice = Some(Err(format!("Não consegui arquivar: {e}"))),
        },
    );
}

fn session_row(r: &Row, archived_view: bool, u: &Ui) -> El {
    let t = LocalTime::at(r.updated_at);
    let open_id = r.id.clone();
    let export_id = r.id.clone();
    let arch_id = r.id.clone();
    row()
        .w_full()
        .h(40.0)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .hover(u.hover)
        .role("button")
        .label(format!("Abrir {}", r.title))
        .focusable(true)
        .on_press(move |s: &mut Shell| s.select_session(&open_id))
        .child(
            text(r.title.clone())
                .font(ui::BODY)
                .color(u.content)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(if r.source == "native" {
            el()
        } else {
            widgets::pill(r.source.clone(), u)
        })
        .child(
            text(format!(
                "{:02}/{:02} {:02}:{:02}",
                t.day, t.month, t.hour, t.minute
            ))
            .font(ui::LABEL)
            .color(u.faint),
        )
        .child(icon_button(
            format!("export-{}", r.id),
            format!("Exportar {}", r.title),
            ui::icon_export(),
            u,
            move |s| export(s, export_id.clone()),
        ))
        .child(icon_button(
            format!("archive-{}", r.id),
            if archived_view {
                format!("Restaurar {}", r.title)
            } else {
                format!("Arquivar {}", r.title)
            },
            ui::icon_archive(),
            u,
            move |s| archive(s, arch_id.clone(), !archived_view),
        ))
}

pub fn view(s: &Shell, u: &Ui) -> El {
    let h = &s.history;
    let kit = ui::kit(u);
    let sources = ["Todas", "Nativas", "Hermes"];
    let src_idx = match h.source() {
        SourceFilter::All => 0,
        SourceFilter::Native => 1,
        SourceFilter::Hermes => 2,
    };
    let filters = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(
            field(
                "history-q",
                &h.query,
                "Buscar conversas (Ctrl+K)",
                "Buscar por título; Enter busca no conteúdo",
                Some(ui::icon_search()),
                u,
                |s: &mut Shell| &mut s.history.query,
                |i| {
                    i.on_submit(|s: &mut Shell, _: &mut Tx| search(s))
                        .on_input_change(|s: &mut Shell, _: &mut Tx| {
                            // Titles filter as you type; content search waits for Enter.
                            s.history.rebuild(&s.sessions);
                        })
                },
            )
            .grow(1.0)
            .min_w(0.0),
        )
        .child(
            SegmentedControl::new(&sources, Some(src_idx))
                .label("Origem")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    s.history.source = Some(
                        [
                            SourceFilter::All,
                            SourceFilter::Native,
                            SourceFilter::Hermes,
                        ][i],
                    );
                    s.history.rebuild(&s.sessions);
                })
                .build(&kit),
        )
        .child(
            SegmentedControl::new(&["Ativas", "Arquivadas"], Some(h.archived as usize))
                .label("Arquivo")
                .fit()
                .on_pick(|s: &mut Shell, i: usize, _: &mut Tx| {
                    s.history.archived = i == 1;
                    s.history.rebuild(&s.sessions);
                })
                .build(&kit),
        );

    let mut top = col().w_full().gap(10.0).child(filters);
    match &h.hits {
        Load::Loading => {
            top = top.child(text("Buscando no conteúdo…").font(ui::LABEL).color(u.faint))
        }
        Load::Failed(e) => top = top.child(widgets::error_line(format!("Busca falhou: {e}"), u)),
        Load::Ready((q, ids)) => {
            top = top.child(
                text(format!("{} conversa(s) mencionam \"{q}\"", ids.len()))
                    .font(ui::LABEL)
                    .color(u.faint),
            )
        }
        Load::Idle => {}
    }
    match &h.notice {
        Some(Ok(m)) => {
            top = top.child(
                text(m.clone())
                    .font(ui::LABEL)
                    .color(u.muted_text)
                    .selectable(true),
            )
        }
        Some(Err(m)) => top = top.child(widgets::error_line(m.clone(), u)),
        None => {}
    }

    let body = if h.items.is_empty() {
        let (t, b) = if s.sessions.is_empty() {
            (
                "Nenhuma conversa",
                "As conversas que você tiver aparecem aqui, agrupadas por dia.",
            )
        } else {
            (
                "Nada encontrado",
                "Nenhuma conversa corresponde aos filtros.",
            )
        };
        notice(t, b, u)
    } else {
        let archived_view = h.archived;
        let uu = *u;
        let more = s.sessions.len() >= s.sessions_limit;
        let mut c = col().w_full().child(
            col()
                .key("history-rows")
                .w_full()
                .role("list")
                .label("Conversas")
                .growth(GrowthSpec::default())
                .virtual_rows(
                    h.items.len(),
                    40.0,
                    |s: &Shell, i| match &s.history.items[i] {
                        Item::Header(b) => format!("h-{}", b.label()).into(),
                        Item::Session(r) => format!("r-{}", r.id).into(),
                    },
                    move |s: &Shell, i| match &s.history.items[i] {
                        Item::Header(b) => row()
                            .w_full()
                            .pt(if i == 0 { 4.0 } else { 18.0 })
                            .pb(6.0)
                            .px(10.0)
                            .child(eyebrow(b.label(), &uu)),
                        Item::Session(r) => session_row(r, archived_view, &uu),
                    },
                ),
        );
        if more {
            c = c.child(row().w_full().py(12.0).justify_center().child(button(
                "history-more",
                "Carregar mais",
                Tone::Plain,
                true,
                u,
                |s, _| s.more_sessions(),
            )));
        }
        c.scroll(ScrollSpec {
            stick_bottom: false,
        })
    };

    widgets::page(
        "Histórico",
        text(format!("{} conversas", s.sessions.len()))
            .font(ui::LABEL)
            .color(u.faint),
        col()
            .w_full()
            .gap(14.0)
            .child(top)
            .child(body.grow(1.0).min_h(0.0)),
        u,
    )
}
