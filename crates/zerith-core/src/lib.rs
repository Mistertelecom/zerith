//! Zerith core: product identity, on-disk layout and shared errors.
//!
//! No UI, no provider, no database. Everything here is the small set of
//! facts every other Zerith crate needs.

pub mod paths;

pub use paths::Paths;

/// Product identity. One place, so a rename never means a grep.
pub const NAME: &str = "Zerith";
pub const TAGLINE: &str = "Native AI Agent Harness";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The system prompt for a native session.
///
/// Kept in core because the CLI, the Rune window and a future RPC host must
/// send the same text: a prompt that differs per host makes the provider's
/// prefix cache useless and the model's behavior unpredictable.
///
/// `title` is the session's name. It is the only per-session fact the prompt
/// uses, and it is passed in rather than read from storage so this stays pure.
pub fn system_prompt(title: &str) -> String {
    format!(
        "You are Zerith, a native desktop AI agent harness running on the user's \
         own machine. Answer in the language the user writes in. Be direct and \
         concrete; say what you do not know.\n\nConversation: {title}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_the_session_and_the_product() {
        let p = system_prompt("status do build");
        assert!(p.contains("Zerith"));
        assert!(p.contains("status do build"));
    }
}
