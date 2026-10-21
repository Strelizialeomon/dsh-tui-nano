//! Error taxonomy for the Rust host boundary.
//!
//! Startup failures, transport failures, and protocol violations stay distinct
//! so callers can report the four shutdown facts honestly instead of collapsing
//! everything into "disconnected".

use std::fmt;
use std::io;

/// A frame or payload violated the versioned protocol contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// The line is not JSON.
    InvalidJson(String),
    /// The JSON object is not a valid v1 frame.
    InvalidFrame(String),
    /// The host claimed a different protocol major.
    UnsupportedMajor(u32),
    /// A capability entry used a status this build does not understand.
    UnknownCapabilityStatus(String),
    /// The host declared a required capability this build does not know.
    UnsupportedRequiredCapability(String),
    /// The host delivered an event kind outside the contract.
    UnknownEventKind(String),
    /// A frame exceeded the declared byte limit.
    FrameTooLarge(usize),
    /// The host's generation does not match this process's generation.
    GenerationMismatch,
    /// The host's runtime identity does not match this process's runtime identity.
    RuntimeMismatch,
    /// Fallback-transport nonce mismatch.
    NonceMismatch,
    /// A declared limit differs from the contract.
    LimitMismatch(&'static str),
    /// A startup error code is outside the contract.
    UnknownStartupCode(String),
    /// A response carried both result and error, or neither.
    MalformedResponse(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => write!(formatter, "invalid JSON: {message}"),
            Self::InvalidFrame(message) => write!(formatter, "invalid frame: {message}"),
            Self::UnsupportedMajor(major) => write!(formatter, "unsupported protocol major {major}"),
            Self::UnknownCapabilityStatus(status) => write!(formatter, "unknown capability status {status}"),
            Self::UnsupportedRequiredCapability(name) => write!(formatter, "unknown required capability {name}"),
            Self::UnknownEventKind(kind) => write!(formatter, "unknown event kind {kind}"),
            Self::FrameTooLarge(bytes) => write!(formatter, "frame of {bytes} bytes exceeds the limit"),
            Self::GenerationMismatch => write!(formatter, "frame belongs to another host generation"),
            Self::RuntimeMismatch => write!(formatter, "frame belongs to another runtime identity"),
            Self::NonceMismatch => write!(formatter, "frame nonce does not match this generation"),
            Self::LimitMismatch(field) => write!(formatter, "declared limit {field} does not match the contract"),
            Self::UnknownStartupCode(code) => write!(formatter, "unknown startup error code {code}"),
            Self::MalformedResponse(message) => write!(formatter, "malformed response: {message}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// A JSON-RPC error returned by the host.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct RpcErrorBody {
    /// Numeric code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A request failed with an explicit host-side error code.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRpcError {
    /// Numeric code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured details.
    pub data: Option<serde_json::Value>,
}

impl fmt::Display for HostRpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for HostRpcError {}

/// Any failure surfaced by the Rust runtime.
#[derive(Debug)]
pub enum HostError {
    /// Local I/O failed.
    Io(io::Error),
    /// The dsh launcher could not be started.
    Spawn(String),
    /// The installed official version is outside the verified matrix.
    UnsupportedVersion(String),
    /// The host exited with `nano.startupError` before the handshake.
    Startup {
        /// Contract startup code.
        code: String,
        /// Sanitized message from the host.
        message: String,
        /// Holder pid for `occupied`.
        holder_pid: Option<u32>,
        /// Repair path for ledger failures.
        repair_path: Option<String>,
    },
    /// The host transport closed.
    TransportClosed,
    /// Too many requests are already waiting.
    PendingLimit,
    /// A request returned a JSON-RPC error.
    Rpc(HostRpcError),
    /// An operation timed out.
    Timeout(String),
    /// A lifecycle transition was invalid.
    Lifecycle(String),
    /// The handshake failed.
    Handshake(String),
    /// The host sent a frame this build cannot accept.
    Protocol(ProtocolError),
    /// The host process is gone.
    ProcessGone,
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Spawn(message) => write!(formatter, "could not start the host: {message}"),
            Self::UnsupportedVersion(version) => write!(formatter, "unsupported official dsh version {version}"),
            Self::Startup {
                code,
                message,
                holder_pid,
                ..
            } => match holder_pid {
                Some(pid) => write!(formatter, "host startup failed ({code}) by pid {pid}: {message}"),
                None => write!(formatter, "host startup failed ({code}): {message}"),
            },
            Self::TransportClosed => write!(formatter, "host transport closed"),
            Self::PendingLimit => write!(formatter, "too many pending host requests"),
            Self::Rpc(error) => write!(formatter, "{error}"),
            Self::Timeout(message) => write!(formatter, "timed out: {message}"),
            Self::Lifecycle(message) => write!(formatter, "invalid lifecycle transition: {message}"),
            Self::Handshake(message) => write!(formatter, "handshake failed: {message}"),
            Self::Protocol(error) => write!(formatter, "protocol error: {error}"),
            Self::ProcessGone => write!(formatter, "host process is gone"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<io::Error> for HostError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for HostError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<HostRpcError> for HostError {
    fn from(error: HostRpcError) -> Self {
        Self::Rpc(error)
    }
}
