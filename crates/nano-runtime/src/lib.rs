//! Rust side of the Nano host boundary.
//!
//! The crate owns process preflight, the fd 3/4 NDJSON transport, the client
//! lifecycle state machine, subscription envelopes, and the bounded shutdown
//! report. It always speaks to the real `dsh` launcher; there is no mock mode.

pub mod frame;
pub mod host;
pub mod ledger;
pub mod lifecycle;
pub mod paths;
pub mod protocol;
pub mod protocol_error;

pub use host::{Host, HostEvent, HostOptions, PendingRequest, SessionMode, ShutdownOutcome};
pub use lifecycle::{Lifecycle, LifecycleState};
pub use paths::{Preflight, RuntimePaths};
pub use protocol::{
    Capability, CapabilityStatus, ConfigSource, CredentialSource, Hello, ProtocolVersion,
    Provenance, RpcError,
};
pub use protocol_error::{HostError, ProtocolError};

/// Wire protocol major version implemented by this crate.
pub const PROTOCOL_MAJOR: u32 = 1;

/// Wire protocol minor version implemented by this crate.
pub const PROTOCOL_MINOR: u32 = 1;

/// Hard frame limit shared with the bridge.
pub const MAX_FRAME_BYTES: usize = 33_554_432;

/// Maximum number of pending requests owned by this client.
pub const MAX_PENDING_REQUESTS: usize = 64;

/// Maximum raw attachment chunk, reserved for S06.
pub const MAX_ATTACHMENT_CHUNK_BYTES: usize = 49_152;

/// Graceful shutdown budget for the host's own cleanup and report.
pub const GRACEFUL_SHUTDOWN_MS: u64 = 10_000;

/// Additional wait after TERM before KILL.
pub const TERM_GRACE_MS: u64 = 3_000;

/// The only official version this build claims.
pub const SUPPORTED_OFFICIAL_VERSION: &str = "0.2.1-rc.2";

/// The dsh profile this crate boots.
pub const PROFILE_NAME: &str = "dsh-tui-nano";

/// Bridge package name installed into the profile.
pub const BRIDGE_PACKAGE: &str = "@dsh-tui-nano/bridge";

/// Lifecycle states in contract order.
pub const LIFECYCLE_STATES: [&str; 9] = [
    "stopped",
    "starting",
    "handshaking",
    "ready",
    "disconnected",
    "stopping",
    "failed",
    "lost",
    "cleanup-failed",
];

/// Lifecycle transitions in contract order.
pub const LIFECYCLE_TRANSITIONS: [(&str, &str); 15] = [
    ("stopped", "starting"),
    ("starting", "handshaking"),
    ("handshaking", "ready"),
    ("starting", "stopping"),
    ("handshaking", "stopping"),
    ("starting", "failed"),
    ("handshaking", "failed"),
    ("ready", "disconnected"),
    ("ready", "lost"),
    ("ready", "stopping"),
    ("disconnected", "lost"),
    ("failed", "stopping"),
    ("lost", "stopping"),
    ("stopping", "stopped"),
    ("stopping", "cleanup-failed"),
];

/// Error code names shared with the bridge contract.
pub const ERROR_CODE_NAMES: [(&str, i64); 74] = [
    ("jsonrpc/parse-error", -32700),
    ("jsonrpc/invalid-request", -32600),
    ("jsonrpc/method-not-found", -32601),
    ("jsonrpc/invalid-params", -32602),
    ("jsonrpc/internal-error", -32603),
    ("nano/not-ready", 1001),
    ("nano/subscription-unknown", 1002),
    ("nano/frame-too-large", 1003),
    ("nano/pending-limit", 1004),
    ("nano/generation-mismatch", 1005),
    ("nano/cancelled", 1006),
    ("nano/cleanup-failed", 1007),
    ("nano/unsupported", 1008),
    ("nano/protocol", 1009),
    ("nano/closed", 1010),
    ("nano/conversation/address", 1011),
    ("nano/conversation/empty", 1012),
    ("nano/conversation/content", 1013),
    ("nano/conversation/queue-item-not-found", 1014),
    ("nano/conversation/queued-attachment", 1015),
    ("nano/conversation/steer-unavailable", 1016),
    ("nano/conversation/rejected", 1017),
    ("nano/conversation/too-large", 1018),
    ("nano/skills/not-found", 1019),
    ("nano/skills/no-preview", 1020),
    ("nano/skills/preview-failed", 1021),
    ("nano/controls/writer-held", 1022),
    ("nano/controls/model-unavailable", 1023),
    ("nano/controls/permission-rejected", 1024),
    ("nano/controls/command-failed", 1025),
    ("nano/controls/unavailable", 1026),
    ("nano/shell/exec-failed", 1027),
    ("nano/jobs/not-found", 1028),
    ("nano/sessions/unavailable", 1029),
    ("nano/sessions/not-active", 1030),
    ("nano/sessions/title-invalid", 1031),
    ("nano/sessions/compact-unavailable", 1032),
    ("nano/sessions/rejected", 1033),
    ("nano/attachments/receipt-unknown", 1034),
    ("nano/attachments/receipt-committed", 1035),
    ("nano/attachments/receipt-incomplete", 1036),
    ("nano/attachments/chunk-sequence", 1037),
    ("nano/attachments/image-invalid", 1038),
    ("nano/attachments/model-no-images", 1039),
    ("nano/attachments/limit", 1040),
    ("nano/attachments/file-failed", 1041),
    ("nano/file-references/failed", 1042),
    ("nano/schedule/unavailable", 1043),
    ("nano/schedule/error", 1044),
    ("nano/decisions/unknown-request", 1045),
    ("nano/decisions/answer-invalid", 1046),
    ("nano/decisions/reply-queued", 1047),
    ("nano/decisions/answer-failed", 1048),
    ("nano/terminal/unavailable", 1049),
    ("nano/terminal/limit-reached", 1050),
    ("nano/terminal/control-unavailable", 1051),
    ("nano/terminal/not-found", 1052),
    ("nano/terminal/input", 1053),
    ("nano/terminal/input-full", 1054),
    ("nano/terminal/closed-id", 1055),
    ("nano/terminal/failed", 1056),
    ("nano/monitor/unavailable", 1057),
    ("nano/monitor/not-found", 1058),
    ("nano/monitor/filter-invalid", 1059),
    ("nano/monitor/budget-invalid", 1060),
    ("nano/monitor/limit-reached", 1061),
    ("nano/subagents/unavailable", 1062),
    ("nano/subagents/unauthorized", 1063),
    ("nano/subagents/not-resumable", 1064),
    ("nano/subagents/parent-unavailable", 1065),
    ("nano/subagents/delivery-unavailable", 1066),
    ("nano/subagents/attachment-invalid", 1067),
    ("nano/subagents/capacity", 1068),
    ("nano/subagents/invalid", 1069),
];

/// Method names implemented by this generation.
pub const IMPLEMENTED_METHODS: [&str; 70] = [
    "nano.hello",
    "nano.startupError",
    "nano.shutdown",
    "nano.subscribe",
    "nano.unsubscribe",
    "nano.cancelRequest",
    "nano.event",
    "nano.streamEnded",
    "nano.attachment.begin",
    "nano.attachment.chunk",
    "nano.attachment.finish",
    "nano.attachment.discard",
    "nano.fileReferences.list",
    "nano.conversation.submit",
    "nano.conversation.editQueued",
    "nano.conversation.deleteQueued",
    "nano.conversation.steerQueued",
    "nano.conversation.cancel",
    "nano.conversation.snapshot",
    "nano.schedule.catalog",
    "nano.schedule.list",
    "nano.schedule.create",
    "nano.schedule.update",
    "nano.schedule.delete",
    "nano.schedule.history",
    "nano.sessions.list",
    "nano.sessions.rename",
    "nano.sessions.context",
    "nano.sessions.compact",
    "nano.sessions.compactCancel",
    "nano.skills.list",
    "nano.skills.preview",
    "nano.instructions.snapshot",
    "nano.controls.state",
    "nano.controls.modelCatalog",
    "nano.controls.selectModel",
    "nano.controls.setPermission",
    "nano.controls.executeCommand",
    "nano.shell.exec",
    "nano.jobs.snapshot",
    "nano.jobs.follow",
    "nano.jobs.kill",
    "nano.monitor.create",
    "nano.monitor.list",
    "nano.monitor.stop",
    "nano.monitor.resetBudget",
    "nano.approval.answer",
    "nano.question.answer",
    "nano.decisions.snapshot",
    "nano.terminal.environment",
    "nano.terminal.shells",
    "nano.terminal.list",
    "nano.terminal.create",
    "nano.terminal.retain",
    "nano.terminal.release",
    "nano.terminal.attach",
    "nano.terminal.detach",
    "nano.terminal.rename",
    "nano.terminal.close",
    "nano.terminal.input",
    "nano.terminal.resize",
    "nano.terminal.output",
    "nano.terminal.state",
    "nano.terminal.ack",
    "nano.terminal.ended",
    "nano.subagents.snapshot",
    "nano.subagents.list",
    "nano.subagents.describe",
    "nano.subagents.prompt",
    "nano.subagents.interrupt",
];
/// Host-to-client terminal notifications understood by this client.
pub const TERMINAL_NOTIFICATION_METHODS: [&str; 4] = [
    "nano.terminal.output",
    "nano.terminal.state",
    "nano.terminal.ack",
    "nano.terminal.ended",
];

/// Event kinds this client accepts.
pub const EVENT_KINDS: [&str; 17] = [
    "session.event",
    "conversation.status",
    "conversation.inbox",
    "conversation.stream",
    "schedule.changed",
    "controls.model",
    "controls.permission",
    "sessions.context",
    "skills.changed",
    "instructions.changed",
    "jobs.event",
    "jobs.output",
    "decisions.approval",
    "decisions.question",
    "decisions.projection",
    "monitor.updated",
    "subagents.event",
];

/// Shutdown report step names, in report order.
pub const SHUTDOWN_STEP_NAMES: [&str; 6] = [
    "accepting",
    "subscriptions",
    "agents",
    "persistence",
    "dispose",
    "ledger",
];

/// Maximum durable events replayed by one subscribe request.
pub const MAX_REPLAY_EVENTS: u64 = 10_000;

/// Startup error codes accepted from the host.
pub const STARTUP_ERROR_CODES: [&str; 4] = [
    "occupied",
    "startup-failed",
    "cleanup-failed",
    "unsupported",
];
