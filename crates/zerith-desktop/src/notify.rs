//! Calls the person back when the window is not in front: the answer is
//! ready, the turn stopped with an error, or it waits for an approval or an
//! answer. Taskbar flash (Tauri) plus a Windows toast (PowerShell + WinRT, no
//! crate). Nothing happens while the window has focus.

use tauri::{AppHandle, Manager, UserAttentionType};

pub fn ping(app: &AppHandle, title: &str, body: &str) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    if in_front(&w) {
        return;
    }
    let _ = w.request_user_attention(Some(UserAttentionType::Informational));
    toast(title, body);
}

/// Whether our window is the one in front. Tauri's `is_focused` says false
/// while the WebView child holds the keyboard focus (seen live), so ask
/// Windows which top-level window is in the foreground.
#[cfg(windows)]
fn in_front(w: &tauri::WebviewWindow) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> *mut std::ffi::c_void;
    }
    // SAFETY: no arguments, returns a window handle or null.
    let front = unsafe { GetForegroundWindow() };
    w.hwnd().is_ok_and(|h| h.0 == front)
}

#[cfg(not(windows))]
fn in_front(w: &tauri::WebviewWindow) -> bool {
    w.is_focused().unwrap_or(false)
}

#[cfg(windows)]
fn toast(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Text travels in env vars and is XML-escaped by PowerShell: nothing the
    // model wrote can reach the script as code.
    const SCRIPT: &str = r#"
[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime] > $null
[Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime] > $null
$t = [Security.SecurityElement]::Escape($env:ZERITH_TOAST_TITLE)
$b = [Security.SecurityElement]::Escape($env:ZERITH_TOAST_BODY)
$x = New-Object Windows.Data.Xml.Dom.XmlDocument
$x.LoadXml("<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>")
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:ZERITH_TOAST_APP).Show([Windows.UI.Notifications.ToastNotification]::new($x))
"#;
    // The installer registers this id on the Start menu shortcut.
    // ponytail: a dev build without that shortcut shows the toast as
    // "Windows PowerShell"; register an AUMID at startup if that matters.
    let installed = std::env::var_os("APPDATA")
        .map(|a| {
            std::path::Path::new(&a)
                .join("Microsoft/Windows/Start Menu/Programs/Zerith.lnk")
                .exists()
        })
        .unwrap_or(false);
    let app_id = if installed {
        "com.zerith.desktop"
    } else {
        r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe"
    };
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("ZERITH_TOAST_TITLE", title)
        .env(
            "ZERITH_TOAST_BODY",
            body.chars().take(200).collect::<String>(),
        )
        .env("ZERITH_TOAST_APP", app_id)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(not(windows))]
fn toast(_: &str, _: &str) {}
