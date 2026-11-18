//! The Agent Skills `SKILL.md` header, parsed without a YAML dependency.
//!
//! The spec's frontmatter is a flat map plus one nested `metadata` map of
//! string keys. Writing a 900-line YAML parser for that would be a liability:
//! this reads exactly the shape the spec defines and rejects the rest loudly.
//!
//! Reference: <https://agentskills.io/specification>

/// The parsed header. Every optional field is `None` when absent, never a
/// guessed default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    /// Arbitrary `metadata:` key/value pairs, kept as strings per the spec.
    pub metadata: Vec<(String, String)>,
    pub allowed_tools: Option<String>,
    /// Header fields the spec does not define (`tags`, `requirements`,
    /// `version`...), kept verbatim as `(key, raw value)`. They are shown to the
    /// person and never acted on: a skill cannot make the harness do something
    /// by naming it here.
    pub extra: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontmatterError {
    #[error("SKILL.md has no `---` frontmatter block")]
    NoFrontmatter,
    #[error("frontmatter is not closed with `---`")]
    Unterminated,
    #[error("{0}")]
    Field(&'static str),
    #[error("unknown frontmatter field: {0}")]
    Unknown(String),
}

impl Frontmatter {
    /// `allowed-tools` split on whitespace, as the spec defines it
    /// (`Bash(git:*) Read`). `None` means the skill declared nothing, which is
    /// different from an empty list.
    pub fn allowed_tools_list(&self) -> Option<Vec<String>> {
        self.allowed_tools
            .as_ref()
            .map(|s| s.split_whitespace().map(str::to_string).collect())
    }
}

/// Splits a document into its frontmatter lines and its body.
fn split(text: &str) -> Result<(Vec<&str>, &str), FrontmatterError> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    if first.trim_end() != "---" {
        return Err(FrontmatterError::NoFrontmatter);
    }
    let mut header = Vec::new();
    let mut body_start = 0usize;
    let mut consumed = first.len() + 1;
    for line in lines {
        if line.trim_end() == "---" {
            body_start = consumed + line.len() + 1;
            break;
        }
        header.push(line);
        consumed += line.len() + 1;
    }
    if body_start == 0 {
        return Err(FrontmatterError::Unterminated);
    }
    let body = text.get(body_start..).unwrap_or("");
    Ok((header, body))
}

/// Parses the frontmatter and returns it with the instruction body.
pub fn parse(text: &str) -> Result<(Frontmatter, &str), FrontmatterError> {
    let (header, body) = split(text)?;
    let mut fm = Frontmatter::default();
    let mut in_metadata = false;
    // Where a continuation line (an indented value, a `- item`) belongs.
    let mut open_extra: Option<usize> = None;
    let mut open_folded = false;
    for line in header {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with([' ', '\t']) || line.trim_start().starts_with("- ");
        // A folded block (`description: >`) or a list under an unknown key:
        // the continuation lines belong to the previous field.
        if indented && !in_metadata {
            if open_folded {
                let cont = line.trim();
                if fm.description.is_empty() {
                    fm.description = cont.to_string();
                } else {
                    fm.description.push(' ');
                    fm.description.push_str(cont);
                }
                continue;
            }
            if let Some(i) = open_extra {
                let slot = &mut fm.extra[i].1;
                if !slot.is_empty() {
                    slot.push_str(", ");
                }
                slot.push_str(line.trim().trim_start_matches("- ").trim());
                continue;
            }
        }
        open_extra = None;
        open_folded = false;
        // Nested `metadata:` entries are indented under the key.
        if in_metadata {
            if let Some(rest) = line.strip_prefix("  ") {
                // A `- item` under a nested metadata key (`tags:` list):
                // folded into the last value, as top-level lists are.
                if let Some(item) = rest.trim_start().strip_prefix("- ") {
                    if let Some(last) = fm.metadata.last_mut() {
                        if !last.1.is_empty() {
                            last.1.push_str(", ");
                        }
                        last.1.push_str(&unquote(item.trim()));
                    }
                    continue;
                }
                if let Some((k, v)) = rest.split_once(':') {
                    fm.metadata.push((k.trim().to_string(), unquote(v.trim())));
                    continue;
                }
            }
            in_metadata = false;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(FrontmatterError::Field("line is not `key: value`"));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => fm.name = unquote(value),
            "description" => {
                if matches!(value, ">" | "|" | ">-" | "|-") {
                    open_folded = true;
                    fm.description.clear();
                } else {
                    fm.description = unquote(value);
                }
            }
            "license" => fm.license = Some(unquote(value)),
            "compatibility" => fm.compatibility = Some(unquote(value)),
            "allowed-tools" => fm.allowed_tools = Some(unquote(value)),
            "metadata" => {
                if !value.is_empty() && value != "{}" {
                    return Err(FrontmatterError::Field(
                        "metadata must be a nested map, not an inline value",
                    ));
                }
                in_metadata = true;
            }
            other => {
                // A key the spec does not define. Kept for display; a typo like
                // `descripton` still fails because `description` is then
                // missing and `validate` says so.
                fm.extra.push((other.to_string(), unquote(value)));
                open_extra = Some(fm.extra.len() - 1);
            }
        }
    }
    validate(&fm)?;
    Ok((fm, body))
}

/// Strips one layer of matching quotes; YAML lets both styles.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Enforces the constraints the spec states, not the ones a parser happens to
/// accept: a skill with a bad name must fail at install, not at load.
fn validate(fm: &Frontmatter) -> Result<(), FrontmatterError> {
    if fm.name.is_empty() {
        return Err(FrontmatterError::Field("name is required"));
    }
    if fm.name.len() > 64 {
        return Err(FrontmatterError::Field("name is longer than 64 characters"));
    }
    if fm.name.starts_with('-') || fm.name.ends_with('-') {
        return Err(FrontmatterError::Field(
            "name cannot start or end with a hyphen",
        ));
    }
    if fm.name.contains("--") {
        return Err(FrontmatterError::Field(
            "name cannot contain consecutive hyphens",
        ));
    }
    if !fm
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(FrontmatterError::Field(
            "name may only contain lowercase letters, digits and hyphens",
        ));
    }
    if fm.description.trim().is_empty() {
        return Err(FrontmatterError::Field("description is required"));
    }
    if fm.description.chars().count() > 1024 {
        return Err(FrontmatterError::Field(
            "description is longer than 1024 characters",
        ));
    }
    if let Some(c) = &fm.compatibility {
        if c.chars().count() > 500 {
            return Err(FrontmatterError::Field(
                "compatibility is longer than 500 characters",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "---\nname: pdf-processing\ndescription: Extracts text from PDFs.\n---\n# Steps\n1. Do it.\n";

    #[test]
    fn the_minimal_document_from_the_spec_parses() {
        let (fm, body) = parse(MINIMAL).unwrap();
        assert_eq!(fm.name, "pdf-processing");
        assert_eq!(fm.description, "Extracts text from PDFs.");
        assert!(body.contains("# Steps"));
    }

    #[test]
    fn optional_fields_and_nested_metadata_are_kept() {
        let doc = "---\nname: a-b\ndescription: d\nlicense: Apache-2.0\ncompatibility: Needs git\nmetadata:\n  author: org\n  version: \"1.0\"\nallowed-tools: Bash(git:*) Read\n---\nbody";
        let (fm, _) = parse(doc).unwrap();
        assert_eq!(fm.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(fm.compatibility.as_deref(), Some("Needs git"));
        assert_eq!(
            fm.metadata,
            vec![
                ("author".to_string(), "org".to_string()),
                ("version".to_string(), "1.0".to_string()),
            ]
        );
        assert_eq!(fm.allowed_tools.as_deref(), Some("Bash(git:*) Read"));
        assert_eq!(
            fm.allowed_tools_list(),
            Some(vec!["Bash(git:*)".to_string(), "Read".to_string()])
        );
        assert_eq!(parse(MINIMAL).unwrap().0.allowed_tools_list(), None);
    }

    #[test]
    fn an_uppercase_or_double_hyphen_name_is_rejected() {
        for bad in ["PDF", "-pdf", "pdf-", "pdf--x", "pdf_x"] {
            let doc = format!("---\nname: {bad}\ndescription: d\n---\n");
            assert!(matches!(parse(&doc), Err(FrontmatterError::Field(_))));
        }
    }

    #[test]
    fn missing_pieces_are_named_not_guessed() {
        assert_eq!(parse("no header"), Err(FrontmatterError::NoFrontmatter));
        assert_eq!(
            parse("---\nname: a\ndescription: d\n"),
            Err(FrontmatterError::Unterminated)
        );
        assert!(matches!(
            parse("---\nname: a\n---\n"),
            Err(FrontmatterError::Field(_))
        ));
        // A key the spec does not define is kept for display, not an error.
        let (fm, _) = parse("---\nname: a\ndescription: d\ncolor: red\n---\n").unwrap();
        assert_eq!(fm.extra, vec![("color".to_string(), "red".to_string())]);
    }

    #[test]
    fn a_typo_in_a_required_field_still_fails() {
        // `descripton` becomes an extra field, so `description` is missing.
        assert!(matches!(
            parse("---\nname: a\ndescripton: d\n---\n"),
            Err(FrontmatterError::Field(_))
        ));
    }

    #[test]
    fn folded_descriptions_and_list_fields_from_real_skills_parse() {
        let doc = "---\nname: agent-desktop\ntags: computer-use, gui\nrequirements:\n  - agent-desktop\n  - node\ndescription: >\n  Operate desktop apps\n  through accessibility.\n---\nbody";
        let (fm, body) = parse(doc).unwrap();
        assert_eq!(
            fm.description,
            "Operate desktop apps through accessibility."
        );
        assert_eq!(fm.extra[0], ("tags".into(), "computer-use, gui".into()));
        assert_eq!(
            fm.extra[1],
            ("requirements".into(), "agent-desktop, node".into())
        );
        assert_eq!(body, "body");
    }

    #[test]
    fn lists_nested_in_metadata_parse() {
        let doc = "---\nname: a\ndescription: d\nmetadata:\n  hermes:\n    tags:\n      - solar\n      - \"mppt\"\n    related_skills:\n      - x\n---\nbody";
        let (fm, _) = parse(doc).unwrap();
        assert_eq!(fm.metadata[1], ("tags".into(), "solar, mppt".into()));
        assert_eq!(fm.metadata[2], ("related_skills".into(), "x".into()));
    }
}
