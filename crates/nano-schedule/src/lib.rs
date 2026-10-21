//! Schedule surface: official task records, the create/edit form, bounded
//! delivery history, and the list state the Nano UI renders.
//!
//! The crate owns no timer and no task table. Everything here is a projection
//! of the official `schedule` service: catalog entries, compare-and-set update
//! inputs, and history pages. Rendering stays in `nano-app`; this crate decides
//! what is shown and which request a key press produces.

pub mod form;
pub mod model;
pub mod wire;

pub use form::{ScheduleForm, Selector, TextField};
pub use model::{HistoryState, Row, RowKind, ScheduleAction, ScheduleMode, ScheduleModel};
pub use wire::{
    parse_catalog, parse_history, DeliveryReceipt, DeliveryRecord, HistoryPage, ScheduleEntry,
};
