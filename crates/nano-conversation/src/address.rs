//! Session addresses. A root conversation is identified by the official
//! `rootSessionId`; a subagent conversation by the official direct parent plus
//! the child session and its mode. Addresses are opaque identities: they are
//! never derived from a path, title, or window title.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Whether a child conversation can accept more turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentMode {
    /// A single historical child; read-only.
    OneShot,
    /// A durable child that can be resumed.
    Continuable,
    /// The official catalog did not prove the mode.
    Unknown,
}

impl SubagentMode {
    /// Stable spelling used in draft keys and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Continuable => "continuable",
            Self::Unknown => "unknown",
        }
    }
}

/// Official direct-parent identity for one child conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildAddress {
    /// The child's direct parent session.
    pub parent_session_id: String,
    /// The child session identity.
    pub child_session_id: String,
    /// Whether the child can be resumed.
    pub mode: SubagentMode,
}

/// The address that owns a transcript, a draft, a queue, and a focus stack.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAddress {
    /// The root session of the conversation tree.
    pub root_session_id: String,
    /// Present only for a subagent conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<ChildAddress>,
}

impl SessionAddress {
    /// Build an ordinary root-conversation address.
    pub fn root(root_session_id: impl Into<String>) -> Self {
        Self {
            root_session_id: root_session_id.into(),
            child: None,
        }
    }

    /// Build a subagent address from official parent/child facts.
    pub fn child(
        root_session_id: impl Into<String>,
        parent_session_id: impl Into<String>,
        child_session_id: impl Into<String>,
        mode: SubagentMode,
    ) -> Self {
        Self {
            root_session_id: root_session_id.into(),
            child: Some(ChildAddress {
                parent_session_id: parent_session_id.into(),
                child_session_id: child_session_id.into(),
                mode,
            }),
        }
    }

    /// Whether this is the root conversation.
    pub fn is_root(&self) -> bool {
        self.child.is_none()
    }

    /// Stable key for per-address drafts and caches.
    pub fn key(&self) -> String {
        match &self.child {
            None => format!("root:{}", self.root_session_id),
            Some(child) => format!(
                "child:{}:{}:{}:{}",
                self.root_session_id,
                child.parent_session_id,
                child.child_session_id,
                child.mode.as_str()
            ),
        }
    }

    /// Short user-facing label; never used as identity.
    pub fn label(&self) -> String {
        match &self.child {
            None => "main".to_string(),
            Some(child) => child.child_session_id.clone(),
        }
    }

    /// The `nano.*` address object for this conversation. Child addresses are
    /// reserved for S12; S02 only submits root addresses.
    pub fn wire_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({ "rootSessionId": self.root_session_id });
        if let Some(child) = &self.child {
            value["parentSessionId"] = serde_json::Value::String(child.parent_session_id.clone());
            value["childSessionId"] = serde_json::Value::String(child.child_session_id.clone());
            value["mode"] = serde_json::Value::String(child.mode.as_str().to_string());
        }
        value
    }
}

impl fmt::Display for SessionAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.key())
    }
}
