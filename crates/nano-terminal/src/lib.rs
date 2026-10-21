//! S09 interactive terminal for the Nano TUI.
//!
//! The official terminal service owns the PTY process and the authoritative
//! screen. This crate owns the display parser, attachment-local input
//! sequencing, scrolling, selection and the client phase machine. Terminal
//! output never enters the conversation transcript.

pub mod input;
pub mod scroll;
pub mod selection;
pub mod state;
pub mod store;
pub mod wire;

pub use input::{InputBudget, InputRejection};
pub use scroll::{ScrollCurve, ScrollProfile};
pub use selection::Selection;
pub use state::{
    is_close_key, is_copy_key, is_reattach_key, is_reserved_key, is_shell_key, is_toggle_key,
    new_terminal_id, Command, Failure, Phase, RequestKind, TerminalState,
};
pub use store::{PendingTerminal, StoreFile, TerminalStore};
pub use wire::{
    parse_attachment, parse_create, parse_environment, parse_hold, parse_list, parse_notification,
    parse_shells, EndReason, Notification, ProcessState, Refusal, TerminalAttachment,
    TerminalEnvironment, TerminalInfo, TerminalShell,
};

/// Frozen parser identity reported in evidence. tui-term 0.2 re-exports the
/// matching vt100 0.15.2.
pub const PARSER_CRATE: &str = "vt100 0.15.2";
/// Frozen widget identity.
pub const WIDGET_CRATE: &str = "tui-term 0.2.0";
/// Maximum retained screen rows retained by the official service.
pub const DEFAULT_SCROLLBACK: usize = 1000;
