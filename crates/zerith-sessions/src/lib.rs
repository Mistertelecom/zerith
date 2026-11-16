//! Durable conversations: SQLite store, versioned migrations, WAL.
//!
//! One database file holds both sessions and memory (zerith-memory owns its
//! own tables in the same file). Durability rule from the spec: a message the
//! user already saw is committed in the same transaction that closes it;
//! streaming deltas are never written one row each.

pub mod migrations;
pub mod ops;
pub mod plan;
pub mod store;
pub mod types;

pub use ops::{MessageHit, SessionFilter};
pub use plan::{NewTask, Task, TaskState};
pub use store::Store;
pub use types::{Message, NewMessage, Role, Session, SessionSource};
