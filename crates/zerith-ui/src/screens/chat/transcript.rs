//! The transcript: messages, tool one-liners, subagent rows and notes.
//!
//! Calm by construction: no boxes around messages, hairlines instead of
//! frames, one muted fill for the things that hold code or text to read.

use rune::{col, el, icon, row, rule, text, El, GrowthSpec, Icon, ScrollSpec};
use rune_kit::Markdown;

use std::path::PathBuf;

use super::{Line, Shell};
use crate::agentwork::{
    format_duration, turn_changes, DiffKind, DiffSummary, Item, SubState, Subagent, ToolRow,
    ToolState, TurnChanges,
};
use crate::mediaout::{self, Clip, Images, MediaOut, Picture};
use crate::ui::{self, Ui};
use crate::widgets::{self, button, Tone};

/// A mark for a running state: a small ring with a turning arc (the kit's
/// pixel-grid spinner turns to a blob at this size). Never colour alone:
/// callers also put the state in the row's label.
pub fn spinner(u: &Ui, color: rune::Color) -> El {
    widgets::spinner(color, 14.0, u)
}

/// What the transcript needs to show generated media: the folder it may show
/// files from and the decoder for them.
#[derive(Clone)]
pub struct MediaCtx {
    pub dir: PathBuf,
    pub images: std::rc::Rc<Images>,
}

impl MediaCtx {
    pub fn of(s: &Shell) -> Self {
        Self {
            dir: s.media_dir.clone(),
            images: s.images.clone(),
        }
    }
}

/// Longest side, in points, a generated picture is drawn at in the chat.
const PICTURE_MAX: f32 = 360.0;

fn open_file(path: PathBuf) -> impl Fn(&mut Shell) + 'static {
    move |s: &mut Shell| {
        s.chat.status = match crate::wire::open_media(&s.media_dir, &path) {
            Ok(()) => String::new(),
            Err(e) => e,
        };
    }
}

/// A picture the user can click to open with the OS viewer. One still
/// decoding shows a quiet line with its alt text.
fn picture(p: &Picture, ctx: &MediaCtx, u: &Ui) -> El {
    let alt = if p.alt.is_empty() {
        "Imagem gerada".to_string()
    } else {
        p.alt.clone()
    };
    let image = ctx.images.get(&p.path);
    let size = image
        .size()
        .filter(|_| image.is_ready() && image.error().is_none());
    let Some([w, _]) = size else {
        return row()
            .role("image")
            .label(alt.clone())
            .h(28.0)
            .gap(8.0)
            .center_y()
            .child(
                text(if image.error().is_some() {
                    format!("Não consegui abrir a imagem: {alt}")
                } else {
                    format!("Carregando imagem: {alt}")
                })
                .font(ui::SMALL)
                .color(u.muted_text)
                .truncate(),
            );
    };
    el().key(format!("pic-{}", p.path.display()))
        .role("button")
        .label(format!("Abrir imagem: {alt}"))
        .focusable(true)
        .w_full()
        .max_w((w as f32).min(PICTURE_MAX))
        .pixels(image.pixels())
        .radius(ui::R_SM)
        .cursor(rune::Cursor::Pointer)
        .help("Abrir com o visualizador do sistema")
        .on_press(open_file(p.path.clone()))
}

fn video_card(c: &Clip, u: &Ui) -> El {
    let name = c
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let open = open_file(c.path.clone());
    row()
        .key(format!("clip-{name}"))
        .role("group")
        .label(format!("Vídeo gerado: {name} {}", c.size))
        .w_full()
        .max_w(PICTURE_MAX + 60.0)
        .gap(10.0)
        .px(12.0)
        .py(8.0)
        .center_y()
        .radius(ui::R_MD)
        .fill(u.muted)
        .border(1.0, u.line)
        .child(icon(ui::icon_media(), 16.0).color(u.muted_text))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(name.clone())
                        .font(ui::SMALL)
                        .color(u.content)
                        .truncate(),
                )
                .child(
                    text(if c.size.is_empty() {
                        "vídeo".to_string()
                    } else {
                        format!("vídeo · {}", c.size)
                    })
                    .font(ui::LABEL)
                    .color(u.muted_text),
                ),
        )
        .child(button(
            format!("clip-open-{name}"),
            "Abrir",
            Tone::Plain,
            true,
            u,
            move |s, _| open(s),
        ))
}

/// Pictures and clips of a generation result, under its tool row.
fn media_block(m: &MediaOut, ctx: &MediaCtx, u: &Ui) -> El {
    let mut c = col().w_full().gap(8.0);
    for p in &m.pictures {
        c = c.child(picture(p, ctx, u));
    }
    for v in &m.clips {
        c = c.child(video_card(v, u));
    }
    c
}

fn tool_icon(name: &str) -> &'static Icon {
    match name {
        "generate_image" | "generate_video" => ui::icon_media(),
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
fn tool_row(index: usize, t: &ToolRow, toggled: bool, ctx: &MediaCtx, u: &Ui) -> El {
    let title = mediaout::tool_title(&t.name);
    // Only a finished generation has files to show.
    let media = title
        .filter(|_| t.state == ToolState::Ok)
        .map(|_| mediaout::parse(&t.preview, &ctx.dir))
        .filter(|m| !m.is_empty());
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
            // A paid call: the cost is the number worth reading.
            (None, ms) if media.as_ref().is_some_and(|m| m.cost.is_some()) => {
                let cost = media
                    .as_ref()
                    .and_then(|m| m.cost.clone())
                    .unwrap_or_default();
                text(match ms {
                    Some(ms) => format!("Custo {cost} · {}", format_duration(ms)),
                    None => format!("Custo {cost}"),
                })
                .font(ui::LABEL)
                .color(u.muted_text)
            }
            (None, Some(ms)) => text(format_duration(ms)).font(ui::LABEL).color(u.faint),
            _ => el(),
        },
    };
    let head = disclosure(
        format!("tool-{}", t.id),
        format!("{} {}, {state_word}", title.unwrap_or(&t.name), t.arg),
        open,
        has_body,
        index,
        u,
        vec![
            chevron(open, has_body, u),
            el().size(20.0, 20.0)
                .radius(ui::R_XS)
                .fill(if t.state == ToolState::Failed {
                    u.danger_soft
                } else {
                    u.layer2
                })
                .center()
                .child(
                    icon(tool_icon(&t.name), 12.0).color(if t.state == ToolState::Failed {
                        u.danger
                    } else if t.state == ToolState::Running {
                        u.accent_text
                    } else {
                        u.muted_text
                    }),
                ),
            text(title.map_or_else(|| t.name.clone(), str::to_string))
                .font(ui::SMALL)
                .color(u.content),
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
    if let Some(m) = &media {
        // The files are the result: shown without opening the row.
        c = c.child(
            col()
                .w_full()
                .pl(28.0)
                .pt(4.0)
                .pb(8.0)
                .child(media_block(m, ctx, u)),
        );
    }
    if open && media.is_none() {
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
/// Parallel ones are consecutive rows with a clear left state bar.
fn subagent_row(index: usize, a: &Subagent, open: bool, u: &Ui) -> El {
    let state_word = match a.state {
        SubState::Running => "em execução",
        SubState::Done => "concluído",
        SubState::Error => "com erro",
    };
    let bar_color = match a.state {
        SubState::Running => u.accent,
        SubState::Done => u.done,
        SubState::Error => u.danger,
    };
    let border_color = match a.state {
        SubState::Running => u.accent.alpha(0.35),
        SubState::Error => u.danger.alpha(0.35),
        SubState::Done => u.line,
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
            el().size(3.0, 14.0)
                .radius(1.5)
                .fill(bar_color)
                .a11y_hidden(),
            sub_mark(a.state, u),
            text(a.name.clone()).font(ui::TITLE).color(u.content),
            widgets::pill("subagente", u),
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
        .fill(u.muted)
        .border(1.0, border_color)
        .py(2.0)
        .px(4.0)
        .child(head);
    if open && has_body {
        card = card.child(rule(1.0, u.line).w_full().my(2.0)).child(
            text(a.summary.clone())
                .wrap()
                .font(ui::SMALL)
                .color(u.secondary)
                .selectable(true)
                .px(12.0)
                .pb(6.0)
                .pt(2.0),
        );
    }
    col().w_full().pb(4.0).child(card)
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

#[allow(clippy::too_many_arguments)]
pub fn line_row(
    index: usize,
    line: &Line,
    u: &Ui,
    live: bool,
    open: bool,
    rewind: bool,
    changes: Option<TurnChanges>,
    ctx: &MediaCtx,
) -> El {
    if let Some(item) = &line.item {
        return match item {
            Item::Tool(t) => tool_row(index, t, open, ctx, u),
            Item::Subagent(a) => subagent_row(index, a, open, u),
            Item::Note(n) => note_row(n, u),
        };
    }
    match line.role {
        "user" => {
            let mut c = col().w_full().pb(8.0).items_end().gap(4.0).child(
                col()
                    .max_w(580.0)
                    .py(8.0)
                    .px(12.0)
                    .radius(ui::R_LG)
                    .fill(u.muted)
                    .border(1.0, u.line)
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
            .pb(8.0)
            .child(widgets::error_line(line.content.clone(), u)),
        "assistant" => {
            let body = if line.content.is_empty() {
                "…".to_string()
            } else {
                line.content.clone()
            };
            // Images in an answer: local files under the media folder only
            // (the generate tools tell the model to paste them). Anything
            // else, a URL included, stays alt text: the window never fetches.
            let (dir, images) = (ctx.dir.clone(), ctx.images.clone());
            col().w_full().pb(8.0).child(
                Markdown::new(body)
                    .streaming(live)
                    .selectable(true)
                    .images(move |src| images.for_markdown(&dir, src))
                    .image_frame(move |e, f| {
                        let alt = if f.alt.is_empty() {
                            "Imagem gerada"
                        } else {
                            f.alt
                        };
                        e.max_w(PICTURE_MAX)
                            .role("button")
                            .label(format!("Abrir imagem: {alt}"))
                            .focusable(true)
                            .cursor(rune::Cursor::Pointer)
                            .on_press(open_file(PathBuf::from(f.src.trim())))
                    })
                    .build(&ui::kit(u)),
            )
        }
        _ => col()
            .w_full()
            .pb(8.0)
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

fn build_row(
    s: &Shell,
    i: usize,
    busy: bool,
    last_user: Option<usize>,
    ctx: &MediaCtx,
    u: &Ui,
) -> El {
    let line = &s.chat.lines[i];
    let live = busy && i + 1 == s.chat.lines.len();
    // Rebobinar sits under the last user message only: the
    // latest checkpoint is that turn's.
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
        u,
        live,
        s.chat.open.contains(&i),
        rewind,
        changes,
        ctx,
    )
}

/// The scrolling message list. Uses exact layout measurement for normal-sized
/// conversations so expanded diffs and subagents never clip at the top, falling
/// back to `virtual_rows` only on very long transcripts.
pub fn list(s: &Shell, u: &Ui) -> El {
    let c = &s.chat;
    let n = c.lines.len();
    let busy = c.busy;
    let uu = *u;
    let ctx = MediaCtx::of(s);
    let last_user = c.lines.iter().rposition(|l| l.role == "user");
    let mut inner = col().w_full().pt(2.0).pb(4.0);
    if c.more {
        inner = inner.child(row().w_full().pb(14.0).justify_center().child(button(
            "older",
            "Carregar mensagens anteriores",
            Tone::Plain,
            true,
            u,
            |s, _| super::load_all(s),
        )));
    }
    if n <= 120 {
        let mut messages = col()
            .key("transcript")
            .w_full()
            .gap(2.0)
            .role("log")
            .label("Mensagens");
        for i in 0..n {
            messages = messages.child(build_row(s, i, busy, last_user, &ctx, &uu));
        }
        inner = inner.child(messages);
    } else {
        inner = inner.child(
            col()
                .key("transcript")
                .w_full()
                .growth(GrowthSpec::default())
                .virtual_rows(
                    n,
                    64.0,
                    |_: &Shell, i| format!("line-{i}").into(),
                    move |s: &Shell, i| build_row(s, i, busy, last_user, &ctx, &uu),
                ),
        );
    }
    inner
        .scroll(ScrollSpec { stick_bottom: true })
        // The newest content stays at the bottom; a soft fade at the edges
        // signals continuing messages without slicing headers mid-line.
        .scroll_edge_fade(10.0)
        .grow(1.0)
        .min_h(0.0)
}
