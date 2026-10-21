//! Skill discovery, preview, workspace-instruction sources, and user-invocation
//! facts for the Nano TUI.
//!
//! This crate owns the display-independent half of S05. The official registry
//! and instruction loader remain the only sources of truth: the wire values
//! parsed here are absolute projections produced by `bridge/skills/`, and a
//! failed refresh never erases the last usable catalog.

pub mod catalog;
pub mod feature;
pub mod instructions;
pub mod invocation;
pub mod preview;

pub use catalog::{NameResolution, SkillCatalog, SkillEntry, SkillError};
pub use feature::contribution;
pub use instructions::{InstructionAction, InstructionSource, InstructionState};
pub use invocation::{InvocationFact, InvocationTracker};
pub use preview::SkillPreview;

/// Maximum instruction sources the bridge keeps in one projection.
pub const MAX_INSTRUCTION_SOURCES: usize = 512;

/// Maximum code points of a budget notice the bridge keeps.
pub const MAX_BUDGET_NOTICE_CHARS: usize = 8192;

/// Maximum bytes a skill preview reads; matches `protocol.limits`.
pub const MAX_PREVIEW_BYTES: u64 = 65_536;
