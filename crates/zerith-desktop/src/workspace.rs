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
// ponytail: videos travel as a data URL like images; past this cap the chat
// offers the system player. Asset protocol if bigger videos must play inline.
const VIDEO_CAP: u64 = 60 * 1024 * 1024;

fn image_mime(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "avif" => "image/avif",
            "mp4" | "m4v" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            _ => return None,
        },
    )
}

/// Checks file magic bytes match a real raster image or video container.
fn matches_media_magic(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/avif" => bytes.len() >= 12 && &bytes[4..8] == b"ftyp",
        "video/mp4" | "video/quicktime" => bytes.len() >= 8 && &bytes[4..8] == b"ftyp",
        "video/webm" => bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => false,
    }
}

/// Canonical directories from which the transcript may read/open local media:
/// attachments, generated media, browser screenshots, and session workspaces.
fn allowed_media_roots(root: &Path, db: &Path) -> Vec<PathBuf> {
    let mut raw = vec![
        root.join("attachments"),
        root.join("media"),
        wire::media_dir(root),
        root.join("browser").join("shots"),
        crate::turn::default_workspace(),
    ];
    if let Ok(st) = zerith_sessions::Store::open(db) {
        if let Ok(dirs) = st.workspaces() {
            for ws in dirs.into_values() {
                raw.push(PathBuf::from(ws));
            }
        }
    }
    let mut out = Vec::new();
    for r in raw {
        if let Ok(c) = r.canonicalize() {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
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

/// A local image checked against the rules above and confined to `roots`
/// (after canonicalizing symlinks).
fn checked_image_in(raw: &str, roots: &[PathBuf]) -> Cmd<(PathBuf, &'static str)> {
    let trimmed = raw.trim();
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") || trimmed.starts_with("/\\") {
        return Err("caminhos UNC não são permitidos".into());
    }
    let path = image_path(raw);
    let path_str = path.to_string_lossy();
    if path_str.starts_with("\\\\") || path_str.starts_with("//") {
        return Err("caminhos UNC não são permitidos".into());
    }
    let mime = image_mime(&path)
        .ok_or("só abro imagens (png, jpg, gif, webp, bmp, avif) e vídeos (mp4, webm, mov)")?;
    let canon = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !roots.iter().any(|r| canon.starts_with(r)) {
        return Err("só abro mídia do workspace, anexos ou pasta de mídia do Zerith".into());
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("{}: {e}", canon.display()))?;
    if !meta.is_file() {
        return Err(format!("{} não é um arquivo", path.display()));
    }
    if mime.starts_with("video/") && meta.len() > VIDEO_CAP {
        return Err("vídeo maior que 60 MB: abra no player".into());
    }
    if !mime.starts_with("video/") && meta.len() > IMAGE_CAP {
        return Err("imagem maior que 20 MB".into());
    }
    Ok((canon, mime))
}

fn image_data_url_in(raw: &str, roots: &[PathBuf]) -> Cmd<String> {
    use base64::Engine;
    let (path, mime) = checked_image_in(raw, roots)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !matches_media_magic(mime, &bytes) {
        return Err("o conteúdo do arquivo não corresponde a uma imagem ou vídeo válido".into());
    }
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// A local image the assistant wrote as `![](path)`, as a data URL the
/// webview can show without the asset protocol.
#[tauri::command]
pub async fn read_image(state: State<'_, AppState>, path: String) -> Cmd<String> {
    let root = state.root().to_path_buf();
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let roots = allowed_media_roots(&root, &db);
        image_data_url_in(&path, &roots)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg_attr(not(windows), allow(dead_code))]
fn strip_verbatim_str(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        Some(format!(r"\\{rest}"))
    } else {
        s.strip_prefix(r"\\?\").map(str::to_string)
    }
}

fn reveal_path(canon: &Path) -> PathBuf {
    let native: PathBuf = canon.components().collect();
    #[cfg(windows)]
    if let Some(stripped) = native.to_str().and_then(strip_verbatim_str) {
        return PathBuf::from(stripped);
    }
    native
}

/// Opens a local image in the system's default viewer.
#[tauri::command]
pub async fn open_image(state: State<'_, AppState>, path: String) -> Cmd<()> {
    let root = state.root().to_path_buf();
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        let roots = allowed_media_roots(&root, &db);
        let (canon, _) = checked_image_in(&path, &roots)?;
        // Native separators without `\\?\` verbatim prefix: `explorer` rejects verbatim paths.
        let native = reveal_path(&canon);
        wire::reveal(&native)
    })
    .await
    .map_err(|e| e.to_string())?
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
pub async fn rewind(
    state: State<'_, AppState>,
    turns: State<'_, crate::chat::Turns>,
    session_id: String,
) -> Cmd<RewindView> {
    if turns.with(&session_id, |_| ()).is_some() {
        return Err("Aguarde ou pare o turno atual antes de continuar".into());
    }
    turns.cancel_precompact(&session_id);
    let db = state.db();
    tauri::async_runtime::spawn_blocking(move || {
        wire::rewind_latest_turn(&db, &session_id).map(|(turn, text)| RewindView { turn, text })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Summarizes older messages so the next turn fits the context window.
#[tauri::command]
pub async fn compact(
    state: State<'_, AppState>,
    turns: State<'_, crate::chat::Turns>,
    session_id: String,
    model: String,
) -> Cmd<usize> {
    if turns.with(&session_id, |_| ()).is_some() {
        return Err("Aguarde ou pare o turno atual antes de continuar".into());
    }
    if model.trim().is_empty() {
        return Err("Escolha um modelo antes de compactar".into());
    }
    turns.cancel_precompact(&session_id);
    let root = state.root().to_path_buf();
    let db = state.db();
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

    /// Only real images/videos inside an allowed root are read as data URLs;
    /// files outside the allowed roots, SVGs, or spoofed headers are refused.
    #[test]
    fn read_image_inlines_images_and_refuses_other_files() {
        let d = scratch("read-image");
        let roots = vec![d.canonicalize().unwrap()];
        let png = d.join("a.PNG");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let url = image_data_url_in(&png.to_string_lossy(), &roots).unwrap();
        assert_eq!(url, "data:image/png;base64,iVBORw==");

        let file_url = format!(
            "file:///{}",
            png.to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        assert_eq!(
            image_data_url_in(&file_url, &roots).unwrap(),
            url,
            "file:// urls name the same file"
        );

        let mp4 = d.join("v.mp4");
        std::fs::write(&mp4, [0, 0, 0, 0x18, b'f', b't', b'y', b'p']).unwrap();
        assert_eq!(
            image_data_url_in(&mp4.to_string_lossy(), &roots).unwrap(),
            "data:video/mp4;base64,AAAAGGZ0eXA=",
            "a generated video plays in the chat"
        );

        // Spoofed extension (text disguised as .png) must fail magic check.
        let fake_png = d.join("fake.png");
        std::fs::write(&fake_png, b"not a png").unwrap();
        assert!(
            image_data_url_in(&fake_png.to_string_lossy(), &roots).is_err(),
            "spoofed png must be refused"
        );

        // File outside allowed roots must be refused even with valid PNG bytes.
        let outside = scratch("outside-image");
        let out_png = outside.join("secret.png");
        std::fs::write(&out_png, [0x89, b'P', b'N', b'G']).unwrap();
        assert!(
            image_data_url_in(&out_png.to_string_lossy(), &roots).is_err(),
            "outside allowed roots must be refused"
        );

        let secret = d.join("auth.json");
        std::fs::write(&secret, "{}").unwrap();
        assert!(
            image_data_url_in(&secret.to_string_lossy(), &roots).is_err(),
            "not an image"
        );
        assert!(
            image_data_url_in(&d.join("nada.png").to_string_lossy(), &roots).is_err(),
            "missing"
        );
        std::fs::create_dir_all(d.join("pasta.png")).unwrap();
        assert!(
            image_data_url_in(&d.join("pasta.png").to_string_lossy(), &roots).is_err(),
            "a directory"
        );

        for unc in [
            r"\\attacker\share\a.png",
            "//attacker/share/a.png",
            r"/\attacker/share/a.png",
            r"file://\\attacker\share\a.png",
        ] {
            assert!(
                checked_image_in(unc, &roots).is_err(),
                "UNC path `{unc}` must be refused before canonicalize"
            );
        }
    }
}

/// Opens a link in the system browser. The webview drops `target=_blank` and
/// `window.open` (wry reports the request as handled unless a handler exists),
/// so a cited source used to do nothing at all. Only the schemes a person can
/// click are accepted: this command must never be a way to run anything else.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    check_url(&url)?;
    zerith_providers::oauth::open_browser(url.trim())
        .map_err(|e| format!("não consegui abrir o link: {e}"))
}

/// The accepted schemes, apart from the spawning so it can be tested.
fn check_url(url: &str) -> Result<(), String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let ok = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:");
    let has_ctrl = u.chars().any(char::is_control);
    if !ok || has_ctrl || (u.contains(char::is_whitespace) && !lower.starts_with("mailto:")) {
        return Err(format!("link recusado: {u}"));
    }
    Ok(())
}

#[cfg(test)]
mod open_url_tests {
    use super::*;

    /// Only http(s) and mailto reach the browser; a `file:` or `javascript:`
    /// link from a page the model read must not.
    #[test]
    fn only_clickable_schemes_pass() {
        for ok in ["https://a.b/c", "http://a.b", "mailto:x@y.z"] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "file:///C:/Windows/win.ini",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "https://a.b/a b",
            "",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn strip_verbatim_handles_drive_and_unc_paths() {
        assert_eq!(
            strip_verbatim_str(r"\\?\C:\Users\Joao\img.png").as_deref(),
            Some(r"C:\Users\Joao\img.png")
        );
        assert_eq!(
            strip_verbatim_str(r"\\?\UNC\fileserver\share\img.png").as_deref(),
            Some(r"\\fileserver\share\img.png")
        );
        assert_eq!(strip_verbatim_str(r"C:\Users\Joao\img.png"), None);
    }
}
