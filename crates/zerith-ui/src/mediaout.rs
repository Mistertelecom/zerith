//! Generated media in the chat: what a `generate_image` / `generate_video`
//! result says, which files the window may show, and the loader that decodes
//! them.
//!
//! The window shows only LOCAL files under the media folder. A remote URL, a
//! `data:` URL, a `file:` URL, a relative path or anything with `..` is
//! refused here, before any decoder or opener sees it.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rune::{Image, ImageCache, Waker};

/// Decoded pictures kept alive. The cache drops pixels when the last handle
/// goes, and the view builds fresh elements every frame, so the window holds
/// the handles itself, up to [`KEEP`] files.
const KEEP: usize = 48;

/// A picture a result mentions.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub alt: String,
    pub path: PathBuf,
}

/// A video file a result mentions. Shown as a card, never played in the app.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub path: PathBuf,
    /// "812 KB" as the tool wrote it; empty when absent.
    pub size: String,
}

/// What a generation result carries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaOut {
    pub pictures: Vec<Picture>,
    pub clips: Vec<Clip>,
    /// The text after `Custo:`, e.g. `US$ 0,0400` or `não informado pelo provedor`.
    pub cost: Option<String>,
}

impl MediaOut {
    pub fn is_empty(&self) -> bool {
        self.pictures.is_empty() && self.clips.is_empty()
    }
}

/// The name a tool row shows: what it does, not its identifier.
pub fn tool_title(name: &str) -> Option<&'static str> {
    match name {
        "generate_image" => Some("Gerar imagem"),
        "generate_video" => Some("Gerar vídeo"),
        _ => None,
    }
}

fn norm(p: &str) -> String {
    p.replace('\\', "/")
}

/// Whether `path` is an absolute path under `dir`, with no `..`, no scheme.
/// Lexical on purpose: a view must not touch the disk.
pub fn is_under(dir: &Path, path: &Path) -> bool {
    let s = path.to_string_lossy();
    // `http://x`, `data:image/…`, `file:///x`. A Windows drive (`C:\`) is a
    // single letter before the colon and is not a scheme.
    if let Some((scheme, _)) = s.split_once(':') {
        if scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
    }
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    let (p, d) = (norm(&s), norm(&dir.to_string_lossy()));
    let d = d.trim_end_matches('/');
    let same = |a: &str, b: &str| {
        if cfg!(windows) {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    p.len() > d.len() + 1
        && p.is_char_boundary(d.len())
        && same(&p[..d.len()], d)
        && p.as_bytes()[d.len()] == b'/'
}

fn is_video(path: &str, mime: &str) -> bool {
    mime.starts_with("video/") || path.to_ascii_lowercase().ends_with(".mp4")
}

fn is_image_file(path: &str, mime: &str) -> bool {
    mime.starts_with("image/")
        || [".png", ".jpg", ".jpeg", ".webp", ".gif"]
            .iter()
            .any(|e| path.to_ascii_lowercase().ends_with(e))
}

/// `![alt](path)` or `![alt](<path with spaces>)` on one line.
fn markdown_image(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix("![")?;
    let close = rest.find("](")?;
    let alt = rest[..close].to_string();
    let target = rest[close + 2..].strip_suffix(')')?;
    let target = target
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .unwrap_or(target);
    Some((alt, target.to_string()))
}

/// Reads a tool result. Only files under `dir` are kept; the rest of the text
/// stays where it is (the tool row's own preview).
pub fn parse(text: &str, dir: &Path) -> MediaOut {
    let mut out = MediaOut::default();
    let mut seen: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some((alt, target)) = markdown_image(line) {
            let path = PathBuf::from(&target);
            if !is_under(dir, &path) {
                continue;
            }
            if seen.contains(&norm(&target)) {
                // The `Arquivo:` line came first and has no alt text.
                if let Some(p) = out
                    .pictures
                    .iter_mut()
                    .find(|p| norm(&p.path.to_string_lossy()) == norm(&target))
                {
                    if p.alt.is_empty() {
                        p.alt = alt;
                    }
                }
            } else {
                seen.push(norm(&target));
                out.pictures.push(Picture { alt, path });
            }
        } else if let Some(rest) = line.trim().strip_prefix("Arquivo:") {
            // `Arquivo: <path> (<mime>, <n> KB)`; the path may hold spaces.
            let rest = rest.trim();
            let (path, meta) = match rest.rfind(" (") {
                Some(i) => (&rest[..i], rest[i + 2..].trim_end_matches(')')),
                None => (rest, ""),
            };
            let (mime, size) = meta.split_once(',').unwrap_or((meta, ""));
            let (mime, size) = (mime.trim(), size.trim());
            let p = PathBuf::from(path);
            if !is_under(dir, &p) || seen.contains(&norm(path)) {
                continue;
            }
            if is_video(path, mime) {
                seen.push(norm(path));
                out.clips.push(Clip {
                    path: p,
                    size: size.to_string(),
                });
            } else if is_image_file(path, mime) {
                seen.push(norm(path));
                out.pictures.push(Picture {
                    alt: String::new(),
                    path: p,
                });
            }
        } else if let Some(c) = line.trim().strip_prefix("Custo:") {
            out.cost = Some(c.trim().to_string());
        }
    }
    out
}

/// Shared decoder for the window's local pictures.
pub struct Images {
    cache: ImageCache,
    held: Mutex<HashMap<PathBuf, Image>>,
}

impl Images {
    pub fn new(waker: Waker) -> Self {
        Self {
            cache: ImageCache::new(waker),
            held: Mutex::new(HashMap::new()),
        }
    }

    fn held(&self) -> MutexGuard<'_, HashMap<PathBuf, Image>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The picture at `path`: decoded on the cache's worker, never in a frame.
    /// Callers have already checked [`is_under`].
    pub fn get(&self, path: &Path) -> Image {
        let mut held = self.held();
        if let Some(i) = held.get(path) {
            return i.clone();
        }
        if held.len() >= KEEP {
            held.clear();
        }
        let image = self.cache.file(path, 0.0, 1.0);
        held.insert(path.to_path_buf(), image.clone());
        image
    }

    /// A picture compiled into the binary (logos), cached by `key`.
    pub fn embedded(&self, key: &str, bytes: &'static [u8]) -> Image {
        let k = PathBuf::from(format!("embedded:{key}"));
        self.held()
            .entry(k)
            .or_insert_with(|| self.cache.load(bytes))
            .clone()
    }

    /// An embedded SVG, rasterized for `size_pt` at 2x so it stays crisp.
    pub fn embedded_svg(&self, key: &str, bytes: &'static [u8], size_pt: f32) -> Image {
        let k = PathBuf::from(format!("embedded:{key}:{size_pt}"));
        self.held()
            .entry(k)
            .or_insert_with(|| self.cache.svg(bytes, size_pt, 2.0))
            .clone()
    }

    /// Seeds `path` with pixels already in memory (demo scenes and tests: no
    /// file needed).
    pub fn seed(&self, path: &Path, width: u32, height: u32, rgba: Vec<u8>) -> Image {
        let image = self.cache.rgba(width, height, rgba);
        self.held().insert(path.to_path_buf(), image.clone());
        image
    }

    /// What `Markdown::images` asks for: `src` as written in the answer. Only
    /// a local file under `dir` gets a picture; everything else gets `None`
    /// and the kit draws its alt text.
    pub fn for_markdown(&self, dir: &Path, src: &str) -> Option<Image> {
        let path = PathBuf::from(src.trim());
        is_under(dir, &path).then(|| self.get(&path))
    }
}

/// A gradient of `w`×`h` for demo scenes: a calm blue-to-violet wash.
pub fn demo_pixels(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            v.extend_from_slice(&[
                (70.0 + 120.0 * fx) as u8,
                (110.0 + 60.0 * (1.0 - fy)) as u8,
                (200.0 - 40.0 * fx + 30.0 * fy) as u8,
                255,
            ]);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Users\x\.zerith\media")
        } else {
            PathBuf::from("/home/x/.zerith/media")
        }
    }

    fn inside(name: &str) -> String {
        format!("{}/{name}", dir().display()).replace('\\', "/")
    }

    #[test]
    fn only_local_files_under_the_media_folder_pass() {
        let d = dir();
        assert!(is_under(&d, Path::new(&inside("a.png"))));
        assert!(is_under(&d, Path::new(&inside("2026/b.png"))));
        for bad in [
            "https://evil.example/a.png",
            "http://127.0.0.1/a.png",
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
            "a.png",
            "../media/a.png",
        ] {
            assert!(!is_under(&d, Path::new(bad)), "{bad}");
        }
        assert!(!is_under(&d, Path::new(&inside("../secret.png"))));
        // A sibling folder with the same prefix is not inside.
        let sibling = format!("{}-other/a.png", d.display()).replace('\\', "/");
        assert!(!is_under(&d, Path::new(&sibling)));
        // The folder itself is not a file.
        assert!(!is_under(&d, &d));
    }

    #[test]
    fn a_result_gives_pictures_cost_and_dedups_the_file_line() {
        let a = inside("cat.png");
        let text = format!(
            "Imagem gerada com x/y.\nParâmetros enviados: n=1\nCusto: US$ 0,0400\nArquivo: {a} (image/png, 12 KB)\n![um gato]({a})\nCole a linha acima."
        );
        let m = parse(&text, &dir());
        assert_eq!(m.pictures.len(), 1, "same file once: {m:?}");
        assert_eq!(
            m.pictures[0].alt, "um gato",
            "alt comes from the Markdown line"
        );
        assert_eq!(m.cost.as_deref(), Some("US$ 0,0400"));
        assert!(m.clips.is_empty());
    }

    #[test]
    fn markdown_with_angle_brackets_and_a_video_card() {
        let a = inside("meu gato.png");
        let v = inside("clip.mp4");
        let m = parse(
            &format!("![gato]( <{a}> )\n![gato](<{a}>)\nArquivo: {v} (video/mp4, 812 KB)"),
            &dir(),
        );
        assert_eq!(m.pictures.len(), 1);
        assert_eq!(m.pictures[0].alt, "gato");
        assert_eq!(m.clips.len(), 1);
        assert_eq!(m.clips[0].size, "812 KB");
    }

    #[test]
    fn a_remote_image_line_is_ignored() {
        let m = parse(
            "![x](https://evil.example/a.png)\nArquivo: https://x/a.mp4 (video/mp4, 1 KB)",
            &dir(),
        );
        assert!(m.is_empty());
    }

    #[test]
    fn tool_rows_get_names() {
        assert_eq!(tool_title("generate_image"), Some("Gerar imagem"));
        assert_eq!(tool_title("generate_video"), Some("Gerar vídeo"));
        assert_eq!(tool_title("shell"), None);
    }
}
