//! One module per screen. Each owns its state struct, its loaders and its
//! `view`; `lib.rs` only routes between them.

pub mod agents;
pub mod chat;
pub mod history;
pub mod media;
pub mod memory;
pub mod providers;
pub mod settings;
pub mod skills;
