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

pub use host::{Host, HostEvent, HostOptions, SessionMode, ShutdownOutcome};
pub use lifecycle::{Lifecycle, LifecycleState};
pub use paths::{Preflight, RuntimePaths};
pub use protocol::{Capability, CapabilityStatus, Hello, ProtocolVersion, RpcError};
pub use protocol_error::{HostError, ProtocolError};

/// Wire protocol major version implemented by this crate.
pub const PROTOCOL_MAJOR: u32 = 1;

/// Wire protocol minor version implemented by this crate.
pub const PROTOCOL_MINOR: u32 = 0;

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
pub const LIFECYCLE_TRANSITIONS: [(&str, &str); 16] = [
    ("stopped", "starting"),
    ("starting", "handshaking"),
    ("handshaking", "ready"),
    ("starting", "stopping"),
    ("handshaking", "stopping"),
    ("starting", "failed"),
    ("handshaking", "failed"),
    ("ready", "disconnected"),
    ("disconnected", "ready"),
    ("ready", "lost"),
    ("ready", "stopping"),
    ("disconnected", "lost"),
    ("failed", "stopping"),
    ("lost", "stopping"),
    ("stopping", "stopped"),
    ("stopping", "cleanup-failed"),
];

/// Error code names shared with the bridge contract.
pub const ERROR_CODE_NAMES: [(&str, i64); 15] = [
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
];

/// Method names implemented by this generation.
pub const IMPLEMENTED_METHODS: [&str; 8] = [
    "nano.hello",
    "nano.startupError",
    "nano.shutdown",
    "nano.subscribe",
    "nano.unsubscribe",
    "nano.cancelRequest",
    "nano.event",
    "nano.streamEnded",
];

/// Event kinds this client accepts.
pub const EVENT_KINDS: [&str; 1] = ["session.event"];

/// Startup error codes accepted from the host.
pub const STARTUP_ERROR_CODES: [&str; 4] = ["occupied", "startup-failed", "cleanup-failed", "unsupported"];
