//! Agent Skills: discovery, validation and safe installation.
//!
//! Two rules from the spec shape this crate:
//!
//! 1. **The format is the open one** (agentskills.io), not a Zerith dialect.
//! 2. **Nothing found is executed.** Reading a `SKILL.md` and listing its
//!    scripts is inspection; running them is a separate, explicit action the
//!    user takes after seeing the scan. A malicious skill cannot get code
//!    execution from installing it.

pub mod author;
pub mod catalog;
pub mod frontmatter;
pub mod install;
pub mod remote;
pub mod scan;

pub use frontmatter::{parse, Frontmatter, FrontmatterError};
pub use install::{Installed, Library, Status};
pub use scan::{discover, scan, Finding, Severity, Skill, SkillError};
