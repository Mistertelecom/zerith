//! The visual vocabulary: a bluish-neutral ladder, a monochrome primary, blue
//! only for selection, focus, links and the trajectory accent. Concepts and
//! plain colour values follow the DeepSeek Harness design-platform sheet
//! (neutral-bluish ladder, alpha hairlines, state colours); no brand asset is
//! used. Values are `#rrggbb` plus an alpha, because Rune authors colour in
//! OKLCH but accepts hex through `color::hex`.
//!
//! Nothing in this file draws; it only names colours, radii, motion curves,
//! type sizes and the icons the shell needs, so screens stay about behaviour.

use rune::color::hex;
use rune::{Color, CubicBezier, Curve, Easing, Icon, TextStyle};

#[derive(Clone, Copy)]
pub struct Ui {
    /// Which ladder this is; the kit and the theme toggle read it.
    pub dark: bool,
    /// The sheet is narrow (window under ~900 pt): screens stack instead of
    /// sitting side by side. Set by `view` from the measured sheet width.
    pub compact: bool,
    /// The window behind everything.
    pub canvas: Color,
    /// The frame the sidebar sits on.
    pub chrome: Color,
    /// `bg-base`: the content sheet.
    pub surface: Color,
    /// `bg-layer-1`: inputs, bubbles, code.
    pub muted: Color,
    /// `bg-layer-2`: raised rows and popups.
    pub layer2: Color,
    /// `bg-layer-3`: the highest fill.
    pub layer3: Color,
    /// The current navigation item or selected row.
    pub selected: Color,
    /// `interactive-bg-hover`: an alpha overlay, safe on any fill.
    pub hover: Color,
    /// `interactive-bg-active`: the pressed overlay.
    pub active: Color,
    /// `border-l2`: the hairline most borders use.
    pub line: Color,
    /// `border-l3`: a hairline that must read (focused field, open card).
    pub line_strong: Color,
    /// `label-primary`.
    pub content: Color,
    /// `label-secondary`.
    pub secondary: Color,
    /// `label-tertiary`.
    pub muted_text: Color,
    /// `label-caption`: placeholders, counters.
    pub faint: Color,
    /// Primary button fill: near-black on light, near-white on dark.
    pub primary: Color,
    /// Primary button hover and press.
    pub primary_press: Color,
    /// Ink on the primary fill.
    pub inverted: Color,
    /// Blue 500: selection, focus ring, trajectory accent.
    pub accent: Color,
    /// Links and accent text, legible on its ladder.
    pub accent_text: Color,
    /// Selection and current-step tint.
    pub accent_soft: Color,
    /// `state-success`. Marks only, never big fills.
    pub ok: Color,
    /// `state-warn`.
    pub warn: Color,
    /// Warning as text: `warn` is a mark colour and is too pale to read on the light sheet.
    pub warn_text: Color,
    /// `state-error`.
    pub danger: Color,
    /// `state-done` (violet): a finished subagent or plan.
    pub done: Color,
    /// `state-idle`: nothing happening.
    pub idle: Color,
    pub warn_soft: Color,
    pub danger_soft: Color,
    /// Diff added and removed line fills, and the marks beside them.
    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_add_ink: Color,
    pub diff_del_ink: Color,
}

fn c(s: &str) -> Color {
    hex(s).expect("a token in this file is not #rrggbb")
}

/// A hex colour with straight alpha: how hairlines and overlays are named.
fn a(s: &str, alpha: f32) -> Color {
    c(s).alpha(alpha)
}

/// Dark ladder: carbon-slate 950 base with restrained depth and WCAG 4.5+ labels.
pub fn dark() -> Ui {
    Ui {
        dark: true,
        compact: false,
        canvas: c("#0b0d11"),
        chrome: c("#090b0f"),
        surface: c("#0f1217"),
        muted: c("#161a22"),
        layer2: c("#1c212b"),
        layer3: c("#222834"),
        selected: c("#1a2030"),
        hover: a("#ffffff", 0.055),
        active: a("#ffffff", 0.095),
        line: a("#ffffff", 0.075),
        line_strong: a("#ffffff", 0.15),
        content: c("#f1f5f9"),
        secondary: c("#cbd5e1"),
        muted_text: c("#aab5c4"),
        faint: c("#8792a2"),
        primary: c("#f1f5f9"),
        primary_press: c("#dfe6ef"),
        inverted: c("#0f1217"),
        accent: c("#6366f1"),
        accent_text: c("#a5b4fc"),
        accent_soft: a("#6366f1", 0.16),
        ok: c("#22c55e"),
        warn: c("#f59e0b"),
        warn_text: c("#f59e0b"),
        danger: c("#f56e6e"),
        done: c("#a78bfa"),
        idle: c("#545b68"),
        warn_soft: a("#f59e0b", 0.12),
        danger_soft: a("#f56e6e", 0.12),
        diff_add: c("#172d22"),
        diff_del: c("#381c1d"),
        diff_add_ink: c("#41c977"),
        diff_del_ink: c("#f87171"),
    }
}

/// Light ladder: white base, layers `#f5f6f7` / `#f1f3f5` / `#ebeef2`.
pub fn light() -> Ui {
    Ui {
        dark: false,
        compact: false,
        canvas: c("#f9fafb"),
        chrome: c("#f9fafb"),
        surface: c("#ffffff"),
        muted: c("#f5f6f7"),
        layer2: c("#f1f3f5"),
        layer3: c("#ebeef2"),
        selected: c("#ebeef2"),
        hover: a("#263148", 0.06),
        active: a("#263148", 0.10),
        line: a("#000000", 0.08),
        line_strong: a("#000000", 0.14),
        content: c("#0f1115"),
        secondary: c("#4b5056"),
        muted_text: c("#5a5f65"),
        faint: c("#666b71"),
        primary: c("#0f1115"),
        primary_press: c("#43454a"),
        inverted: c("#ffffff"),
        accent: c("#4f46e5"),
        accent_text: c("#4338ca"),
        accent_soft: a("#4f46e5", 0.11),
        ok: c("#22c55e"),
        warn: c("#f59e0b"),
        warn_text: c("#a14a08"),
        danger: c("#d31212"),
        done: c("#7c3aed"),
        idle: c("#d4d4d4"),
        warn_soft: a("#f59e0b", 0.12),
        danger_soft: a("#d31212", 0.08),
        diff_add: c("#e6f4e7"),
        diff_del: c("#fce6e2"),
        diff_add_ink: c("#01a241"),
        diff_del_ink: c("#ba2723"),
    }
}

pub fn pick(dark_mode: bool) -> Ui {
    if dark_mode {
        dark()
    } else {
        light()
    }
}

/// Radii of the design scale: 4 / 8 / 12 / 16 / 20 / 28. The scale is kept
/// whole even where a step is unused today.
#[allow(dead_code)]
pub const R_XS: f32 = 4.0;
pub const R_SM: f32 = 8.0;
pub const R_MD: f32 = 12.0;
pub const R_LG: f32 = 16.0;
#[allow(dead_code)]
pub const R_XL: f32 = 20.0;
#[allow(dead_code)]
pub const R_PANEL: f32 = 28.0;

/// The one motion curve of the design: `cubic-bezier(0.4, 0, 0.2, 1)`.
pub const EASE: Easing = Easing::CubicBezier(CubicBezier::new(0.4, 0.0, 0.2, 1.0));
/// 100 ms: hover and press feedback.
pub const FAST: Curve = Curve::tween(0.1, EASE);
/// 200 ms: the default transition.
#[allow(dead_code)]
pub const BASE: Curve = Curve::tween(0.2, EASE);
/// 300 ms: panels and disclosure.
#[allow(dead_code)]
pub const SLOW: Curve = Curve::tween(0.3, EASE);

/// The kit `Markdown` and the kit's controls read their colours from. Starts
/// from Rune's neutral kit and overwrites the roles they paint (text, code
/// surface, rules, links, focus) with this file's tokens, so an answer and a
/// popup match the window.
pub fn kit(u: &Ui) -> rune_kit::Kit {
    let mut k = rune_kit::Kit::neutral(u.dark);
    let p = &mut k.c;
    p.page = u.surface;
    p.bg_base = u.surface;
    p.bg_deep = u.chrome;
    p.layer_01 = u.muted;
    p.layer_02 = u.layer2;
    p.layer_03 = u.layer3;
    p.text_base = u.content;
    p.text_strong = u.content;
    p.text_muted = u.muted_text;
    p.text_weak = u.muted_text;
    p.text_faint = u.faint;
    p.text_accent = u.accent_text;
    p.border_muted = u.line;
    p.border_weak = u.line;
    p.border_base = u.line_strong;
    p.border_strong = u.line_strong;
    // Switches, segment indicators and focus outlines take the window's one
    // accent instead of the kit's own blue.
    p.bg_accent = u.accent;
    p.text_contrast = u.inverted;
    p.border_focus = u.accent.alpha(0.7);
    p.hover = u.hover;
    p.pressed = u.active;
    // The off switch track: visible on white, quiet on the dark sheet.
    p.layer_04 = u.layer3;
    k
}

/// Body copy. OpenHuman reads `text-sm` (14) with relaxed leading; 13.5 keeps
/// the same rhythm while fitting more characters in a native window.
pub const BODY: TextStyle = TextStyle::new(13.5, 400.0).lh(20.0);
/// Secondary copy: session previews, fact text, plan tasks.
pub const SMALL: TextStyle = TextStyle::new(12.0, 400.0).lh(17.0);
/// Eyebrow labels and section headers.
pub const LABEL: TextStyle = TextStyle::new(11.0, 500.0).lh(15.0);
/// Code, paths and diffs.
pub const MONO: TextStyle = TextStyle::new(12.0, 400.0).mono().lh(17.0);
/// Panel and message headers.
pub const TITLE: TextStyle = TextStyle::new(13.0, 600.0).lh(18.0);

static ICON_SEND: Icon = Icon {
    name: "send",
    view: 16.0,
    body: r#"<path d="M8 13V3.5M4 7.2L8 3.2L12 7.2" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static ICON_STOP: Icon = Icon {
    name: "stop",
    view: 16.0,
    body: r#"<rect x="4.5" y="4.5" width="7" height="7" rx="1.4" fill="currentColor"/>"#,
};

static ICON_NEW: Icon = Icon {
    name: "new-chat",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>"#,
};

pub fn icon_send() -> &'static Icon {
    &ICON_SEND
}

pub fn icon_stop() -> &'static Icon {
    &ICON_STOP
}

pub fn icon_new() -> &'static Icon {
    &ICON_NEW
}

static ICON_CLOSE: Icon = Icon {
    name: "close",
    view: 16.0,
    body: r#"<path d="M4.5 4.5L11.5 11.5M11.5 4.5L4.5 11.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

pub fn icon_close() -> &'static Icon {
    &ICON_CLOSE
}

/// Declares a 16 pt stroke icon and its accessor. Every glyph in the window
/// is drawn on the same 16 grid with a 1.5 stroke, so they sit together.
macro_rules! glyph {
    ($fn:ident, $name:literal, $body:literal) => {
        pub fn $fn() -> &'static Icon {
            static I: Icon = Icon {
                name: $name,
                view: 16.0,
                body: concat!(
                    r#"<g fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">"#,
                    $body,
                    "</g>"
                ),
            };
            &I
        }
    };
}

glyph!(
    icon_chat,
    "nav-chat",
    r#"<path d="M4.5 3h7A1.5 1.5 0 0 1 13 4.5v5a1.5 1.5 0 0 1-1.5 1.5H7.5L5 13v-2h-.5A1.5 1.5 0 0 1 3 9.5v-5A1.5 1.5 0 0 1 4.5 3z"/>"#
);
glyph!(
    icon_history,
    "nav-history",
    r#"<circle cx="8" cy="8" r="5.25"/><path d="M8 5.25V8l1.9 1.3"/>"#
);
glyph!(
    icon_agents,
    "nav-agents",
    r#"<rect x="3" y="5.5" width="10" height="7" rx="2"/><path d="M8 3v2.5M6.25 9h0M9.75 9h0"/>"#
);
glyph!(
    icon_skills,
    "nav-skills",
    r#"<path d="M8 2.75l1.35 3.4 3.4 1.35-3.4 1.35L8 12.25 6.65 8.85 3.25 7.5l3.4-1.35z"/>"#
);
glyph!(
    icon_memory,
    "nav-memory",
    r#"<path d="M8 3l5 2.5L8 8 3 5.5zM3 8.25l5 2.5 5-2.5M3 10.75l5 2.5 5-2.5"/>"#
);
glyph!(
    icon_providers,
    "nav-providers",
    r#"<path d="M6 2.75v2.75M10 2.75v2.75M4.5 5.5h7v2a3.5 3.5 0 0 1-7 0zM8 11v2.25"/>"#
);
glyph!(
    icon_settings,
    "nav-settings",
    r#"<path d="M3 5h5.5M11.5 5H13M3 11h1.5M7.5 11H13"/><circle cx="10" cy="5" r="1.5"/><circle cx="6" cy="11" r="1.5"/>"#
);
glyph!(
    icon_media,
    "nav-media",
    r#"<rect x="2.75" y="3.5" width="10.5" height="9" rx="1.75"/><circle cx="6" cy="6.75" r="1"/><path d="M3.5 11.5l3-3 2 2 1.5-1.5 2.5 2.5"/>"#
);
glyph!(
    icon_search,
    "search",
    r#"<circle cx="7.25" cy="7.25" r="3.75"/><path d="M10 10l3 3"/>"#
);
glyph!(
    icon_archive,
    "archive",
    r#"<path d="M2.75 3.75h10.5v2.5H2.75zM3.75 6.25v6h8.5v-6M6.5 8.75h3"/>"#
);
glyph!(
    icon_export,
    "export",
    r#"<path d="M8 2.75v7M5.25 5.5L8 2.75l2.75 2.75M3.5 9.5v3h9v-3"/>"#
);
glyph!(
    icon_trash,
    "trash",
    r#"<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>"#
);
glyph!(
    icon_edit,
    "edit",
    r#"<path d="M10.25 3.25l2.5 2.5-7 7h-2.5v-2.5z"/>"#
);
glyph!(icon_check, "check", r#"<path d="M3.5 8.5l3 3 6-6.5"/>"#);
glyph!(
    icon_refresh,
    "refresh",
    r#"<path d="M12.5 8a4.5 4.5 0 1 1-1.3-3.2M12.5 2.75v2.5H10"/>"#
);
glyph!(
    icon_folder,
    "folder",
    r#"<path d="M2.5 4.5a1 1 0 0 1 1-1h3l1.5 1.5h4.5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/>"#
);
glyph!(
    icon_shield,
    "shield",
    r#"<path d="M8 2.5l4.5 1.8v3.6c0 2.7-1.9 4.6-4.5 5.6-2.6-1-4.5-2.9-4.5-5.6V4.3z"/>"#
);
glyph!(
    icon_terminal,
    "terminal",
    r#"<path d="M3 4.5l3.5 3.5L3 11.5M8 12h5"/>"#
);
glyph!(
    icon_file,
    "file",
    r#"<path d="M4.5 2.75h4.25l3 3v7a.5.5 0 0 1-.5.5h-6.75a.5.5 0 0 1-.5-.5v-9.5a.5.5 0 0 1 .5-.5zM8.5 2.75v3.25h3.25"/>"#
);
glyph!(
    icon_tool,
    "tool",
    r#"<path d="M9.5 3.25a3 3 0 0 0-3.2 4L3 10.55l2.45 2.45 3.3-3.3a3 3 0 0 0 4-3.2L11 8.25 8.75 6 10.5 4.25z"/>"#
);
glyph!(
    icon_chevron_right,
    "chevron-right",
    r#"<path d="M6 3.75L10.25 8 6 12.25"/>"#
);
glyph!(
    icon_chevron_down,
    "chevron-down",
    r#"<path d="M3.75 6L8 10.25 12.25 6"/>"#
);
glyph!(
    icon_list,
    "task-list",
    r#"<path d="M6.5 4.5h6.5M6.5 8h6.5M6.5 11.5h6.5M3 4.5h.01M3 8h.01M3 11.5h.01"/>"#
);
glyph!(
    icon_question,
    "question",
    r#"<circle cx="8" cy="8" r="5.5"/><path d="M6.5 6.5a1.6 1.6 0 1 1 2.3 1.45c-.5.3-.8.65-.8 1.3M8 11.25h.01"/>"#
);
glyph!(icon_cross, "cross", r#"<path d="M5 5l6 6M11 5l-6 6"/>"#);

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance. `Color` is already linear sRGB.
    fn lum(c: Color) -> f32 {
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
    }

    /// WCAG contrast ratio of two opaque colours.
    pub(crate) fn ratio(a: Color, b: Color) -> f32 {
        let (x, y) = (lum(a), lum(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn fills(u: &Ui) -> [(&'static str, Color); 6] {
        [
            ("surface", u.surface),
            ("chrome", u.chrome),
            ("muted", u.muted),
            ("layer2", u.layer2),
            ("layer3", u.layer3),
            ("selected", u.selected),
        ]
    }

    #[test]
    fn the_wcag_helper_matches_known_values() {
        assert!((ratio(Color::BLACK, Color::WHITE) - 21.0).abs() < 0.01);
        assert!((ratio(hex("#767676").unwrap(), Color::WHITE) - 4.54).abs() < 0.02);
    }

    #[test]
    fn body_text_tokens_reach_4_5_on_every_fill_in_both_themes() {
        for u in [light(), dark()] {
            let theme = if u.dark { "dark" } else { "light" };
            // The label ladder holds body-size text on any layer.
            for (name, fg) in [
                ("content", u.content),
                ("secondary", u.secondary),
                ("muted_text", u.muted_text),
                ("faint", u.faint),
            ] {
                for (bg_name, bg) in fills(&u) {
                    let r = ratio(fg, bg);
                    assert!(r >= 4.5, "{theme}: {name} on {bg_name} is {r:.2}:1");
                }
            }
            // Coloured text sits on the sheet, the chrome, inputs and raised rows.
            for (name, fg) in [
                ("accent_text", u.accent_text),
                ("danger", u.danger),
                ("warn_text", u.warn_text),
                ("done", u.done),
            ] {
                for (bg_name, bg) in fills(&u).into_iter().take(4) {
                    let r = ratio(fg, bg);
                    assert!(r >= 4.5, "{theme}: {name} on {bg_name} is {r:.2}:1");
                }
            }
            let r = ratio(u.inverted, u.primary);
            assert!(r >= 4.5, "{theme}: primary button ink is {r:.2}:1");
        }
    }

    #[test]
    fn the_label_ladder_keeps_its_order() {
        for u in [light(), dark()] {
            let on = u.surface;
            assert!(ratio(u.content, on) > ratio(u.secondary, on));
            assert!(ratio(u.secondary, on) > ratio(u.muted_text, on));
            assert!(ratio(u.muted_text, on) > ratio(u.faint, on));
        }
    }
}
