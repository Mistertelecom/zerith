//! The visual vocabulary, taken from OpenHuman's own token sheet
//! (`app/src/styles/tokens.css`, `.dark` block) instead of invented here: same
//! surface ladder, same text ladder, same accent. Values are sRGB hex because
//! Rune authors colour in OKLCH but accepts `#rrggbb` through `color::hex`.
//!
//! Nothing in this file draws; it only names colours, type sizes and the icons the shell needs, so the screens stay about behaviour.

use rune::color::hex;
use rune::{Color, Icon, TextStyle};

#[derive(Clone, Copy)]
pub struct Ui {
    /// Which ladder this is; the kit and the theme toggle read it.
    pub dark: bool,
    /// `--surface-canvas`: the window behind everything.
    pub canvas: Color,
    /// `--surface-chrome`: the frame the sidebar sits on.
    pub chrome: Color,
    /// `--surface`: the content sheet.
    pub surface: Color,
    /// `--surface-muted`: inputs and bubbles.
    pub muted: Color,
    /// `--surface-hover`.
    pub hover: Color,
    /// `--line`.
    pub line: Color,
    /// `--line-strong`.
    pub line_strong: Color,
    /// `--content`.
    pub content: Color,
    /// `--content-secondary`.
    pub secondary: Color,
    /// `--content-muted`.
    pub muted_text: Color,
    /// `--content-faint`.
    pub faint: Color,
    /// `--primary-500`.
    pub primary: Color,
    /// `--primary-600`, for the pressed state.
    pub primary_press: Color,
    /// `--content-inverted`: text and icons on an accent fill.
    pub inverted: Color,
    /// `--success-500`: a reachable provider. Status dots only, never fills.
    pub ok: Color,
    /// `--warning-500`: checking, or answered with a complaint.
    pub warn: Color,
    /// `--danger-500`: unreachable, failed, destructive.
    pub danger: Color,
}

fn c(s: &str) -> Color {
    hex(s).expect("a token in this file is not #rrggbb")
}

pub fn dark() -> Ui {
    Ui {
        dark: true,
        canvas: c("#000000"),
        chrome: c("#0a0a0a"),
        surface: c("#171717"),
        muted: c("#262626"),
        hover: c("#262626"),
        line: c("#262626"),
        line_strong: c("#404040"),
        content: c("#f5f5f5"),
        secondary: c("#d4d4d4"),
        muted_text: c("#a3a3a3"),
        faint: c("#737373"),
        primary: c("#2f6ef4"),
        primary_press: c("#2563eb"),
        inverted: c("#ffffff"),
        ok: c("#22c55e"),
        warn: c("#f59e0b"),
        danger: c("#ef4444"),
    }
}

/// OpenHuman's `:root` (light) block, same roles as `dark()`.
pub fn light() -> Ui {
    Ui {
        dark: false,
        canvas: c("#f5f5f5"),
        chrome: c("#d6d6d6"),
        surface: c("#ffffff"),
        muted: c("#fafafa"),
        hover: c("#f0f0f0"),
        line: c("#e5e5e5"),
        line_strong: c("#d4d4d4"),
        content: c("#171717"),
        secondary: c("#525252"),
        muted_text: c("#737373"),
        faint: c("#a3a3a3"),
        primary: c("#2f6ef4"),
        primary_press: c("#2563eb"),
        inverted: c("#ffffff"),
        ok: c("#16a34a"),
        warn: c("#d97706"),
        danger: c("#dc2626"),
    }
}

pub fn pick(dark_mode: bool) -> Ui {
    if dark_mode {
        dark()
    } else {
        light()
    }
}

/// The kit `Markdown` reads its colours from. Starts from Rune's neutral dark
/// kit and overwrites only the roles Markdown paints (text, code surface,
/// rules, links) with this file's tokens, so an answer matches the window.
pub fn kit(u: &Ui) -> rune_kit::Kit {
    let mut k = rune_kit::Kit::neutral(u.dark);
    let p = &mut k.c;
    p.page = u.surface;
    p.bg_base = u.surface;
    p.bg_deep = u.chrome;
    p.layer_01 = u.muted;
    p.layer_02 = u.muted;
    p.layer_03 = u.hover;
    p.text_base = u.content;
    p.text_strong = u.content;
    p.text_muted = u.muted_text;
    p.text_weak = u.muted_text;
    p.text_faint = u.faint;
    p.text_accent = u.primary;
    p.border_muted = u.line;
    p.border_weak = u.line;
    p.border_base = u.line_strong;
    p.border_strong = u.line_strong;
    // Switches, segment indicators and focus outlines take the window's one
    // accent instead of the kit's own blue.
    p.bg_accent = u.primary;
    p.text_contrast = u.inverted;
    p.border_focus = u.primary.alpha(0.6);
    p.hover = u.hover;
    // The off switch track: visible on white, quiet on the dark sheet.
    p.layer_04 = u.line_strong;
    k
}

/// Body copy. OpenHuman reads `text-sm` (14) with relaxed leading; 13.5 keeps
/// the same rhythm while fitting more characters in a native window.
pub const BODY: TextStyle = TextStyle::new(13.5, 400.0).lh(20.0);
/// Secondary copy: session previews, fact text, plan tasks.
pub const SMALL: TextStyle = TextStyle::new(12.0, 400.0).lh(17.0);
/// Eyebrow labels and section headers.
pub const LABEL: TextStyle = TextStyle::new(11.0, 500.0).lh(15.0);
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
