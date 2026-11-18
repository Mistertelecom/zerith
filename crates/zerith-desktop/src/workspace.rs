//! What the composer needs beyond the turn itself: the files `@` can mention,
//! the images the transcript shows, and the two housekeeping commands that act
//! on a conversation (`/compact` and `/rewind`).
//!
//! Both walk the workspace or the store on a blocking worker. None of it
//! decides policy: compaction and checkpoints live in the core crates.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use crate::wire;

type Cmd<T> = Result<T, String>;

/// Files offered by the `@` popup. Bounded so a huge checkout cannot stall the
/// popup; the model only needs the ones near the top anyway.
const FILE_CAP: usize = 5_000;
const MAX_DEPTH: usize = 8;
const SKIP: [&str; 4] = ["target", "node_modules", "dist", "build"];

fn walk(root: &Path, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            // Dot dirs and the app's own data folders (_wv WebView profile,
            // _data store) are noise, not something to mention.
            if name.starts_with('.') || name.starts_with('_') {
                continue;
            }
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH && !SKIP.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                    if out.len() >= cap {
                        out.sort();
                        return out;
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Files of `root` (the open conversation's folder), else of the folder a new
/// conversation would get.
#[tauri::command]
pub async fn workspace_files(state: State<'_, AppState>, root: Option<String>) -> Cmd<Vec<String>> {
    let db = state.db();
    let root = match root {
        Some(r) if !r.trim().is_empty() => PathBuf::from(r),
        _ => crate::turn::last_workspace(&db),
    };
    tauri::async_runtime::spawn_blocking(move || walk(&root, FILE_CAP))
        .await
        .map_err(|e| e.to_string())
}

/// Largest image the transcript will inline. The model names the path, so the
/// command must not turn into a way to pull arbitrary files into the window:
/// only image extensions, only regular files, only this big.
const IMAGE_CAP: u64 = 20 * 1024 * 1024;

fn image_mime(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "avif" => "image/avif",
            // Scripts inside an SVG do not run when it is shown through <img>.
            "svg" => "image/svg+xml",
            _ => return None,
        },
    )
}

/// `file:///C:/a.png`, `C:\a.png` or a path relative to the workspace, as the
/// absolute path it names.
fn image_path(raw: &str) -> PathBuf {
    let s = raw.trim();
    let s = match s.strip_prefix("file://") {
        // `file:///C:/x` keeps a slash before the drive on Windows.
        Some(rest) if cfg!(windows) => rest.trim_start_matches('/'),
        Some(rest) => rest,
        None => s,
    };
    let p = PathBuf::from(s);
    if p.is_absolute() {
        p
    } else {
        // ponytail: relative image paths resolve against the newest folder,
        // not the open conversation's; pass the session id if two differ.
        crate::turn::default_workspace().join(p)
    }
}

/// The native folder picker, without a dialog crate: PowerShell's
/// FolderBrowserDialog on an STA thread, no console window. The start folder
/// travels in an env var, never inside the script, so a path cannot inject
/// code. `Ok(None)`: cancelled.
// ponytail: PowerShell start-up costs ~1 s and the dialog is the classic tree;
// use IFileOpenDialog via the `windows` crate if it ever becomes a dependency.
pub fn pick_folder(start: Option<&str>) -> Cmd<Option<String>> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
Add-Type -AssemblyName System.Windows.Forms; \
$d = New-Object System.Windows.Forms.FolderBrowserDialog; \
$d.Description = 'Pasta da conversa'; $d.ShowNewFolderButton = $true; \
if ($env:ZERITH_PICK_START) { $d.SelectedPath = $env:ZERITH_PICK_START }; \
$f = New-Object System.Windows.Forms.Form -Property @{TopMost = $true}; \
if ($d.ShowDialog($f) -eq 'OK') { $d.SelectedPath }";
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", SCRIPT])
            .env("ZERITH_PICK_START", start.unwrap_or(""))
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Não consegui abrir o seletor de pasta: {e}"))?;
        if !out.status.success() {
            return Err("O seletor de pasta falhou".into());
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!path.is_empty()).then_some(path))
    }
    #[cfg(not(windows))]
    {
        let _ = start;
        Err("Escolher pasta pelo seletor só existe no Windows por enquanto".into())
    }
}

/// A local image checked against the rules above.
fn checked_image(raw: &str) -> Cmd<(PathBuf, &'static str)> {
    let path = image_path(raw);
    let mime = image_mime(&path).ok_or("só abro imagens (png, jpg, gif, webp, bmp, avif, svg)")?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} não é um arquivo", path.display()));
    }
    if meta.len() > IMAGE_CAP {
        return Err("imagem maior que 20 MB".into());
    }
    Ok((path, mime))
}

fn image_data_url(raw: &str) -> Cmd<String> {
    use base64::Engine;
    let (path, mime) = checked_image(raw)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// A local image the assistant wrote as `![](path)`, as a data URL the
/// webview can show without the asset protocol.
#[tauri::command]
pub async fn read_image(path: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || image_data_url(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// Opens a local image in the system's default viewer.
#[tauri::command]
pub async fn open_image(path: String) -> Cmd<()> {
    let (path, _) = checked_image(&path)?;
    // Native separators: `explorer C:/a/b.png` does not find the file.
    let native: PathBuf = path.components().collect();
    wire::reveal(&native)
}

/// Which turns this conversation can be rewound to.
#[tauri::command]
pub async fn checkpoints(state: State<'_, AppState>, session_id: String) -> Cmd<Vec<i64>> {
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || wire::checkpoint_turns(&db, &session_id))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindView {
    pub turn: i64,
    pub text: String,
}

/// Restores the newest checkpoint: the files it recorded and the conversation
/// up to then.
#[tauri::command]
pub async fn rewind(state: State<'_, AppState>, session_id: String) -> Cmd<RewindView> {
    let db = state.db();
    let turn = wire::checkpoint_turns(&db, &session_id)
        .last()
        .copied()
        .ok_or("nenhum ponto de restauração nesta conversa")?;
    tauri::async_runtime::spawn_blocking(move || {
        wire::rewind_latest(&db, &session_id).map(|text| RewindView { turn, text })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Summarizes older messages so the next turn fits the context window.
#[tauri::command]
pub async fn compact(state: State<'_, AppState>, session_id: String, model: String) -> Cmd<usize> {
    let root = state.root().to_path_buf();
    let db = state.db();
    if model.trim().is_empty() {
        return Err("Escolha um modelo antes de compactar".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        // The primary provider is the one the turn would use; it is first in
        // this list by construction.
        let cfg = wire::configured_providers(&root)
            .into_iter()
            .next()
            .ok_or("nenhum provedor configurado")?;
        // `conta:modelo` from the picker goes where a turn would send it.
        let settings =
            zerith_core::Settings::load(&zerith_core::Paths::with_root(&root), None).settings;
        let (gw, _) = zerith_runtime::resolve_gateway(&settings, &model, &cfg);
        let (cfg, model) = match gw.candidates.into_iter().next() {
            Some(c) => (c.config, c.model),
            None => (cfg, model),
        };
        wire::compact_now(&db, cfg, &session_id, &model)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(tag);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Only images, read whole, as a data URL; anything else is refused.
    #[test]
    fn read_image_inlines_images_and_refuses_other_files() {
        let d = scratch("read-image");
        let png = d.join("a.PNG");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let url = image_data_url(&png.to_string_lossy()).unwrap();
        assert_eq!(url, "data:image/png;base64,iVBORw==");

        let file_url = format!(
            "file:///{}",
            png.to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        assert_eq!(
            image_data_url(&file_url).unwrap(),
            url,
            "file:// urls name the same file"
        );

        let secret = d.join("auth.json");
        std::fs::write(&secret, "{}").unwrap();
        assert!(
            image_data_url(&secret.to_string_lossy()).is_err(),
            "not an image"
        );
        assert!(
            image_data_url(&d.join("nada.png").to_string_lossy()).is_err(),
            "missing"
        );
        std::fs::create_dir_all(d.join("pasta.png")).unwrap();
        assert!(
            image_data_url(&d.join("pasta.png").to_string_lossy()).is_err(),
            "a directory"
        );
    }
}
