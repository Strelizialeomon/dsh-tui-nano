//! S12 subagent navigation for the Nano TUI.
//!
//! This crate owns the display-independent half of S12: the official child
//! catalog projection, the bottom dock, status lamps, per-child navigation
//! origins, cleanup timing, and the read-only input boundary. It never creates
//! a child, never resumes one, and never reads a child log; the bridge consumes
//! the official subagent and Session services and this crate consumes its wire
//! projection.

pub mod composer;
pub mod lifecycle;
pub mod model;
pub mod nav;
pub mod state;
pub mod wire;

pub use composer::ComposerGate;
pub use lifecycle::{
    Lamp, Lifecycle, LifecycleEvent, StopReason, TerminalFact, BLINK_PERIOD, CLEANUP_DELAY,
};
pub use model::{Activity, AgentMode, AgentRow, Catalog, Diagnostic};
pub use nav::{EntryOrigin, ExitTarget, MountedChild, Navigation};
pub use state::{SubagentState, DOCK_LIMIT, EVENT_KIND};
pub use wire::{
    interrupt_params, parse_description, parse_event, parse_list, parse_snapshot, prompt_params,
    WireError,
};
