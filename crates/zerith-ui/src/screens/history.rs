//! Every conversation, searchable, grouped by day.

use std::collections::HashSet;
use std::path::Path;

use rune::{col, el, icon, row, rule, text, El, GrowthSpec, LocalTime, ScrollSpec, TextEdit, Tx};
use rune_kit::{SegmentedControl, Spinner, Switch};
use zerith_importers::{ImportReport, SessionPreview, SourcePreview};

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

pub struct HistoryState {
    pub query: TextEdit,
    pub source: Option<SourceFilter>,
    pub archived: bool,
    /// Session ids whose messages matched the last content search.
    pub hits: Load<(String, Vec<String>)>,
    /// What the list shows now, flattened for `virtual_rows`.
    pub items: Vec<Item>,
    pub notice: Option<Result<String, String>>,
    pub import_open: bool,
    pub import_path: TextEdit,
    pub import_preview: Load<Vec<SourcePreview>>,
    pub import_selected_source: usize,
    pub import_include_archived: bool,
    pub import_picked: HashSet<String>,
    pub import_running: bool,
    pub import_report: Option<Result<ImportReport, String>>,
}

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            query: TextEdit::default(),
            source: None,
            archived: false,
            hits: Load::Idle,
            items: Vec::new(),
            notice: None,
            import_open: false,
            import_path: TextEdit::default(),
            import_preview: Load::Idle,
            import_selected_source: 0,
            import_include_archived: true,
            import_picked: HashSet::new(),
            import_running: false,
            import_report: None,
        }
    }
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

fn active_import_source(h: &HistoryState) -> Option<&SourcePreview> {
    h.import_preview
        .ready()
        .and_then(|list| list.get(h.import_selected_source))
}

fn picked_for_source(src: &SourcePreview, include_archived: bool) -> HashSet<String> {
    src.sessions
        .iter()
        .filter(|sess| include_archived || !sess.archived)
        .map(|sess| sess.id.clone())
        .collect()
}

pub fn toggle_import(s: &mut Shell) {
    s.history.import_open = !s.history.import_open;
    if s.history.import_open && s.history.import_preview.is_idle() {
        scan_hermes(s);
    }
}

pub fn scan_hermes(s: &mut Shell) {
    s.history.import_preview = Load::Loading;
    s.history.import_report = None;
    s.work(wire::detect_hermes, |s, list| {
        s.history.import_selected_source = 0;
        let include_archived = s.history.import_include_archived;
        s.history.import_picked = list
            .first()
            .map(|src| picked_for_source(src, include_archived))
            .unwrap_or_default();
        s.history.import_preview = Load::Ready(list);
    });
}

pub fn preview_custom_path(s: &mut Shell) {
    let p = s.history.import_path.text.trim().to_string();
    s.history.import_report = None;
    if p.is_empty() {
        s.history.import_picked.clear();
        s.history.import_preview =
            Load::Failed("Informe o caminho do arquivo state.db ou .jsonl".into());
        return;
    }
    s.history.import_preview = Load::Loading;
    s.work(
        move || wire::preview_hermes(Path::new(&p)),
        |s, r| match r {
            Ok(prev) => {
                let include_archived = s.history.import_include_archived;
                s.history.import_selected_source = 0;
                s.history.import_picked = picked_for_source(&prev, include_archived);
                s.history.import_preview = Load::Ready(vec![prev]);
            }
            Err(e) => {
                s.history.import_picked.clear();
                s.history.import_preview = Load::Failed(e);
            }
        },
    );
}

pub fn select_import_source(s: &mut Shell, idx: usize) {
    let include_archived = s.history.import_include_archived;
    let picked = match s
        .history
        .import_preview
        .ready()
        .and_then(|list| list.get(idx))
    {
        Some(src) => picked_for_source(src, include_archived),
        None => return,
    };
    s.history.import_selected_source = idx;
    s.history.import_picked = picked;
    s.history.import_report = None;
}

pub fn toggle_import_session(s: &mut Shell, id: &str) {
    if !s.history.import_picked.remove(id) {
        s.history.import_picked.insert(id.to_string());
    }
}

pub fn toggle_include_archived(s: &mut Shell) {
    s.history.import_include_archived = !s.history.import_include_archived;
    if !s.history.import_include_archived {
        let archived_ids: Vec<String> = active_import_source(&s.history)
            .map(|src| {
                src.sessions
                    .iter()
                    .filter(|sess| sess.archived)
                    .map(|sess| sess.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in archived_ids {
            s.history.import_picked.remove(&id);
        }
    }
}

pub fn toggle_import_all(s: &mut Shell) {
    let include_archived = s.history.import_include_archived;
    let Some(src) = active_import_source(&s.history) else {
        return;
    };
    let eligible: Vec<String> = src
        .sessions
        .iter()
        .filter(|sess| include_archived || !sess.archived)
        .map(|sess| sess.id.clone())
        .collect();
    if eligible.is_empty() {
        s.history.import_picked.clear();
        return;
    }
    let all_picked = eligible
        .iter()
        .all(|id| s.history.import_picked.contains(id));
    if all_picked {
        s.history.import_picked.clear();
    } else {
        s.history.import_picked = eligible.into_iter().collect();
    }
}

pub fn run_import(s: &mut Shell) {
    if s.history.import_running {
        return;
    }
    let include_archived = s.history.import_include_archived;
    let Some(src) = active_import_source(&s.history) else {
        return;
    };
    let source_path = src.layout.path.clone();
    let profile = src.layout.profile.clone();
    let only_ids: Vec<String> = src
        .sessions
        .iter()
        .filter(|sess| {
            (include_archived || !sess.archived) && s.history.import_picked.contains(&sess.id)
        })
        .map(|sess| sess.id.clone())
        .collect();
    s.history.import_running = true;
    s.history.import_report = None;
    let db = s.db().to_path_buf();
    s.work(
        move || {
            wire::run_hermes_import(
                &db,
                &source_path,
                &profile,
                include_archived,
                Some(only_ids),
            )
        },
        |s, res| {
            s.history.import_running = false;
            let ok = res.is_ok();
            s.history.import_report = Some(res);
            if ok {
                s.refresh_sessions();
            }
        },
    );
}

fn fmt_date(ms: Option<i64>) -> String {
    let Some(ms) = ms.filter(|&v| v > 0) else {
        return "—".to_string();
    };
    let t = LocalTime::at(ms);
    format!("{:02}/{:02}/{}", t.day, t.month, t.year)
}

fn import_session_row(sess: &SessionPreview, on: bool, u: &Ui) -> El {
    let sid = sess.id.clone();
    let mark = el()
        .size(16.0, 16.0)
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
        });
    let mut r = row()
        .key(format!("import-sess-{}", sess.id))
        .role("checkbox")
        .label(sess.title.clone())
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
        .on_press(move |s: &mut Shell| toggle_import_session(s, &sid))
        .child(mark)
        .child(
            text(sess.title.clone())
                .font(ui::SMALL)
                .color(u.content)
                .truncate()
                .grow(1.0)
                .min_w(0.0)
                .a11y_hidden(),
        );
    if let Some(m) = &sess.model {
        r = r.child(widgets::pill(m.clone(), u));
    }
    if sess.archived {
        r = r.child(widgets::pill("arquivada", u));
    }
    r.child(
        text(format!("{} msg", sess.message_count))
            .font(ui::LABEL)
            .color(u.muted_text)
            .a11y_hidden(),
    )
    .child(
        text(fmt_date((sess.updated_at > 0).then_some(sess.updated_at)))
            .font(ui::LABEL)
            .color(u.faint)
            .a11y_hidden(),
    )
}

fn import_panel(h: &HistoryState, u: &Ui) -> El {
    let kit = ui::kit(u);
    let busy = h.import_running || h.import_preview.is_loading();
    let mut card = col()
        .key("history-import-panel")
        .role("region")
        .label("Importar conversas do Hermes")
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(eyebrow("Importar Hermes", u).grow(1.0))
                .child(icon_button(
                    "history-import-close",
                    "Fechar importação",
                    ui::icon_close(),
                    u,
                    toggle_import,
                )),
        )
        .child(
            col()
                .w_full()
                .gap(8.0)
                .child(
                    row()
                        .w_full()
                        .gap(10.0)
                        .center_y()
                        .child(button(
                            "history-import-detect",
                            "Detectar automaticamente",
                            Tone::Plain,
                            !busy,
                            u,
                            |s, _| scan_hermes(s),
                        ))
                        .child(el().grow(1.0))
                        .child(
                            Switch::new(h.import_include_archived)
                                .label("Incluir arquivadas")
                                .on_toggle(|s: &mut Shell| toggle_include_archived(s))
                                .build(&kit),
                        ),
                )
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(
                            field(
                                "import-path",
                                &h.import_path,
                                "Caminho do banco Hermes",
                                "~/.hermes/state.db ou arquivo .jsonl",
                                Some(ui::icon_folder()),
                                u,
                                |s: &mut Shell| &mut s.history.import_path,
                                |i| i.on_submit(|s: &mut Shell, _: &mut Tx| preview_custom_path(s)),
                            )
                            .grow(1.0)
                            .min_w(0.0),
                        )
                        .child(button(
                            "history-import-inspect",
                            "Inspecionar",
                            Tone::Plain,
                            !busy,
                            u,
                            |s, _| preview_custom_path(s),
                        )),
                ),
        );

    match &h.import_preview {
        Load::Idle => {}
        Load::Loading => {
            card = card.child(
                row()
                    .w_full()
                    .py(6.0)
                    .gap(8.0)
                    .center_y()
                    .role("status")
                    .label("Inspecionando histórico do Hermes")
                    .child(Spinner::new().size(14.0).color(u.faint).build(&kit))
                    .child(
                        text("Inspecionando fonte…")
                            .font(ui::SMALL)
                            .color(u.muted_text),
                    ),
            );
        }
        Load::Failed(e) => {
            card = card.child(widgets::error_line(e.clone(), u));
        }
        Load::Ready(sources) if sources.is_empty() => {
            card = card.child(
                text("Nenhuma instalação do Hermes encontrada automaticamente. Informe o caminho de um state.db ou .jsonl acima.")
                    .wrap()
                    .font(ui::SMALL)
                    .color(u.muted_text),
            );
        }
        Load::Ready(sources) => {
            if sources.len() > 1 {
                let mut picks = row().w_full().gap(6.0).flex_wrap();
                for (i, src) in sources.iter().enumerate() {
                    let active = i == h.import_selected_source;
                    picks = picks.child(
                        row()
                            .key(format!("history-import-src-{i}"))
                            .role("button")
                            .label(format!(
                                "Perfil {} ({} sessões)",
                                src.layout.profile,
                                src.sessions.len()
                            ))
                            .checked(active)
                            .focusable(true)
                            .h(26.0)
                            .px(10.0)
                            .center_y()
                            .radius(ui::R_SM)
                            .fill(if active {
                                u.accent_soft
                            } else {
                                rune::Color::TRANSPARENT
                            })
                            .border(1.0, if active { u.accent } else { u.line })
                            .hover(u.hover)
                            .press(u.active)
                            .on_press(move |s: &mut Shell| select_import_source(s, i))
                            .child(
                                text(format!("{} ({})", src.layout.profile, src.sessions.len()))
                                    .font(ui::LABEL)
                                    .color(if active { u.content } else { u.secondary }),
                            ),
                    );
                }
                card = card.child(picks);
            }

            if let Some(src) = sources.get(h.import_selected_source) {
                card = card.child(
                    text(format!(
                        "Perfil: {} · {} · {} sessão(ões) · {} mensagem(ns) · {} — {}",
                        src.layout.profile,
                        src.layout.path.display(),
                        src.sessions.len(),
                        src.total_messages,
                        fmt_date(src.min_date),
                        fmt_date(src.max_date),
                    ))
                    .wrap()
                    .font(ui::LABEL)
                    .color(u.muted_text)
                    .selectable(true),
                );

                let visible: Vec<&SessionPreview> = src
                    .sessions
                    .iter()
                    .filter(|sess| h.import_include_archived || !sess.archived)
                    .collect();
                let picked_count = visible
                    .iter()
                    .filter(|sess| h.import_picked.contains(&sess.id))
                    .count();

                card = card.child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(
                            text(format!(
                                "{} de {} selecionada(s)",
                                picked_count,
                                visible.len()
                            ))
                            .font(ui::LABEL)
                            .color(u.faint)
                            .grow(1.0),
                        )
                        .child(button(
                            "history-import-toggle-all",
                            "Selecionar todas / limpar",
                            Tone::Plain,
                            !visible.is_empty() && !h.import_running,
                            u,
                            |s, _| toggle_import_all(s),
                        )),
                );

                if visible.is_empty() {
                    card = card.child(
                        text(if src.sessions.is_empty() {
                            "Nenhuma sessão encontrada nesta fonte."
                        } else {
                            "Nenhuma sessão ativa encontrada (ative \"Incluir arquivadas\")."
                        })
                        .font(ui::SMALL)
                        .color(u.muted_text),
                    );
                } else {
                    let mut list = col()
                        .key("history-import-sessions")
                        .role("list")
                        .label("Sessões do Hermes")
                        .w_full()
                        .gap(2.0);
                    for sess in &visible {
                        let on = h.import_picked.contains(&sess.id);
                        list = list.child(import_session_row(sess, on, u));
                    }
                    card = card.child(rule(1.0, u.line).w_full()).child(
                        list.scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .max_h(160.0),
                    );
                }

                let mut footer = row().w_full().gap(10.0).center_y().child(button(
                    "history-import-run",
                    "Importar selecionadas",
                    Tone::Primary,
                    !h.import_running && !h.import_picked.is_empty(),
                    u,
                    |s, _| run_import(s),
                ));
                if h.import_running {
                    footer = footer.child(text("Importando…").font(ui::LABEL).color(u.muted_text));
                }
                card = card.child(footer);
            }
        }
    }

    match &h.import_report {
        Some(Ok(r)) => {
            card = card.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(widgets::dot(u.ok))
                    .child(
                        text(format!(
                            "{} importadas · {} retomadas · {} ignoradas · {} mensagens",
                            r.sessions_imported,
                            r.sessions_resumed,
                            r.sessions_skipped,
                            r.messages_imported
                        ))
                        .wrap()
                        .font(ui::LABEL)
                        .color(u.content)
                        .selectable(true)
                        .grow(1.0)
                        .min_w(0.0),
                    ),
            );
            for err in &r.failed {
                card = card.child(widgets::error_line(err.clone(), u));
            }
        }
        Some(Err(e)) => {
            card = card.child(widgets::error_line(format!("Importação falhou: {e}"), u));
        }
        None => {}
    }

    card
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

    let actions = row()
        .gap(10.0)
        .center_y()
        .child(
            text(format!("{} conversas", s.sessions.len()))
                .font(ui::LABEL)
                .color(u.faint),
        )
        .child(button(
            "history-import-btn",
            "Importar Hermes",
            Tone::Plain,
            true,
            u,
            |s, _| toggle_import(s),
        ));

    let mut content = col().w_full().gap(14.0);
    if h.import_open {
        content = content.child(import_panel(h, u));
    }
    content = content.child(top).child(body.grow(1.0).min_h(0.0));

    widgets::page("Histórico", actions, content, u)
}
