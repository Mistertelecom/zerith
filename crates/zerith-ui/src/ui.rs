//! The visual vocabulary, taken from OpenHuman's own token sheet
//! (`app/src/styles/tokens.css`, `.dark` block) instead of invented here: same
//! surface ladder, same text ladder, same accent. Values are sRGB hex because
//! Rune authors colour in OKLCH but accepts `#rrggbb` through `color::hex`.
//!
//! Nothing in this file draws; it only names colours, type sizes and the two
//! icons the shell needs, so `lib.rs` stays about behaviour.

use rune::color::hex;
use rune::{Color, Icon, TextStyle};

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

static ICON_THEME: Icon = Icon {
    name: "theme",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 3a5 5 0 0 1 0 10z" fill="currentColor"/>"#,
};

pub fn icon_close() -> &'static Icon {
    &ICON_CLOSE
}

pub fn icon_theme() -> &'static Icon {
    &ICON_THEME
}
