//! Wire payloads for the conversation event kinds.
//!
//! `session.event` carries the official durable event verbatim; the other kinds
//! are S02-owned live projections. Unknown stream frames and unknown durable
//! event types are ignored, never guessed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::address::SessionAddress;
use crate::queue::InboxView;

/// Error while reading one wire payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

/// Address as carried by the protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAddress {
    /// Root session identity.
    pub root_session_id: String,
    /// Optional child session identity (S12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    /// Optional direct parent identity (S12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Optional subagent mode (S12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl WireAddress {
    /// Whether this wire address belongs to the given conversation.
    pub fn matches(&self, address: &SessionAddress) -> bool {
        if self.root_session_id != address.root_session_id {
            return false;
        }
        match (&self.child_session_id, &address.child) {
            (None, None) => true,
            (Some(child), Some(value)) => {
                child == &value.child_session_id
                    && self
                        .parent_session_id
                        .as_deref()
                        .is_none_or(|parent| parent == value.parent_session_id)
                    && self
                        .mode
                        .as_deref()
                        .is_none_or(|mode| mode == value.mode.as_str())
            }
            _ => false,
        }
    }
}

/// Agent lifecycle state mirrored on every transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationStatus {
    /// No driver is active.
    Idle,
    /// A turn is running or waiting behind one.
    Running,
}

impl ConversationStatus {
    /// Short user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "空闲",
            Self::Running => "运行中",
        }
    }
}

/// One durable official session event as forwarded by S01.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    /// Address the event belongs to.
    pub address: WireAddress,
    /// Durable sequence number.
    pub seq: u64,
    /// Official event type, such as `user/message`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Official event data, verbatim.
    pub data: Value,
}

/// One live assistant-stream frame.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamFrame {
    /// Start of one attempt and its dense chunk sequence.
    Start {
        /// Attempt identity.
        attempt_id: String,
        /// Revision within the attached Agent lifecycle.
        revision: u64,
        /// Owning turn.
        turn: u64,
        /// Owning step.
        step: u64,
    },
    /// One dense chunk.
    Chunk {
        /// Attempt identity.
        attempt_id: String,
        /// Revision.
        revision: u64,
        /// Dense zero-based chunk position.
        index: u64,
        /// Official chunk time.
        time: Option<u64>,
        /// Raw official stream chunk.
        chunk: Value,
    },
    /// End of one attempt.
    End {
        /// Attempt identity.
        attempt_id: String,
        /// Revision.
        revision: u64,
        /// Number of chunk frames emitted.
        index: u64,
        /// Official settlement, when reported.
        outcome: Value,
    },
}

/// One S02-owned live event.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationEvent {
    /// Absolute Agent status.
    Status {
        /// Address.
        address: WireAddress,
        /// Current status.
        status: ConversationStatus,
    },
    /// Absolute inbox projection.
    Inbox {
        /// Address.
        address: WireAddress,
        /// Full or bounded pending input.
        inbox: InboxView,
    },
    /// One live assistant-stream frame.
    Stream {
        /// Address.
        address: WireAddress,
        /// Frame.
        frame: StreamFrame,
    },
    /// A kind or frame this build does not understand; the caller ignores it.
    Ignored,
}

/// Parse `nano.event` params into the kind-specific value.
pub fn parse_conversation_event(
    kind: &str,
    payload: &Value,
) -> Result<ConversationEvent, WireError> {
    let address = read_address(payload)?;
    match kind {
        "conversation.status" => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| WireError("conversation.status.status must be a string".into()))?;
            let status = match status {
                "idle" => ConversationStatus::Idle,
                "running" => ConversationStatus::Running,
                other => return Err(WireError(format!("unknown conversation status {other}"))),
            };
            Ok(ConversationEvent::Status { address, status })
        }
        "conversation.inbox" => {
            let inbox = payload
                .get("inbox")
                .ok_or_else(|| WireError("conversation.inbox.inbox is missing".into()))?;
            Ok(ConversationEvent::Inbox {
                address,
                inbox: InboxView::from_event(
                    inbox,
                    payload
                        .get("truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
            })
        }
        "conversation.stream" => {
            let frame = payload
                .get("frame")
                .ok_or_else(|| WireError("conversation.stream.frame is missing".into()))?;
            match parse_stream_frame(frame) {
                Some(frame) => Ok(ConversationEvent::Stream { address, frame }),
                None => Ok(ConversationEvent::Ignored),
            }
        }
        _ => Ok(ConversationEvent::Ignored),
    }
}

/// Parse one `session.event` payload.
pub fn parse_session_event(payload: &Value) -> Result<SessionEvent, WireError> {
    serde_json::from_value(payload.clone())
        .map_err(|error| WireError(format!("session.event: {error}")))
}

/// Parse one assistant-stream frame; unknown frame types return `None`.
pub fn parse_stream_frame(value: &Value) -> Option<StreamFrame> {
    let attempt_id = value.get("attemptId")?.as_str()?.to_string();
    let revision = value.get("revision")?.as_u64()?;
    match value.get("type")?.as_str()? {
        "start" => Some(StreamFrame::Start {
            attempt_id,
            revision,
            turn: value.get("turn")?.as_u64()?,
            step: value.get("step")?.as_u64()?,
        }),
        "chunk" => Some(StreamFrame::Chunk {
            attempt_id,
            revision,
            index: value.get("index")?.as_u64()?,
            time: value.get("time").and_then(Value::as_u64),
            chunk: value.get("chunk")?.clone(),
        }),
        "end" => Some(StreamFrame::End {
            attempt_id,
            revision,
            index: value.get("index")?.as_u64()?,
            outcome: value.get("outcome").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    }
}

fn read_address(payload: &Value) -> Result<WireAddress, WireError> {
    let address = payload
        .get("address")
        .ok_or_else(|| WireError("event payload has no address".into()))?;
    serde_json::from_value(address.clone())
        .map_err(|error| WireError(format!("event.address: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_status_and_inbox_events() {
        let status = parse_conversation_event(
            "conversation.status",
            &json!({"address": {"rootSessionId": "root"}, "status": "running"}),
        )
        .unwrap();
        assert!(matches!(
            status,
            ConversationEvent::Status {
                status: ConversationStatus::Running,
                ..
            }
        ));

        let inbox = parse_conversation_event(
            "conversation.inbox",
            &json!({
                "address": {"rootSessionId": "root"},
                "inbox": {"nextTurn": [], "nextStep": []}
            }),
        )
        .unwrap();
        assert!(matches!(inbox, ConversationEvent::Inbox { .. }));
    }

    #[test]
    fn unknown_frames_and_kinds_are_ignored_not_guessed() {
        let unknown_kind = parse_conversation_event(
            "future.kind",
            &json!({"address": {"rootSessionId": "root"}}),
        )
        .unwrap();
        assert_eq!(unknown_kind, ConversationEvent::Ignored);
        let unknown_frame = parse_conversation_event(
            "conversation.stream",
            &json!({
                "address": {"rootSessionId": "root"},
                "frame": {"type": "future", "attemptId": "a", "revision": 1}
            }),
        )
        .unwrap();
        assert_eq!(unknown_frame, ConversationEvent::Ignored);
    }

    #[test]
    fn address_match_requires_the_exact_conversation() {
        let root = SessionAddress::root("root");
        let wire = WireAddress {
            root_session_id: "root".into(),
            child_session_id: None,
            parent_session_id: None,
            mode: None,
        };
        assert!(wire.matches(&root));
        let child =
            SessionAddress::child("root", "root", "child", crate::SubagentMode::Continuable);
        assert!(!wire.matches(&child));
    }
}
