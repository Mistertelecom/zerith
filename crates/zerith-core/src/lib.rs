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
