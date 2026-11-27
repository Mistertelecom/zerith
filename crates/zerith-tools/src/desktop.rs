//! `desktop_inspect`: lightweight native Windows desktop inspection tool with
//! zero external binaries required.
//!
//! - `action: "windows"`: enumerates visible top-level windows and highlights
//!   any blocking Win32 `#32770` modal dialogs with their inner `Static` text
//!   (pure text output — works even on text-only models like DeepSeek).
//! - `action: "screenshot"`: captures either a specific window (matching
//!   `title` or `pid`) or the primary screen to `<data>/desktop/shots/<ms>.png`
//!   and emits `IMAGE_MARKER` so the runtime feeds it to native vision or the
//!   configured `vision_model` fallback.

use serde_json::{json, Value};

use crate::fntool::{make, opt_str, str_arg};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make(
        "desktop_inspect",
        "Inspeciona janelas reais da área de trabalho do PC (Windows) sem depender de programas externos: \
`windows` lista janelas visíveis e destaca caixas de diálogo modais (#32770 como MessageBox/tkinter.messagebox/QMessageBox) com título e texto interno em texto puro (funciona até em modelos sem visão); \
`screenshot` captura um print PNG da tela ou de uma janela específica (por `title` ou `pid`) e anexa automaticamente para visão (ou para o Modelo de Visão configurado em Delegação).",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["windows", "screenshot"],
                    "description": "windows: lista janelas e textos de diálogos modais; screenshot: tira print da tela ou da janela filtrada"
                },
                "title": {
                    "type": "string",
                    "description": "filtro opcional (substring sem diferenciar maiúsculas) pelo título da janela"
                },
                "pid": {
                    "type": "integer",
                    "description": "filtro opcional pelo PID do processo dono da janela"
                }
            },
            "required": ["action"]
        }),
        "windows: lista de janelas visíveis e diálogos modais com texto; screenshot: caminho do PNG, linha IMAGE_MARKER e Markdown",
        Permission::Read,
        30,
        true,
        run,
    )]
}

fn run(args: &Value, _ctx: &CallCtx<'_>, _exec: &Exec<'_>) -> Result<String> {
    let action = str_arg(args, "action")?;
    let title = opt_str(args, "title");
    let pid = args["pid"].as_u64().and_then(|v| u32::try_from(v).ok());
    match action {
        "windows" => list_windows(title, pid),
        "screenshot" => capture_screenshot(title, pid),
        other => Err(ToolError::BadArguments(format!(
            "ação desconhecida em desktop_inspect: {other}; use windows ou screenshot"
        ))),
    }
}

fn list_windows(title: Option<&str>, pid: Option<u32>) -> Result<String> {
    if !cfg!(windows) {
        return Ok(
            "desktop_inspect (windows) está disponível apenas no Windows no momento.".into(),
        );
    }
    let wins = zerith_security::jail::list_visible_windows(title, pid);
    if wins.is_empty() {
        return Ok("Nenhuma janela visível encontrada para o filtro informado.".into());
    }
    let mut lines = Vec::with_capacity(wins.len() + 1);
    let modals = wins.iter().filter(|w| w.is_modal).count();
    lines.push(format!(
        "{} janela(s) visível(is){}:",
        wins.len(),
        if modals > 0 {
            format!(" ({modals} diálogo(s) modal(is) bloqueante(s) #32770)")
        } else {
            String::new()
        }
    ));
    for w in wins.into_iter().take(60) {
        let shown_title = if w.title.is_empty() {
            "(sem título)"
        } else {
            w.title.as_str()
        };
        if w.is_modal {
            let body = if w.modal_text.is_empty() {
                "(sem texto)"
            } else {
                w.modal_text.as_str()
            };
            lines.push(format!(
                "- [MODAL #32770] PID {} — \"{}\": {}",
                w.pid, shown_title, body
            ));
        } else {
            lines.push(format!("- PID {} [{}] — {}", w.pid, w.class, shown_title));
        }
    }
    Ok(lines.join("\n"))
}

#[cfg(windows)]
fn capture_screenshot(title: Option<&str>, pid: Option<u32>) -> Result<String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let root = zerith_core::Paths::resolve().root;
    let dir = root.join("desktop").join("shots");
    std::fs::create_dir_all(&dir)?;
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("{ms}.png"));
    let path_str = path.to_string_lossy().into_owned();

    let script = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$outPath = $env:ZERITH_SHOT_OUT
$filterTitle = $env:ZERITH_SHOT_TITLE
$filterPid = $env:ZERITH_SHOT_PID

$rect = $null
if ($filterTitle -or $filterPid) {
    Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class ZerithWinCap {
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int cap);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    public static int[] FindRect(string titleSub, int wantPid) {
        int[] found = null;
        string needle = titleSub == null ? "" : titleSub.ToLowerInvariant();
        EnumWindows((h, l) => {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (wantPid > 0 && p != (uint)wantPid) return true;
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
            string t = sb.ToString();
            if (needle.Length > 0 && !t.ToLowerInvariant().Contains(needle)) return true;
            if (needle.Length == 0 && wantPid == 0) return true;
            RECT r;
            if (GetWindowRect(h, out r) && (r.Right - r.Left) > 20 && (r.Bottom - r.Top) > 20) {
                found = new int[] { r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top };
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
"@
    $pidInt = 0
    if ($filterPid) { [void][int]::TryParse($filterPid, [ref]$pidInt) }
    $rect = [ZerithWinCap]::FindRect($filterTitle, $pidInt)
}

if ($null -ne $rect) {
    $x = $rect[0]; $y = $rect[1]; $w = $rect[2]; $h = $rect[3]
} else {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $x = $bounds.X; $y = $bounds.Y; $w = $bounds.Width; $h = $bounds.Height
}

$bmp = New-Object System.Drawing.Bitmap($w, $h)
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$gfx.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
$gfx.Dispose()
$bmp.Dispose()
"#;

    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("ZERITH_SHOT_OUT", &path_str)
        .env("ZERITH_SHOT_TITLE", title.unwrap_or(""))
        .env(
            "ZERITH_SHOT_PID",
            pid.map(|p| p.to_string()).unwrap_or_default(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let out = cmd.output()?;
    if !out.status.success() || !path.is_file() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ToolError::Failed(format!(
            "falha ao capturar a tela: {}",
            stderr.trim()
        )));
    }
    let bytes = std::fs::metadata(&path)?.len() as usize;
    let kb = bytes.div_ceil(1024);
    let shown = path.display().to_string().replace('\\', "/");
    let md = if shown.contains(char::is_whitespace) || shown.contains(')') {
        format!("![screenshot](<{shown}>)")
    } else {
        format!("![screenshot]({shown})")
    };
    Ok(format!(
        "{} png, {kb} KB: ![imagem](<{shown}>)\nCaptura de tela salva em {} ({kb} KB).\n{md}\nCole a linha Markdown acima na resposta se quiser mostrar o print ao usuário.",
        crate::IMAGE_MARKER,
        path.display()
    ))
}

#[cfg(not(windows))]
fn capture_screenshot(_title: Option<&str>, _pid: Option<u32>) -> Result<String> {
    Err(ToolError::Failed(
        "desktop_inspect screenshot está disponível apenas no Windows no momento".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_inspect_rejects_unknown_action() {
        let tool = all().into_iter().next().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let policy = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::AcceptEdits,
            dir.path(),
        );
        let ctx = CallCtx {
            workspace: dir.path(),
            policy: &policy,
            memory: None,
            scope: None,
        };
        let host = crate::Host::default();
        let cancel = zerith_providers::Cancel::default();
        let exec = Exec {
            host: &host,
            cancel: &cancel,
            timeout: std::time::Duration::from_secs(10),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
        };
        assert!(tool.run(&json!({"action": "nope"}), &ctx, &exec).is_err());
    }
}
