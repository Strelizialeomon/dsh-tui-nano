//! S04 display state: the official model catalog, the durable model-selection
//! projection, permission presets, and the official command list.
//!
//! This crate never decides whether a selection is allowed and never writes a
//! second configuration. Every field is a parsed view of an official fact; a
//! missing or failed read is kept distinct from a successful one so the UI can
//! show "unknown" or "possibly stale" instead of guessing.

pub mod catalog;
pub mod model;
pub mod panel;

pub use catalog::{
    CatalogFailure, CatalogModel, ModelCatalog, ModelReasoning, ModelReasoningEffort,
    ModelSelection, ProviderGroup,
};
pub use model::{
    CommandEntry, CommandOrigin, ControlsModel, ModelState, PermissionCatalog, PermissionOption,
    PermissionState,
};
pub use panel::{Panel, PanelAction, PanelMode, PanelRow};

/// Maximum command-line bytes accepted by the bridge for one command execution.
pub const MAX_COMMAND_LINE_BYTES: usize = 4096;

/// Maximum model-catalog groups retained for display.
pub const MAX_PROVIDER_GROUPS: usize = 64;

/// Maximum models retained per provider group.
pub const MAX_MODELS_PER_GROUP: usize = 256;

/// Maximum permission options retained for display.
pub const MAX_PERMISSION_OPTIONS: usize = 64;

/// Maximum official commands retained for display.
pub const MAX_COMMANDS: usize = 256;
