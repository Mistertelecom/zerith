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
