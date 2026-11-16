//! Deterministic fact extraction from a user message.
//!
//! No model, no guessing: a fact is captured only when the user says so with a
//! marker (`lembre que ...`, `remember that ...`). Everything else would be an
//! inference dressed as a memory, and the spec forbids showing one as the other.

use crate::backend::{Confidence, FactKind};

/// A fact the user asked to keep, already classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub kind: FactKind,
    pub text: String,
    pub confidence: Confidence,
}

/// Markers that mean "store this". Longest first so `lembre-se que` wins over
/// `lembre que`, and the captured text never keeps the marker.
const MARKERS: &[&str] = &[
    "lembre-se que",
    "lembre que",
    "lembra que",
    "não esqueça que",
    "nao esqueca que",
    "guarde que",
    "anote que",
    "remember that",
    "keep in mind that",
];

/// Rules the user wants applied every time, not facts to recall.
const RULES: &[&str] = &["sempre", "nunca", "always", "never", "prefiro", "i prefer"];

/// Case-insensitive `strip_prefix` that returns a slice of the *original*,
/// so byte offsets stay valid even when lowercasing would change lengths.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let mut rest = s;
    for pc in prefix.chars() {
        let mut it = rest.chars();
        let c = it.next()?;
        if !c.to_lowercase().eq(pc.to_lowercase()) {
            return None;
        }
        rest = it.as_str();
    }
    Some(rest)
}

/// Finds a marker anywhere in the message, preferring a word boundary so
/// `deslembre que` does not match `lembre que`.
fn find_marker(s: &str) -> Option<(usize, &'static str)> {
    for (i, _) in s.char_indices() {
        if i > 0 {
            let prev = s[..i].chars().next_back().unwrap_or(' ');
            if prev.is_alphanumeric() {
                continue;
            }
        }
        let at = &s[i..];
        for m in MARKERS {
            if strip_prefix_ci(at, m).is_some() {
                return Some((i, m));
            }
        }
    }
    None
}

/// Extracts a fact when the message carries a marker, otherwise `None`.
pub fn extract(message: &str) -> Option<Extracted> {
    let (at, marker) = find_marker(message)?;
    let rest = strip_prefix_ci(&message[at..], marker)?;
    let rest = rest.trim_start_matches([',', ':', ';', ' ']);
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let first = rest.split_whitespace().next().unwrap_or("").to_lowercase();
    let kind = if RULES.contains(&first.as_str()) {
        FactKind::Instruction
    } else {
        FactKind::Fact
    };
    Some(Extracted {
        kind,
        text: rest.to_string(),
        confidence: Confidence::Confirmed,
    })
}

/// `extract`, gated by the backend's pause/exclusion switches. This is what a
/// turn should call instead of `extract` directly.
pub fn extract_allowed(
    mem: &dyn crate::MemoryBackend,
    session_id: &str,
    message: &str,
) -> Option<Extracted> {
    match mem.is_extraction_allowed(session_id) {
        Ok(true) => extract(message),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_captures_the_rest_of_the_sentence() {
        let got = extract("lembre que o deploy é às sextas").unwrap();
        assert_eq!(got.text, "o deploy é às sextas");
        assert_eq!(got.kind, FactKind::Fact);
        assert_eq!(got.confidence, Confidence::Confirmed);
    }

    #[test]
    fn politeness_before_the_marker_does_not_block_it() {
        let got = extract("por favor, lembre-se que: uso Rust").unwrap();
        assert_eq!(got.text, "uso Rust");
    }

    #[test]
    fn a_rule_becomes_an_instruction() {
        let got = extract("lembre que sempre quero testes antes do commit").unwrap();
        assert_eq!(got.kind, FactKind::Instruction);
    }

    #[test]
    fn a_marker_inside_a_word_is_not_a_marker() {
        assert_eq!(extract("deslembre que isso exista"), None);
    }

    #[test]
    fn plain_text_captures_nothing() {
        assert_eq!(extract("qual é a capital da França?"), None);
        assert_eq!(extract("lembre que"), None);
    }
}
