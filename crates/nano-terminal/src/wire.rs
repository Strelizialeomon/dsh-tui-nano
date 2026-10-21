//! Wire types for the S09 terminal notifications and responses.
//!
//! These mirror `protocol/nano-protocol.json`; malformed shapes are rejected
//! here instead of being guessed into a partial screen.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One verified shell profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalShell {
    /// Absolute executable path.
    pub path: String,
    /// Arguments passed to the interactive shell.
    #[serde(default)]
    pub args: Vec<String>,
    /// Display name.
    pub name: String,
}

/// Host-owned process state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    /// The shell process is alive.
    Running,
    /// The process ended normally.
    Exited,
    /// The provider failed to start or supervise the process.
    Failed,
}

impl ProcessState {
    /// Whether input may be accepted.
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Host-owned terminal metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    /// Host terminal identity.
    pub id: String,
    /// Display title.
    #[serde(default)]
    pub title: String,
    /// Shell profile.
    pub shell: TerminalShell,
    /// Initial working directory; shell `cd` does not change it.
    #[serde(default)]
    pub cwd: String,
    /// Current columns.
    pub cols: u16,
    /// Current rows.
    pub rows: u16,
    /// Process state.
    pub state: ProcessState,
    /// Exit code, when known.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Provider error, when the process failed.
    #[serde(default)]
    pub error: Option<String>,
}

/// Session environment and terminal limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEnvironment {
    /// Logged working directory.
    pub cwd: String,
    /// Maximum raw UTF-8 bytes per input command.
    pub max_input_bytes: usize,
    /// Maximum unacknowledged encoded input bytes.
    pub max_buffered_bytes: usize,
    /// Maximum columns.
    pub max_cols: u16,
    /// Maximum rows.
    pub max_rows: u16,
    /// Retained screen history rows.
    pub scrollback: usize,
}

/// Result of one attach; the screen is the bounded opening snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachment {
    /// Attachment identity.
    pub attachment_id: String,
    /// Terminal identity.
    pub terminal_id: String,
    /// Last output sequence included in the snapshot.
    pub sequence: u64,
    /// Whether this attachment owns input and resize control.
    pub writable: bool,
    /// Host metadata at attach time.
    pub info: TerminalInfo,
    /// Serialized bounded screen (ANSI), including scrollback limits.
    pub snapshot: String,
}

/// Why an attachment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndReason {
    /// The client detached.
    Detached,
    /// The process ended and its final output was delivered.
    Drained,
    /// The provider or transport failed.
    Failed,
    /// The output follower could not keep up.
    SlowConsumer,
    /// The terminal was closed.
    Closed,
    /// Input validation failed; the process survives.
    InputFailed,
}

/// One live host-to-client terminal notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// Ordered output bytes (UTF-8).
    Output {
        /// Attachment identity.
        attachment_id: String,
        /// Terminal identity.
        terminal_id: String,
        /// Monotonic output sequence.
        sequence: u64,
        /// Output bytes as UTF-8.
        data: String,
    },
    /// Writable/state update.
    State {
        /// Attachment identity.
        attachment_id: String,
        /// Terminal identity.
        terminal_id: String,
        /// Whether this attachment still owns input.
        writable: bool,
        /// Current metadata.
        info: TerminalInfo,
    },
    /// Provider acceptance of one input/resize command.
    Ack {
        /// Attachment identity.
        attachment_id: String,
        /// Acknowledged attachment-local sequence.
        sequence: u64,
        /// Why the command was refused, if it was.
        refusal: Option<Refusal>,
    },
    /// The attachment ended; the process may still be alive.
    Ended {
        /// Attachment identity.
        attachment_id: String,
        /// Terminal identity.
        terminal_id: String,
        /// Why the attachment ended.
        reason: EndReason,
        /// Sanitized detail.
        detail: Option<String>,
    },
}

/// Why an input command was refused without ending the attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// Another attachment owns input.
    ReadOnly,
    /// The process is no longer running.
    NotRunning,
}

/// Parse `nano.terminal.environment` result.
pub fn parse_environment(value: &Value) -> Result<TerminalEnvironment, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid terminal environment: {error}"))
}

/// Parse `nano.terminal.shells` result.
pub fn parse_shells(value: &Value) -> Result<Vec<TerminalShell>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Shells {
        shells: Vec<TerminalShell>,
    }
    let shells: Shells = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid shell list: {error}"))?;
    Ok(shells.shells)
}

/// Parse `nano.terminal.list` result.
pub fn parse_list(value: &Value) -> Result<Vec<TerminalInfo>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct List {
        terminals: Vec<TerminalInfo>,
    }
    let list: List = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid terminal list: {error}"))?;
    Ok(list.terminals)
}

/// Parse `nano.terminal.create` result.
pub fn parse_create(value: &Value) -> Result<TerminalInfo, String> {
    #[derive(Deserialize)]
    struct Created {
        terminal: TerminalInfo,
    }
    let created: Created = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid create result: {error}"))?;
    Ok(created.terminal)
}

/// Parse `nano.terminal.attach` result.
pub fn parse_attachment(value: &Value) -> Result<TerminalAttachment, String> {
    serde_json::from_value(value.clone()).map_err(|error| format!("invalid attach result: {error}"))
}

/// Parse `nano.terminal.retain` result.
pub fn parse_hold(value: &Value) -> Result<String, String> {
    value
        .get("holdId")
        .and_then(Value::as_str)
        .filter(|hold| !hold.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "retain result has no holdId".to_string())
}

/// Parse one live terminal notification.
pub fn parse_notification(method: &str, params: &Value) -> Result<Notification, String> {
    match method {
        "nano.terminal.output" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Output {
                attachment_id: String,
                terminal_id: String,
                sequence: u64,
                data: String,
            }
            let output: Output = serde_json::from_value(params.clone())
                .map_err(|error| format!("invalid terminal output: {error}"))?;
            Ok(Notification::Output {
                attachment_id: output.attachment_id,
                terminal_id: output.terminal_id,
                sequence: output.sequence,
                data: output.data,
            })
        }
        "nano.terminal.state" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct State {
                attachment_id: String,
                terminal_id: String,
                writable: bool,
                info: TerminalInfo,
            }
            let state: State = serde_json::from_value(params.clone())
                .map_err(|error| format!("invalid terminal state: {error}"))?;
            Ok(Notification::State {
                attachment_id: state.attachment_id,
                terminal_id: state.terminal_id,
                writable: state.writable,
                info: state.info,
            })
        }
        "nano.terminal.ack" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Ack {
                attachment_id: String,
                sequence: u64,
                #[serde(default)]
                refusal: Option<Refusal>,
            }
            let ack: Ack = serde_json::from_value(params.clone())
                .map_err(|error| format!("invalid terminal ack: {error}"))?;
            Ok(Notification::Ack {
                attachment_id: ack.attachment_id,
                sequence: ack.sequence,
                refusal: ack.refusal,
            })
        }
        "nano.terminal.ended" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Ended {
                attachment_id: String,
                terminal_id: String,
                reason: EndReason,
                #[serde(default)]
                detail: Option<String>,
            }
            let ended: Ended = serde_json::from_value(params.clone())
                .map_err(|error| format!("invalid terminal ended: {error}"))?;
            Ok(Notification::Ended {
                attachment_id: ended.attachment_id,
                terminal_id: ended.terminal_id,
                reason: ended.reason,
                detail: ended.detail,
            })
        }
        other => Err(format!("unknown terminal notification {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_environment_and_shells() {
        let environment = parse_environment(&json!({
            "cwd": "/work",
            "maxInputBytes": 65536,
            "maxBufferedBytes": 2097152,
            "maxCols": 500,
            "maxRows": 200,
            "scrollback": 1000
        }))
        .unwrap();
        assert_eq!(environment.max_cols, 500);
        let shells = parse_shells(&json!({
            "shells": [{ "path": "/bin/zsh", "args": ["-i"], "name": "zsh" }]
        }))
        .unwrap();
        assert_eq!(shells[0].name, "zsh");
    }

    #[test]
    fn parses_output_state_ack_and_ended() {
        let info = json!({
            "id": "term-1",
            "title": "zsh",
            "shell": { "path": "/bin/zsh", "args": ["-i"], "name": "zsh" },
            "cwd": "/work",
            "cols": 80,
            "rows": 24,
            "state": "running",
            "exitCode": null
        });
        let output = parse_notification(
            "nano.terminal.output",
            &json!({ "attachmentId": "att-1", "terminalId": "term-1", "sequence": 3, "data": "hi" }),
        )
        .unwrap();
        assert!(matches!(output, Notification::Output { sequence: 3, .. }));
        let state = parse_notification(
            "nano.terminal.state",
            &json!({ "attachmentId": "att-1", "terminalId": "term-1", "writable": false, "info": info }),
        )
        .unwrap();
        assert!(matches!(
            state,
            Notification::State {
                writable: false,
                ..
            }
        ));
        let ack = parse_notification(
            "nano.terminal.ack",
            &json!({ "attachmentId": "att-1", "sequence": 2, "refusal": "read-only" }),
        )
        .unwrap();
        assert!(matches!(
            ack,
            Notification::Ack {
                refusal: Some(Refusal::ReadOnly),
                ..
            }
        ));
        let ended = parse_notification(
            "nano.terminal.ended",
            &json!({
                "attachmentId": "att-1",
                "terminalId": "term-1",
                "reason": "drained"
            }),
        )
        .unwrap();
        assert!(matches!(
            ended,
            Notification::Ended {
                reason: EndReason::Drained,
                ..
            }
        ));
    }

    #[test]
    fn rejects_malformed_notifications() {
        assert!(parse_notification("nano.terminal.output", &json!({ "data": 1 })).is_err());
        assert!(parse_notification("nano.terminal.future", &json!({})).is_err());
    }
}
