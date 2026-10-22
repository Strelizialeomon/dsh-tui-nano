//! Wire types and validation for protocol v1.
//!
//! Unknown optional fields are ignored (a later minor may add them). Unknown
//! required capabilities, unknown event kinds, a different major, a wrong
//! generation, or a wrong fallback nonce are hard errors.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::protocol_error::{ProtocolError, RpcErrorBody};
use crate::{
    EVENT_KINDS, MAX_ATTACHMENT_CHUNK_BYTES, MAX_FRAME_BYTES, MAX_PENDING_REQUESTS, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, STARTUP_ERROR_CODES,
};

/// Alias used by callers that only need the response error body.
pub type RpcError = RpcErrorBody;

/// Protocol major/minor pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    /// Major version; a mismatch is a hard refusal.
    pub major: u32,
    /// Minor version; newer optional fields may be ignored.
    pub minor: u32,
}

/// Version facts reported in the handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    /// Official dsh version.
    pub official: String,
    /// Bridge package version.
    pub bridge: String,
    /// Node runtime version.
    pub node: String,
}

/// Whether this host created or resumed its root session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionFactMode {
    /// A new session was created.
    Created,
    /// An existing session was resumed.
    Resumed,
}

/// Root-session facts in the handshake.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFacts {
    /// Created or resumed.
    pub mode: SessionFactMode,
    /// Durable event count observed after creation/resume.
    pub seq: u64,
    /// Opaque official session header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Value>,
}

/// Three-state capability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityStatus {
    /// Composed and usable now.
    Supported,
    /// Composed but blocked by a missing dependency at this moment.
    Unavailable,
    /// Not composed in this profile.
    Unsupported,
}

/// One capability entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    /// Three-state status.
    pub status: CapabilityStatus,
    /// Optional mode, such as the `tool-ask-user` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Optional human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A later minor may mark an unknown capability as required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Capacity limits echoed by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// Maximum frame bytes.
    pub max_frame_bytes: u64,
    /// Maximum pending requests.
    pub max_pending_requests: u64,
    /// Maximum raw attachment chunk bytes.
    pub max_attachment_chunk_bytes: u64,
}

/// Deployment image limits reported when the attachment service is composed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentLimits {
    /// Maximum encoded source bytes per image.
    pub max_image_bytes: u64,
    /// Maximum images in one submitted message.
    pub max_images_per_message: u64,
    /// Maximum aggregate encoded source bytes in one message.
    pub max_message_image_bytes: u64,
    /// Maximum source width multiplied by height.
    pub max_image_pixels: u64,
    /// Maximum source width or height.
    pub max_image_dimension: u64,
    /// Accepted source media types.
    pub media_types: Vec<String>,
    /// Maximum raw bytes in one `nano.attachment.chunk`.
    pub max_chunk_bytes: u64,
    /// Whether generic files are stored verbatim (`verbatim`) or not at all.
    pub file_admission: String,
}

/// Where the writable profile configuration lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSource {
    /// Absolute path to the profile patch file.
    pub path: String,
}

/// Where the default model credential is supplied, never its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSource {
    /// Credential reference (an environment-variable name).
    pub reference: String,
    /// Whether the reference currently resolves.
    pub configured: bool,
    /// Official source label (`file`, `env`, `project-env`, `user-env`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Additive source labels reported in `nano.hello`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// Writable profile configuration source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigSource>,
    /// Default credential source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<CredentialSource>,
}

/// Parsed `nano.hello` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    /// Wire protocol version.
    pub protocol: ProtocolVersion,
    /// Host runtime nonce identity.
    pub runtime_id: String,
    /// Host generation.
    pub generation: u64,
    /// Version facts.
    pub versions: Versions,
    /// Actual working directory.
    pub cwd: String,
    /// Root session identity.
    pub root_session_id: String,
    /// Root session facts.
    pub session: SessionFacts,
    /// Capability snapshot.
    pub capabilities: BTreeMap<String, Capability>,
    /// Capacity limits.
    pub limits: Limits,
    /// Optional deployment attachment limits (S06 minor addition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_limits: Option<AttachmentLimits>,
    /// Optional source labels for first-launch provenance (B5 addition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

impl Hello {
    /// Validate a handshake against the contract and this client's expectations.
    pub fn validate(
        &self,
        expected_runtime_id: &str,
        expected_generation: u64,
    ) -> Result<(), ProtocolError> {
        if self.protocol.major != PROTOCOL_MAJOR {
            return Err(ProtocolError::UnsupportedMajor(self.protocol.major));
        }
        if self.runtime_id.is_empty() {
            return Err(ProtocolError::InvalidFrame(
                "hello.runtimeId is empty".into(),
            ));
        }
        if self.runtime_id != expected_runtime_id {
            return Err(ProtocolError::RuntimeMismatch);
        }
        if self.generation == 0 {
            return Err(ProtocolError::InvalidFrame(
                "hello.generation must be positive".into(),
            ));
        }
        if self.generation != expected_generation {
            return Err(ProtocolError::GenerationMismatch);
        }
        for (field, value) in [
            ("hello.cwd", &self.cwd),
            ("hello.rootSessionId", &self.root_session_id),
            ("hello.versions.official", &self.versions.official),
            ("hello.versions.bridge", &self.versions.bridge),
            ("hello.versions.node", &self.versions.node),
        ] {
            if value.is_empty() {
                return Err(ProtocolError::InvalidFrame(format!("{field} is empty")));
            }
        }
        for (name, capability) in &self.capabilities {
            if !KNOWN_CAPABILITIES.contains(&name.as_str())
                && capability.required == Some(true)
                && capability.status == CapabilityStatus::Supported
            {
                return Err(ProtocolError::UnsupportedRequiredCapability(name.clone()));
            }
        }
        if self.limits.max_frame_bytes != MAX_FRAME_BYTES as u64 {
            return Err(ProtocolError::LimitMismatch("maxFrameBytes"));
        }
        if self.limits.max_pending_requests != MAX_PENDING_REQUESTS as u64 {
            return Err(ProtocolError::LimitMismatch("maxPendingRequests"));
        }
        if self.limits.max_attachment_chunk_bytes != MAX_ATTACHMENT_CHUNK_BYTES as u64 {
            return Err(ProtocolError::LimitMismatch("maxAttachmentChunkBytes"));
        }
        Ok(())
    }
}

/// Parsed `nano.startupError` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupErrorParams {
    /// Contract startup code.
    pub code: String,
    /// Holder pid for `occupied`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<u32>,
    /// Generation, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// Sanitized message.
    pub message: String,
    /// Repair path for ledger failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_path: Option<String>,
}

impl StartupErrorParams {
    /// Validate the startup code and message.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !STARTUP_ERROR_CODES.contains(&self.code.as_str()) {
            return Err(ProtocolError::UnknownStartupCode(self.code.clone()));
        }
        if self.message.is_empty() {
            return Err(ProtocolError::InvalidFrame(
                "startupError.message is empty".into(),
            ));
        }
        Ok(())
    }
}

/// Parsed `nano.event` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventParams {
    /// Runtime identity.
    pub runtime_id: String,
    /// Host generation.
    pub generation: u64,
    /// Subscription identity.
    pub subscription_id: String,
    /// Per-subscription envelope sequence.
    pub sequence: u64,
    /// Event kind.
    pub kind: String,
    /// Kind-specific payload.
    pub payload: Value,
}

impl EventParams {
    /// Validate identity, generation, kind, and payload shape.
    pub fn validate(
        &self,
        expected_runtime_id: &str,
        expected_generation: u64,
    ) -> Result<(), ProtocolError> {
        if self.runtime_id != expected_runtime_id {
            return Err(ProtocolError::RuntimeMismatch);
        }
        if self.generation != expected_generation {
            return Err(ProtocolError::GenerationMismatch);
        }
        if self.subscription_id.is_empty() {
            return Err(ProtocolError::InvalidFrame(
                "event.subscriptionId is empty".into(),
            ));
        }
        if !EVENT_KINDS.contains(&self.kind.as_str()) {
            return Err(ProtocolError::UnknownEventKind(self.kind.clone()));
        }
        if !self.payload.is_object() {
            return Err(ProtocolError::InvalidFrame(
                "event.payload must be an object".into(),
            ));
        }
        Ok(())
    }
}

/// Whether a subscription ended normally, was cancelled, or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamOutcome {
    /// The host completed the stream.
    Completed,
    /// The client unsubscribed.
    Cancelled,
    /// The host closed the stream; the client must recover from a baseline.
    Failed,
}

/// Parsed `nano.streamEnded` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEndedParams {
    /// Runtime identity.
    pub runtime_id: String,
    /// Host generation.
    pub generation: u64,
    /// Subscription identity.
    pub subscription_id: String,
    /// Terminal outcome.
    pub outcome: StreamOutcome,
    /// Optional reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl StreamEndedParams {
    /// Validate identity and generation.
    pub fn validate(
        &self,
        expected_runtime_id: &str,
        expected_generation: u64,
    ) -> Result<(), ProtocolError> {
        if self.runtime_id != expected_runtime_id {
            return Err(ProtocolError::RuntimeMismatch);
        }
        if self.generation != expected_generation {
            return Err(ProtocolError::GenerationMismatch);
        }
        if self.subscription_id.is_empty() {
            return Err(ProtocolError::InvalidFrame(
                "streamEnded.subscriptionId is empty".into(),
            ));
        }
        Ok(())
    }
}

/// Step status in a shutdown report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// The step completed.
    Stopped,
    /// The step failed.
    Failed,
    /// The host could not determine the outcome.
    Unknown,
}

/// One shutdown step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownStep {
    /// Step name.
    pub name: String,
    /// Step status.
    pub status: StepStatus,
    /// Optional detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Host-side shutdown report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownReport {
    /// Per-step outcomes.
    pub steps: Vec<ShutdownStep>,
    /// True only when every step reported `stopped`.
    pub clean: bool,
}

/// Address carried by a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    /// Root session identity.
    pub root_session_id: String,
    /// Optional child session identity, reserved for S12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
}

/// Result of `nano.subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResult {
    /// Subscription identity.
    pub subscription_id: String,
    /// Echoed address.
    pub address: Address,
    /// Durable seq at subscribe time.
    pub seq: u64,
    /// Optional session header baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Value>,
}

/// Result of `nano.cancelRequest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResult {
    /// Whether an in-flight request was cancelled.
    pub cancelled: bool,
}

/// Incoming frame from the host.
#[derive(Debug, Clone)]
pub enum IncomingFrame {
    /// A response to one of this client's requests.
    Response {
        /// Request id, or `None` for a parse-error report.
        id: Option<String>,
        /// Successful result.
        result: Option<Value>,
        /// Error body.
        error: Option<RpcErrorBody>,
    },
    /// A host notification.
    Notification {
        /// Method name.
        method: String,
        /// Raw params.
        params: Value,
    },
}

/// Fallback-transport nonce rules.
#[derive(Debug, Clone, Copy)]
pub struct FrameRules<'a> {
    /// Whether every frame must carry the nonce.
    pub require_nonce: bool,
    /// The per-generation nonce.
    pub nonce: &'a str,
}

/// Parse one incoming NDJSON line.
pub fn parse_incoming(line: &str, rules: FrameRules<'_>) -> Result<IncomingFrame, ProtocolError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| ProtocolError::InvalidFrame("frame must be a JSON object".into()))?;

    if rules.require_nonce {
        let nonce = object.get("nonce").and_then(Value::as_str);
        if nonce != Some(rules.nonce) {
            return Err(ProtocolError::NonceMismatch);
        }
    }
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ProtocolError::InvalidFrame(
            "jsonrpc must be \"2.0\"".into(),
        ));
    }

    if let Some(method) = object.get("method") {
        let method = method
            .as_str()
            .ok_or_else(|| ProtocolError::InvalidFrame("method must be a string".into()))?;
        if !method.starts_with("nano.") {
            return Err(ProtocolError::InvalidFrame(format!(
                "method is outside nano.*: {method}"
            )));
        }
        if object.contains_key("id") {
            return Err(ProtocolError::InvalidFrame(
                "host-to-client requests are not part of protocol v1".into(),
            ));
        }
        let params = object
            .get("params")
            .filter(|params| params.is_object())
            .cloned()
            .ok_or_else(|| {
                ProtocolError::InvalidFrame("notification params must be an object".into())
            })?;
        return Ok(IncomingFrame::Notification {
            method: method.to_string(),
            params,
        });
    }

    let id = match object.get("id") {
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(Value::Null) => None,
        Some(_) => {
            return Err(ProtocolError::InvalidFrame(
                "response id must be a non-empty string or null".into(),
            ))
        }
        None => {
            return Err(ProtocolError::InvalidFrame(
                "frame is neither a response nor a notification".into(),
            ))
        }
    };
    let result = object.get("result").cloned();
    let error = match object.get("error") {
        Some(error) => Some(
            serde_json::from_value::<RpcErrorBody>(error.clone())
                .map_err(|error| ProtocolError::MalformedResponse(error.to_string()))?,
        ),
        None => None,
    };
    if result.is_some() == error.is_some() {
        return Err(ProtocolError::MalformedResponse(
            "response must contain exactly one of result or error".into(),
        ));
    }
    Ok(IncomingFrame::Response { id, result, error })
}

/// Encode one outgoing request frame.
pub fn encode_request(
    id: &str,
    method: &str,
    params: Value,
    nonce: Option<&str>,
) -> Result<Vec<u8>, ProtocolError> {
    let mut frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    if let Some(nonce) = nonce {
        frame
            .as_object_mut()
            .expect("frame is an object")
            .insert("nonce".to_string(), Value::String(nonce.to_string()));
    }
    encode_value(&frame)
}

/// Encode one client-to-host notification frame.
pub fn encode_notification(
    method: &str,
    params: Value,
    nonce: Option<&str>,
) -> Result<Vec<u8>, ProtocolError> {
    let mut frame = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    if let Some(nonce) = nonce {
        frame
            .as_object_mut()
            .expect("frame is an object")
            .insert("nonce".to_string(), Value::String(nonce.to_string()));
    }
    encode_value(&frame)
}

/// Encode any frame value with the newline terminator and size limit.
pub fn encode_value(value: &Value) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| ProtocolError::InvalidFrame(error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Capability names this build understands.
pub const KNOWN_CAPABILITIES: [&str; 19] = [
    "agent",
    "sessionPersistence",
    "sessionQuery",
    "attachments",
    "fileReferences",
    "userQuestions",
    "approval",
    "jobs",
    "shell",
    "subagents",
    "schedule",
    "monitor",
    "terminal",
    "tool-ask-user",
    "networkListener",
    "modelSelection",
    "permissionPresets",
    "commands",
    "skills",
];

/// Convenience for tests and diagnostics.
pub fn protocol_version() -> ProtocolVersion {
    ProtocolVersion {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAX_ATTACHMENT_CHUNK_BYTES, MAX_FRAME_BYTES, MAX_PENDING_REQUESTS};
    use serde_json::json;

    fn hello() -> Hello {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(
            "agent".to_string(),
            Capability {
                status: CapabilityStatus::Supported,
                mode: None,
                detail: None,
                required: None,
            },
        );
        capabilities.insert(
            "tool-ask-user".to_string(),
            Capability {
                status: CapabilityStatus::Supported,
                mode: Some("blocking".into()),
                detail: None,
                required: None,
            },
        );
        Hello {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            runtime_id: "rt-1".into(),
            generation: 1,
            versions: Versions {
                official: "0.2.1-rc.2".into(),
                bridge: "0.1.0".into(),
                node: "v24.18.0".into(),
            },
            cwd: "/work".into(),
            root_session_id: "session-a".into(),
            session: SessionFacts {
                mode: SessionFactMode::Created,
                seq: 3,
                header: None,
            },
            capabilities,
            limits: Limits {
                max_frame_bytes: MAX_FRAME_BYTES as u64,
                max_pending_requests: MAX_PENDING_REQUESTS as u64,
                max_attachment_chunk_bytes: MAX_ATTACHMENT_CHUNK_BYTES as u64,
            },
            attachment_limits: None,
            provenance: None,
        }
    }

    #[test]
    fn accepts_a_valid_hello() {
        hello().validate("rt-1", 1).unwrap();
    }

    #[test]
    fn a_hello_may_carry_source_labels_without_values() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value["provenance"] = json!({
            "config": {"path": "/home/u/.dsh/profiles/dsh-tui-nano/cordis.patch.yml"},
            "credentials": {
                "reference": "DEEPSEEK_API_KEY",
                "configured": true,
                "source": "env"
            }
        });
        let parsed: Hello = serde_json::from_value(value).unwrap();
        let provenance = parsed.provenance.expect("provenance");
        assert_eq!(
            provenance.config.expect("config").path,
            "/home/u/.dsh/profiles/dsh-tui-nano/cordis.patch.yml"
        );
        let credentials = provenance.credentials.expect("credentials");
        assert_eq!(credentials.reference, "DEEPSEEK_API_KEY");
        assert!(credentials.configured);
        assert_eq!(credentials.source.as_deref(), Some("env"));
        assert!(!serde_json::to_string(&credentials).unwrap().contains("sk-"));
    }

    #[test]
    fn an_absent_provenance_is_optional() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value.as_object_mut().unwrap().remove("provenance");
        let parsed: Hello = serde_json::from_value(value).unwrap();
        assert!(parsed.provenance.is_none());
    }

    #[test]
    fn rejects_wrong_major_and_generation() {
        let mut wrong_major = hello();
        wrong_major.protocol.major = 2;
        assert!(matches!(
            wrong_major.validate("rt-1", 1),
            Err(ProtocolError::UnsupportedMajor(2))
        ));
        let mut wrong_generation = hello();
        wrong_generation.generation = 2;
        assert!(matches!(
            wrong_generation.validate("rt-1", 1),
            Err(ProtocolError::GenerationMismatch)
        ));
        assert!(matches!(
            hello().validate("rt-other", 1),
            Err(ProtocolError::RuntimeMismatch)
        ));
    }

    #[test]
    fn rejects_unknown_required_capabilities_and_bad_limits() {
        let mut value = hello();
        value.capabilities.insert(
            "future-required".into(),
            Capability {
                status: CapabilityStatus::Supported,
                mode: None,
                detail: None,
                required: Some(true),
            },
        );
        assert!(matches!(
            value.validate("rt-1", 1),
            Err(ProtocolError::UnsupportedRequiredCapability(_))
        ));

        let mut value = hello();
        value.limits.max_pending_requests = 1;
        assert!(matches!(
            value.validate("rt-1", 1),
            Err(ProtocolError::LimitMismatch("maxPendingRequests"))
        ));
    }

    #[test]
    fn event_rejects_unknown_kind_and_wrong_generation() {
        let event = EventParams {
            runtime_id: "rt-1".into(),
            generation: 1,
            subscription_id: "sub-1".into(),
            sequence: 0,
            kind: "session.event".into(),
            payload: json!({}),
        };
        event.validate("rt-1", 1).unwrap();
        let mut wrong = event.clone();
        wrong.generation = 2;
        assert!(matches!(
            wrong.validate("rt-1", 1),
            Err(ProtocolError::GenerationMismatch)
        ));
        let mut unknown = event;
        unknown.kind = "future.event".into();
        assert!(matches!(
            unknown.validate("rt-1", 1),
            Err(ProtocolError::UnknownEventKind(_))
        ));
    }

    #[test]
    fn parses_responses_and_notifications() {
        let rules = FrameRules {
            require_nonce: false,
            nonce: "",
        };
        let notification = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"nano.hello","params":{}}"#,
            rules,
        )
        .unwrap();
        assert!(matches!(notification, IncomingFrame::Notification { .. }));
        let response = parse_incoming(
            r#"{"jsonrpc":"2.0","id":"req-1","result":{"ok":true}}"#,
            rules,
        )
        .unwrap();
        assert!(matches!(response, IncomingFrame::Response { .. }));
        assert!(parse_incoming(
            r#"{"jsonrpc":"2.0","id":"req-1","method":"nano.x","params":{}}"#,
            rules
        )
        .is_err());
        assert!(parse_incoming(
            r#"{"jsonrpc":"2.0","id":"req-1","result":{},"error":{"code":1,"message":"x"}}"#,
            rules
        )
        .is_err());
    }

    #[test]
    fn fallback_nonce_is_enforced() {
        let line = r#"{"jsonrpc":"2.0","method":"nano.hello","params":{}}"#;
        let rules = FrameRules {
            require_nonce: true,
            nonce: "n1",
        };
        assert!(matches!(
            parse_incoming(line, rules),
            Err(ProtocolError::NonceMismatch)
        ));
        let with_nonce = r#"{"jsonrpc":"2.0","nonce":"n1","method":"nano.hello","params":{}}"#;
        assert!(parse_incoming(with_nonce, rules).is_ok());
    }

    #[test]
    fn oversized_encoded_frame_is_refused() {
        let huge = "x".repeat(MAX_FRAME_BYTES + 1);
        let value = json!({ "payload": huge });
        assert!(matches!(
            encode_value(&value),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }
}
