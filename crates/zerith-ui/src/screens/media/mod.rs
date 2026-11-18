//! Mídia: connect OpenRouter and choose the image and the video model the
//! agent generates with. Listing models needs no key; generating does, and
//! costs money.
//!
//! `state` holds the screen and the backend, `actions` the search state
//! machine and persistence, `params` the fixed-parameter form, `view` the
//! drawing, `demo` fake data for offline snapshots.

mod actions;
pub mod demo;
mod params;
mod state;
mod view;

pub use actions::*;
pub use params::{revalidate, summary, validate, Draft};
pub use state::{kind_label, Backend, KeySource, MediaState, Slot, DEBOUNCE, PAGE};
pub use view::view;
