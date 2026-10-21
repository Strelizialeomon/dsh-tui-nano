//! Manual-compaction outcome.
//!
//! `/compact` is an official command, so every expected failure is an official
//! stable sentence. Nano passes that sentence through verbatim and never retries
//! on its own; a cancelled wait is the official `Compaction cancelled.` text and
//! the history may already have changed.

use serde_json::Value;

/// The official text for a cancelled compaction wait.
pub const COMPACTION_CANCELLED: &str = "Compaction cancelled.";

/// Whether the command settled successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionKind {
    /// The command produced its settled success text.
    Success,
    /// The command produced an official failure text.
    Error,
}

/// One settled `/compact` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// Official command lifecycle identity, when the official registry returned
    /// one. A cancelled wait is settled by the host after the registry's abort
    /// races the handler, so the id may be unavailable.
    pub command_id: Option<String>,
    /// Success or official failure.
    pub kind: CompactionKind,
    /// Official text to show verbatim, when present.
    pub text: Option<String>,
    /// Durable `compaction/summary` seq on success.
    pub source_event_seq: Option<u64>,
}

impl CompactionOutcome {
    /// Parse one `sessionsCompact` value.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "compact result must be an object".to_string())?;
        let command_id = match object.get("commandId") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            Some(_) => {
                return Err("compact.commandId must be a non-empty string when present".to_string())
            }
        };
        let kind = match object.get("kind").and_then(Value::as_str) {
            Some("success") => CompactionKind::Success,
            Some("error") => CompactionKind::Error,
            Some(other) => return Err(format!("unsupported compact kind {other}")),
            None => return Err("compact.kind is missing".to_string()),
        };
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string);
        let source_event_seq = object.get("sourceEventSeq").and_then(Value::as_u64);
        Ok(Self {
            command_id,
            kind,
            text,
            source_event_seq,
        })
    }

    /// Settled outcome for a cancelled wait. The official registry rejects an
    /// aborted command before its handler can return, so the host preserves the
    /// official stable text itself.
    pub fn cancelled_wait() -> Self {
        Self {
            command_id: None,
            kind: CompactionKind::Error,
            text: Some(COMPACTION_CANCELLED.to_string()),
            source_event_seq: None,
        }
    }

    /// Text for the user; official text wins over any local wording.
    pub fn display_text(&self) -> String {
        self.text.clone().unwrap_or_else(|| match self.kind {
            CompactionKind::Success => "压缩已完成（官方未提供文本）".to_string(),
            CompactionKind::Error => "压缩失败（官方未提供文本）".to_string(),
        })
    }

    /// Whether the user cancelled the wait.
    pub fn cancelled(&self) -> bool {
        self.kind == CompactionKind::Error && self.text.as_deref() == Some(COMPACTION_CANCELLED)
    }

    /// Whether the history may already have changed and must be reread.
    pub fn history_may_have_changed(&self) -> bool {
        self.kind == CompactionKind::Success
            || self.cancelled()
            || self
                .text
                .as_deref()
                .is_some_and(|text| text.contains("some session history may have changed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn official_success_text_is_passed_through() {
        let outcome = CompactionOutcome::parse(&json!({
            "commandId": "cmd-1",
            "kind": "success",
            "text": "Compacted 12 history items (~3456 tokens).",
            "sourceEventSeq": 42
        }))
        .unwrap();
        assert_eq!(outcome.kind, CompactionKind::Success);
        assert_eq!(outcome.command_id.as_deref(), Some("cmd-1"));
        assert_eq!(
            outcome.display_text(),
            "Compacted 12 history items (~3456 tokens)."
        );
        assert_eq!(outcome.source_event_seq, Some(42));
        assert!(outcome.history_may_have_changed());
    }

    #[test]
    fn official_failure_text_is_passed_through_without_retry_flags() {
        let outcome = CompactionOutcome::parse(&json!({
            "commandId": "cmd-2",
            "kind": "error",
            "text": "Compaction is unavailable because this process has an active compaction, or the agent is not idle."
        }))
        .unwrap();
        assert_eq!(outcome.kind, CompactionKind::Error);
        assert!(!outcome.cancelled());
        assert!(!outcome.history_may_have_changed());
    }

    #[test]
    fn cancellation_is_a_stable_official_text() {
        let outcome = CompactionOutcome::parse(&json!({
            "commandId": "cmd-3",
            "kind": "error",
            "text": COMPACTION_CANCELLED
        }))
        .unwrap();
        assert!(outcome.cancelled());
        assert!(outcome.history_may_have_changed());
    }

    #[test]
    fn an_aborted_wait_is_settled_without_a_command_id() {
        let outcome = CompactionOutcome::parse(&json!({
            "kind": "error",
            "text": COMPACTION_CANCELLED
        }))
        .unwrap();
        assert_eq!(outcome.command_id, None);
        assert!(outcome.cancelled());
        assert!(outcome.history_may_have_changed());
        assert_eq!(
            CompactionOutcome::cancelled_wait().display_text(),
            COMPACTION_CANCELLED
        );
    }

    #[test]
    fn malformed_results_are_rejected() {
        assert!(CompactionOutcome::parse(&json!({})).is_err());
        assert!(CompactionOutcome::parse(&json!({"commandId": "c", "kind": "future"})).is_err());
    }
}
