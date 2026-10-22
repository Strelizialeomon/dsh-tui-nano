//! Monitor display models for the Nano TUI.
//!
//! S10 observes one official background job at its own absolute byte offset and
//! delivers matching lines through the official Agent `followup`/`inject`
//! surface. This crate owns no process, no second output store, no scheduler,
//! and no model notification. It only displays the bridge's confirmed offsets,
//! budgets, delivery receipts, and UI-only stream facts.

pub mod model;
pub mod view;

pub use model::{stream_is_terminal, MonitorModel, MAX_MONITORS};
pub use view::{
    DeliveryState, MonitorBudgets, MonitorDelivery, MonitorFilter, MonitorFrom, MonitorMode,
    MonitorStream, MonitorUsage, MonitorView,
};
