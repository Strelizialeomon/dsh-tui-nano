//! Official session-list rows.
//!
//! Every field comes from the official session query/projection. A fact the
//! official source does not provide (a cold session's last activity, for
//! example) stays `None`; the UI shows an explicit placeholder instead of
//! inventing a value. A row never claims "resumable": opening is decided by the
//! official persistence check at startup.

use serde_json::Value;

/// Durable format classification reported by official persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStatus {
    /// The current on-disk format.
    Current,
    /// A migration is required before the session can be used.
    MigrationRequired,
}

impl FormatStatus {
    /// Stable wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::MigrationRequired => "migration-required",
        }
    }
}

/// One official session-list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Opaque official session identity.
    pub session_id: String,
    /// Official title, when the log or projection cache has one.
    pub title: Option<String>,
    /// Durable creation time in Unix epoch milliseconds.
    pub created_at: u64,
    /// Last human prompt time, known only for the live root session.
    pub updated_at: Option<u64>,
    /// Whether a live Agent currently owns the session in this host.
    pub agent_available: bool,
    /// Whether that live Agent is running.
    pub running: bool,
    /// Whether no turn has started, when the official source states it.
    pub blank: Option<bool>,
    /// Recorded working directory.
    pub cwd: Option<String>,
    /// Format classification, when reported.
    pub format_status: Option<FormatStatus>,
    /// Coarse origin classification, such as `subagent`.
    pub origin: Option<String>,
    /// Direct parent for a subagent-origin session.
    pub parent_session_id: Option<String>,
    /// Whether this row is the root session this host drives.
    pub current: bool,
}

impl SessionSummary {
    /// Parse the `nano.sessions.list` result into rows.
    /// @param value - wire result object.
    /// @returns rows in wire order.
    pub fn parse_result(value: &Value) -> Result<Vec<Self>, String> {
        let sessions = value
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| "sessions.list.sessions must be an array".to_string())?;
        sessions.iter().map(Self::parse).collect()
    }

    /// Parse one row. Unknown optional fields are ignored for minor additions.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "session row must be an object".to_string())?;
        let session_id = required_string(object, "sessionId")?;
        let created_at = required_u64(object, "createdAt")?;
        let agent_available = required_bool(object, "agentAvailable")?;
        let running = required_bool(object, "running")?;
        let current = required_bool(object, "current")?;
        let format_status = match object.get("formatStatus") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value == "current" => Some(FormatStatus::Current),
            Some(Value::String(value)) if value == "migration-required" => {
                Some(FormatStatus::MigrationRequired)
            }
            Some(other) => {
                return Err(format!("unsupported formatStatus {other}"));
            }
        };
        Ok(Self {
            session_id,
            title: optional_string(object, "title"),
            created_at,
            updated_at: optional_u64(object, "updatedAt"),
            agent_available,
            running,
            blank: optional_bool(object, "blank"),
            cwd: optional_string(object, "cwd"),
            format_status,
            origin: optional_string(object, "origin"),
            parent_session_id: optional_string(object, "parentSessionId"),
            current,
        })
    }

    /// Title for display; the placeholder is display-only and never persisted.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("未命名")
    }

    /// Whether this row may become the main session. Subagent children belong
    /// to S12's navigation and cannot be opened as a root.
    pub fn switchable(&self) -> bool {
        self.origin.as_deref() != Some("subagent")
    }

    /// Sort key: last activity when known, otherwise creation time.
    pub fn activity_at(&self) -> u64 {
        self.updated_at.unwrap_or(self.created_at)
    }

    /// One-line state label built only from official facts.
    pub fn state_label(&self) -> &'static str {
        if self.current {
            "当前"
        } else if self.running {
            "运行中"
        } else if self.agent_available {
            "已打开"
        } else if self.blank == Some(true) {
            "空白"
        } else {
            ""
        }
    }
}

fn required_string(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(format!("session.{key} must be a non-empty string")),
    }
}

fn required_u64(object: &serde_json::Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("session.{key} must be a non-negative integer"))
}

fn required_bool(object: &serde_json::Map<String, Value>, key: &str) -> Result<bool, String> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("session.{key} must be a boolean"))
}

fn optional_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(object: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

fn optional_bool(object: &serde_json::Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_known_and_unknown_facts_without_guessing() {
        let rows = SessionSummary::parse_result(&json!({
            "sessions": [
                {
                    "sessionId": "s-1",
                    "title": "会话一",
                    "createdAt": 100,
                    "updatedAt": 200,
                    "agentAvailable": true,
                    "running": false,
                    "blank": false,
                    "cwd": "/work",
                    "formatStatus": "current",
                    "current": true,
                    "futureOptional": {"ignored": true}
                },
                {
                    "sessionId": "s-2",
                    "title": null,
                    "createdAt": 50,
                    "agentAvailable": false,
                    "running": false,
                    "current": false
                }
            ]
        }))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_title(), "会话一");
        assert_eq!(rows[0].activity_at(), 200);
        assert_eq!(rows[0].state_label(), "当前");
        assert_eq!(rows[1].display_title(), "未命名");
        assert_eq!(rows[1].updated_at, None);
        assert_eq!(rows[1].blank, None);
        assert_eq!(rows[1].activity_at(), 50);
    }

    #[test]
    fn subagent_children_are_not_switchable_as_roots() {
        let row = SessionSummary::parse(&json!({
            "sessionId": "s-child",
            "createdAt": 1,
            "agentAvailable": false,
            "running": false,
            "origin": "subagent",
            "parentSessionId": "s-root",
            "current": false
        }))
        .unwrap();
        assert!(!row.switchable());
        assert_eq!(row.parent_session_id.as_deref(), Some("s-root"));
    }

    #[test]
    fn malformed_rows_are_rejected_instead_of_defaulted() {
        let error = SessionSummary::parse(&json!({
            "sessionId": "",
            "createdAt": 1,
            "agentAvailable": false,
            "running": false,
            "current": false
        }))
        .unwrap_err();
        assert!(error.contains("sessionId"));
        assert!(SessionSummary::parse_result(&json!({})).is_err());
    }
}
