//! Local images a user message points at with `![alt](path)`: the composer
//! writes attachments that way and the transcript shows them, so each protocol
//! module turns the same refs into its own image parts. The text stays as is.

use base64::Engine;
use std::path::Path;

/// Same ceiling the window applies when it shows a local image.
// ponytail: one cap for all; Anthropic refuses images over 5 MB (400), so
// downscale or per-protocol caps if big screenshots start failing there.
const CAP: u64 = 20 * 1024 * 1024;

pub(crate) struct Image {
    pub mime: &'static str,
    /// Standard base64, no prefix.
    pub data: String,
}

impl Image {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.data)
    }
}

/// The raw targets of every `![alt](target)`; `<target>` may hold spaces and `)`.
fn refs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("](") else { break };
        if rest[..j].contains('\n') {
            continue;
        }
        let after = &rest[j + 2..];
        let (raw, next) = match after.strip_prefix('<') {
            Some(inner) => match inner.find(">)") {
                Some(k) => (&inner[..k], &inner[k + 2..]),
                None => break,
            },
            None => match after.find(')') {
                Some(k) => (&after[..k], &after[k + 1..]),
                None => break,
            },
        };
        if !raw.contains('\n') {
            out.push(raw.trim());
        }
        rest = next;
    }
    out
}

/// A ref that names an existing local png/jpg/webp/gif up to 20 MB, read whole.
/// Remote URLs, other files and missing paths are left to the text.
fn load(raw: &str) -> Option<Image> {
    let p = match raw.strip_prefix("file://") {
        // `file:///C:/x` keeps a slash before the drive on Windows.
        Some(rest) if cfg!(windows) => rest.trim_start_matches('/'),
        Some(rest) => rest,
        None if raw.contains("://") || raw.starts_with("data:") => return None,
        None => raw,
    };
    let path = Path::new(p);
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > CAP {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(Image {
        mime: sniff_mime(&bytes)?,
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// What the bytes really are. The extension lies often enough (a webp saved as
/// .png) and Anthropic refuses an image whose declared type does not match.
pub fn sniff_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The images a user message carries, in the order it names them.
// ponytail: replayed history re-sends its images every turn (up to the
// history window); strip refs from older turns if that cost starts to matter.
pub(crate) fn local_images(text: &str) -> Vec<Image> {
    refs(text).into_iter().filter_map(load).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatMessage, ChatRequest};

    #[test]
    fn user_image_refs_become_image_parts_in_every_protocol() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch/images");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a b (1)")).unwrap();
        let png = d.join("a b (1)/x.PNG");
        let jpg = d.join("y.jpg");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(&jpg, [0xff, 0xd8, 0xff]).unwrap();
        std::fs::write(d.join("auth.json"), "{}").unwrap();
        std::fs::write(d.join("fake.png"), "{}").unwrap();
        let s = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let text = format!(
            "olha ![x](<{}>) e ![y]({})\n![sumiu]({}) ![segredo]({}) ![f]({}) ![web](https://e.com/a.png) [link]({})",
            s(&png),
            s(&jpg),
            s(&d.join("nada.png")),
            s(&d.join("auth.json")),
            s(&d.join("fake.png")),
            s(&jpg),
        );
        let imgs = local_images(&text);
        assert_eq!(
            imgs.len(),
            2,
            "missing, non-image, fake png, remote and plain links are skipped"
        );
        assert_eq!(imgs[0].data_url(), "data:image/png;base64,iVBORw0KGgo=");
        assert_eq!(imgs[1].mime, "image/jpeg");

        let req = ChatRequest::new("m", vec![ChatMessage::user(text.clone())]);
        let o = crate::openai::body(&req, false);
        assert_eq!(o["messages"][0]["content"][0]["text"], text.as_str());
        assert_eq!(o["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            o["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
        let a = crate::anthropic::body(&req, false);
        assert_eq!(
            a["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(a["messages"][0]["content"][2]["source"]["type"], "base64");
        let c = crate::codex::body(&req, false);
        assert_eq!(c["input"][0]["content"][1]["type"], "input_image");
        let g = crate::antigravity::body(&req, None);
        assert_eq!(
            g["request"]["contents"][0]["parts"][1]["inlineData"]["data"],
            "iVBORw0KGgo="
        );

        // a plain user message keeps the plain string content
        let plain = ChatRequest::new("m", vec![ChatMessage::user("oi")]);
        assert_eq!(
            crate::openai::body(&plain, false)["messages"][0]["content"],
            "oi"
        );
    }
}
