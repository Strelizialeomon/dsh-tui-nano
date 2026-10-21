//! Conversation and input model for the Nano TUI.
//!
//! This crate owns the display-independent half of S02: the ordered input
//! buffer, per-address drafts, the submission state machine, the official
//! inbox projection, live streaming previews, durable message history, and the
//! scene/focus/feature registration entry points. It never reads or writes the
//! official session store and never creates a second transcript database.

pub mod address;
pub mod commands;
pub mod content;
pub mod events;
pub mod history;
pub mod ids;
pub mod input;
pub mod model;
pub mod preview;
pub mod queue;
pub mod scene;
pub mod submissions;

pub use address::{ChildAddress, SessionAddress, SubagentMode};
pub use commands::{CommandOwner, CommandRouter, InputRoute};
pub use content::{ContentError, ContentPart, OrderedContent};
pub use events::{
    parse_conversation_event, parse_session_event, ConversationEvent, ConversationStatus,
    SessionEvent, StreamFrame, WireAddress,
};
pub use history::{
    ConversationHistory, ConversationItem, MessageKind, MessageView, ToolCallView, ToolStatus,
};
pub use ids::new_request_id;
pub use input::{DraftStore, InputBuffer, InputEvent, InputMode, InputOutcome};
pub use model::{ConversationModel, SubmitError, SubmitPlan};
pub use preview::{PreviewAccumulator, PreviewRetirement, PreviewView, ToolPreview};
pub use queue::{InboxView, QueueTarget, QueuedItem};
pub use scene::{
    FeatureContribution, FeatureId, FeatureRegistry, FocusId, FocusStack, SceneId, SceneState,
};
pub use submissions::{Delivery, Submission, SubmissionState, SubmissionTracker};

/// Maximum preview bytes retained per attempt; later chunks are marked truncated.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// Maximum live preview bytes retained across all attempts.
pub const MAX_TOTAL_PREVIEW_BYTES: usize = 1024 * 1024;

/// Maximum durable conversation items retained for display.
pub const MAX_HISTORY_ITEMS: usize = 2_000;
