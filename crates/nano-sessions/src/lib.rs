//! Session selection, controlled main-session switching, naming, context
//! occupancy, and manual compaction for the Nano TUI.
//!
//! This crate is the display half of S07. It never owns a session, a title, a
//! usage sample, or a compaction fact: every value is parsed from an official
//! `nano.*` payload. Unknown or missing official facts stay explicit, so the UI
//! cannot turn an absent measurement into a zero or a guessed session state.
//!
//! The controlled restart itself lives in `nano-app` because it owns the host
//! process; this crate supplies the explicit state machine that makes "stopping
//! the old host", "starting the new host", and "no usable host after a failure"
//! distinct UI states.

pub mod compact;
pub mod context;
pub mod naming;
pub mod summary;
pub mod switch;

pub use compact::{CompactionKind, CompactionOutcome, COMPACTION_CANCELLED};
pub use context::{
    format_age, format_clock, format_tokens, ContextBreakdown, ContextMeter, Occupancy, TokenUsage,
};
pub use naming::RenameOutcome;
pub use summary::{FormatStatus, SessionSummary};
pub use switch::{SwitchMachine, SwitchState, SwitchTarget, TargetMode};
