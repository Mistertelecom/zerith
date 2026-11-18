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
}
