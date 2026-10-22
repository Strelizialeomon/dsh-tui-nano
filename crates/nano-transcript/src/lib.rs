//! Reading layer for the Nano TUI.
//!
//! S03 consumes S02's official message/tool identities and turns them into a
//! readable transcript: answer, thinking, commands, running output, failures,
//! code diffs, line budgets, in-place expansion, and stable scrolling. It never
//! builds a second conversation history and never rewrites the recorded text.
//!
//! The same renderer backs the D04 read-only and interactive views consumed by
//! S12; decision cards themselves are owned by `nano-decisions`.

pub mod diff;
pub mod display;
pub mod model;
pub mod render;
pub mod scroll;
pub mod tool;
pub mod view;

pub use diff::{DiffHunk, DiffLine, DiffLineKind, DiffState, DiffView};
pub use display::{display_text, line_width, row_count, wrap_line, wrap_text};
pub use model::{
    Enrichment, Entry, EntryKind, PreviewEntry, PreviewTool, TranscriptModel, TranscriptSource,
};
pub use render::{
    render, DiffTheme, Hit, HitKind, RenderCache, TranscriptRender, COMMAND_BUDGET, DIFF_BUDGET,
    FAILURE_BUDGET, MAX_MOUNTED_ROWS, OUTPUT_BUDGET, OVERSCAN_ROWS, THINKING_BUDGET,
};
pub use scroll::{Anchor, EntryOffset, ScrollState, WheelProfile};
pub use tool::{
    ApprovalView, DisplayStatus, ExitStatus, QuestionRecord, RecordedQuestionState, ToolDisplay,
    ToolErrorFact, ToolRecord, ToolResultFact,
};
pub use view::TranscriptView;

use nano_conversation::SessionAddress;
use serde_json::Value;

/// Whether a decision/event payload belongs to the mounted address.
pub(crate) fn address_matches(payload: &Value, address: &SessionAddress) -> bool {
    let Some(value) = payload.get("address") else {
        return false;
    };
    let Some(root) = value.get("rootSessionId").and_then(Value::as_str) else {
        return false;
    };
    if root != address.root_session_id {
        return false;
    }
    match (
        value.get("childSessionId").and_then(Value::as_str),
        address.child.as_ref(),
    ) {
        (None, None) => true,
        (Some(child), Some(current)) => child == current.child_session_id,
        _ => false,
    }
}
