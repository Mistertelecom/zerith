//! Images the composer attaches (paste, drop, paperclip): written once under
//! `<root>/attachments` and referenced from the prompt as `![nome](path)`, so
//! the providers send them as image input and the transcript can show them
//! again after a reload, with no change to the store.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use base64::Engine;
use tauri::State;

use crate::state::AppState;

/// Same ceiling `read_image` and the providers apply.
const CAP: usize = 20 * 1024 * 1024;
/// Ceiling for general file/video/log/config attachments.
const FILE_CAP: usize = 50 * 1024 * 1024;
/// Maximum length of a sanitized filename (preserving its extension).
const MAX_FILENAME_LEN: usize = 120;

fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("anexo").trim();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = clean.trim_matches(['.', '_']);
    if trimmed.is_empty() {
        return "anexo.bin".into();
    }
    if trimmed.len() <= MAX_FILENAME_LEN {
        return trimmed.to_string();
    }
    if let Some((stem, ext)) = trimmed.rsplit_once('.') {
        if !stem.is_empty() && !ext.is_empty() && ext.len() + 1 < MAX_FILENAME_LEN {
            let keep = MAX_FILENAME_LEN - 1 - ext.len();
            let stem_prefix: String = stem.chars().take(keep).collect();
            let stem_clean = stem_prefix.trim_end_matches(['.', '_']);
            let stem_out = if stem_clean.is_empty() {
                stem_prefix.as_str()
            } else {
                stem_clean
            };
            return format!("{stem_out}.{ext}");
        }
    }
    trimmed.chars().take(MAX_FILENAME_LEN).collect()
}

/// Decodes `data` (bare base64 or a `data:` URL) for any file (configs, logs,
/// videos, PDFs, captures, images) up to 50 MB and writes it under
/// `<root>/attachments/<hash>-<safe_name>`.
fn save_named(root: &Path, name: &str, data: &str) -> Result<PathBuf, String> {
    let b64 = match data.strip_prefix("data:") {
        Some(rest) => rest.split_once(',').ok_or("data URL sem conteúdo")?.1,
        None => data,
    };
    if b64.len() > FILE_CAP / 3 * 4 + 4 {
        return Err("arquivo maior que 50 MB".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| "anexo corrompido (base64 inválido)".to_string())?;
    if bytes.len() > FILE_CAP {
        return Err("arquivo maior que 50 MB".into());
    }
    let safe_name = sanitize_filename(name);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = root.join("attachments");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{:016x}-{safe_name}", h.finish()));
    if !path.is_file() {
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Decodes `data` (bare base64 or a `data:` URL), checks it, writes it as
/// `<root>/attachments/<hash>.<ext>` and returns that path.
fn save(root: &Path, data: &str) -> Result<PathBuf, String> {
    let b64 = match data.strip_prefix("data:") {
        Some(rest) => rest.split_once(',').ok_or("data URL sem conteúdo")?.1,
        None => data,
    };
    // base64 is 4/3 of the bytes: refuse before decoding something huge.
    if b64.len() > CAP / 3 * 4 + 4 {
        return Err("imagem maior que 20 MB".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| "anexo corrompido (base64 inválido)".to_string())?;
    if bytes.len() > CAP {
        return Err("imagem maior que 20 MB".into());
    }
    // Named by what the bytes are, not by the file name: a clipboard image has
    // none worth trusting, and the providers re-check the bytes anyway.
    let ext = match zerith_providers::images::sniff_mime(&bytes) {
        Some(m) => m.trim_start_matches("image/").replace("jpeg", "jpg"),
        None => return Err("só anexo imagens png, jpg, webp ou gif".into()),
    };
    // ponytail: SipHash of the bytes names the file (same image = same file);
    // swap for SHA-256 if attachments ever need a stable cross-version id.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = root.join("attachments");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    if !path.is_file() {
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Saves one attached image; the answer is the absolute path the prompt cites.
#[tauri::command]
pub async fn save_attachment(state: State<'_, AppState>, data: String) -> Result<String, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        save(&root, &data).map(|p| p.to_string_lossy().replace('\\', "/"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DroppedAttachment {
    pub path: String,
    pub data_url: Option<String>,
}

fn attach_dropped_path_in(root: &Path, raw_path: &str) -> Result<DroppedAttachment, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err("caminho vazio".into());
    }
    if trimmed.starts_with("\\\\")
        || trimmed.starts_with("//")
        || trimmed.starts_with("/\\")
        || trimmed.to_ascii_lowercase().starts_with("file://\\\\")
    {
        return Err("caminhos UNC não são permitidos".into());
    }
    let src = Path::new(trimmed);
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{} não é um arquivo", src.display()));
    }
    if meta.len() > CAP as u64 {
        return Err("arquivo maior que 20 MB".into());
    }
    let ext_lower = src
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            "só anexo imagens (png, jpg, webp, gif) ou vídeos (mp4, m4v, webm, mov)".to_string()
        })?;
    let is_image = matches!(ext_lower.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif");
    let is_video = matches!(ext_lower.as_str(), "mp4" | "m4v" | "webm" | "mov");
    if !is_image && !is_video {
        return Err(
            "só anexo imagens (png, jpg, webp, gif) ou vídeos (mp4, m4v, webm, mov)".into(),
        );
    }
    let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if bytes.len() > CAP {
        return Err("arquivo maior que 20 MB".into());
    }
    let dir = root.join("attachments");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let hash = h.finish();

    if is_image {
        let mime = zerith_providers::images::sniff_mime(&bytes)
            .ok_or("só anexo imagens png, jpg, webp ou gif")?;
        let ext = mime.trim_start_matches("image/").replace("jpeg", "jpg");
        let dest = dir.join(format!("{hash:016x}.{ext}"));
        if !dest.is_file() {
            std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        let data_url = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        Ok(DroppedAttachment {
            path: dest.to_string_lossy().replace('\\', "/"),
            data_url: Some(data_url),
        })
    } else {
        if bytes.is_empty() {
            return Err("vídeo vazio".into());
        }
        let safe_name = sanitize_filename(trimmed);
        let dest = dir.join(format!("{hash:016x}-{safe_name}"));
        if !dest.is_file() {
            std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        Ok(DroppedAttachment {
            path: dest.to_string_lossy().replace('\\', "/"),
            data_url: None,
        })
    }
}

/// Copies a user-dropped local image or video into `<root>/attachments`,
/// validating size <= 20 MB and magic bytes for images.
#[tauri::command]
pub async fn attach_dropped_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<DroppedAttachment, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || attach_dropped_path_in(&root, &path))
        .await
        .map_err(|e| e.to_string())?
}

/// Saves any attached file (config, log, video, capture, PDF, archive); returns
/// its normalized absolute path.
#[tauri::command]
pub async fn save_file_attachment(
    state: State<'_, AppState>,
    name: String,
    data: String,
) -> Result<String, String> {
    let root = state.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        save_named(&root, &name, &data).map(|p| p.to_string_lossy().replace('\\', "/"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_images_are_saved_once_under_attachments() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch/attach");
        let _ = std::fs::remove_dir_all(&root);
        let png = b"\x89PNG\r\n\x1a\nresto";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let a = save(&root, &format!("data:image/png;base64,{b64}")).unwrap();
        assert_eq!(a.parent().unwrap(), root.join("attachments"));
        assert_eq!(a.extension().unwrap(), "png");
        assert_eq!(std::fs::read(&a).unwrap(), png);
        assert_eq!(save(&root, &b64).unwrap(), a, "same bytes, same file");

        let txt = base64::engine::general_purpose::STANDARD.encode(b"{\"segredo\":1}");
        assert!(save(&root, &txt).is_err(), "not an image");
        assert!(save(&root, "%%%").is_err(), "not base64");
        assert!(
            save(&root, &"A".repeat(CAP / 3 * 4 + 8)).is_err(),
            "too big"
        );
    }

    #[test]
    fn named_file_attachments_save_configs_videos_and_logs_safely() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch/attach-file");
        let _ = std::fs::remove_dir_all(&root);
        let wg = b"[Interface]\nPrivateKey = abc\n";
        let b64 = base64::engine::general_purpose::STANDARD.encode(wg);
        let saved = save_named(
            &root,
            "../../wg-cliente (1).conf",
            &format!("data:text/plain;base64,{b64}"),
        )
        .unwrap();
        assert_eq!(saved.parent().unwrap(), root.join("attachments"));
        assert!(saved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-wg-cliente__1_.conf"));
        assert_eq!(std::fs::read(&saved).unwrap(), wg);
    }

    #[test]
    fn sanitize_filename_caps_length_at_120_and_preserves_extension() {
        let long = format!("{}.conf", "a".repeat(300));
        let sanitized = sanitize_filename(&long);
        assert!(sanitized.len() <= MAX_FILENAME_LEN, "{sanitized}");
        assert!(sanitized.ends_with(".conf"), "{sanitized}");

        let no_ext = "x".repeat(300);
        let sanitized_no_ext = sanitize_filename(&no_ext);
        assert_eq!(sanitized_no_ext.len(), MAX_FILENAME_LEN);
    }

    #[test]
    fn attach_dropped_path_validates_and_copies_media_to_attachments() {
        let base =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch/attach-drop");
        let _ = std::fs::remove_dir_all(&base);
        let src_dir = base.join("src");
        let root = base.join("root");
        std::fs::create_dir_all(&src_dir).unwrap();

        let png = src_dir.join("shot.png");
        let png_bytes = b"\x89PNG\r\n\x1a\npayload";
        std::fs::write(&png, png_bytes).unwrap();
        let res = attach_dropped_path_in(&root, &png.to_string_lossy()).unwrap();
        assert!(res.path.contains("/attachments/"));
        assert!(res.path.ends_with(".png"));
        assert_eq!(std::fs::read(&res.path).unwrap(), png_bytes);
        assert!(res
            .data_url
            .as_deref()
            .is_some_and(|u| u.starts_with("data:image/png;base64,")));

        let mp4 = src_dir.join("clip.mp4");
        let mp4_bytes = [0, 0, 0, 0x18, b'f', b't', b'y', b'p'];
        std::fs::write(&mp4, mp4_bytes).unwrap();
        let vres = attach_dropped_path_in(&root, &mp4.to_string_lossy()).unwrap();
        assert!(vres.path.contains("/attachments/"));
        assert!(vres.path.ends_with("-clip.mp4"));
        assert_eq!(std::fs::read(&vres.path).unwrap(), mp4_bytes);
        assert_eq!(vres.data_url, None);

        // Spoofed PNG bytes must fail sniff_mime.
        let fake = src_dir.join("fake.png");
        std::fs::write(&fake, b"not a png").unwrap();
        assert!(attach_dropped_path_in(&root, &fake.to_string_lossy()).is_err());

        // Non-media extension must be rejected.
        let txt = src_dir.join("note.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(attach_dropped_path_in(&root, &txt.to_string_lossy()).is_err());

        // UNC paths and directories must be rejected.
        assert!(attach_dropped_path_in(&root, r"\\server\share\a.png").is_err());
        assert!(attach_dropped_path_in(&root, "//server/share/a.png").is_err());
        assert!(attach_dropped_path_in(&root, &src_dir.to_string_lossy()).is_err());
    }
}
