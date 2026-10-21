//! Local display record for one manual `!` command.
//!
//! The command is not a job and the record is not history: it disappears as
//! soon as the official synthetic `bash-mode` user message becomes durable.
//! This is only the immediate output window the user sees before the next step
//! claims the injected context.

use serde_json::Value;

use crate::view::{CollectedOutput, ShellResult};

/// Lifecycle of one manual shell record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashState {
    /// The request is in flight.
    Running,
    /// The executor returned a result (including a nonzero exit).
    Done,
    /// The request never reached execution.
    Failed(String),
    /// The transport failed after the request may have been delivered.
    Unknown(String),
}

/// One `!` command's bounded display state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRecord {
    /// Local display identity.
    pub id: u64,
    /// Exact command text after `!`.
    pub command: String,
    /// Lifecycle.
    pub state: BashState,
    /// Executor result, once returned.
    pub result: Option<ShellResult>,
    /// Official synthetic message id, when injection succeeded.
    pub injected_message_id: Option<String>,
    /// Injection failure, when the command ran but context injection failed.
    pub inject_error: Option<String>,
}

impl BashRecord {
    /// Create a record before the request is sent.
    pub fn new(id: u64, command: impl Into<String>) -> Self {
        Self {
            id,
            command: command.into(),
            state: BashState::Running,
            result: None,
            injected_message_id: None,
            inject_error: None,
        }
    }

    /// Apply the `nano.shell.exec` result. A nonzero exit is still `Done`.
    pub fn finish(&mut self, value: &Value) -> Result<(), String> {
        let result: ShellResult = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid shell result: {error}"))?;
        self.injected_message_id = result.injected_message_id.clone();
        self.inject_error = result.inject_error.clone();
        self.result = Some(result);
        self.state = BashState::Done;
        Ok(())
    }

    /// Mark a request that never ran. The caller restores the draft.
    pub fn fail(&mut self, message: impl Into<String>) {
        self.state = BashState::Failed(message.into());
    }

    /// Mark an outcome that is genuinely unknown. Never restored or retried.
    pub fn unknown(&mut self, message: impl Into<String>) {
        self.state = BashState::Unknown(message.into());
    }

    /// Whether the official synthetic message is now durable.
    pub fn matches_injected(&self, message_id: &str) -> bool {
        self.injected_message_id.as_deref() == Some(message_id)
    }

    /// Status-line label.
    pub fn outcome_label(&self) -> String {
        match &self.state {
            BashState::Running => "运行中".to_string(),
            BashState::Done => self
                .result
                .as_ref()
                .map(ShellResult::outcome_label)
                .unwrap_or_else(|| "完成".to_string()),
            BashState::Failed(message) => format!("未能执行：{message}"),
            BashState::Unknown(message) => format!("结果未知：{message}"),
        }
    }

    /// Collected stdout with its truncation notice.
    pub fn stdout(&self) -> String {
        self.result
            .as_ref()
            .map(|result| result.stdout.display())
            .unwrap_or_default()
    }

    /// Collected stderr with its truncation notice.
    pub fn stderr(&self) -> String {
        self.result
            .as_ref()
            .map(|result| result.stderr.display())
            .unwrap_or_default()
    }

    /// True when both streams are empty.
    pub fn output_is_empty(&self) -> bool {
        self.result
            .as_ref()
            .map(|result| result.stdout.text.is_empty() && result.stderr.text.is_empty())
            .unwrap_or(true)
    }
}

impl CollectedOutput {
    /// Display text plus the explicit truncation/spill fact.
    pub fn display(&self) -> String {
        if !self.truncated {
            return self.text.clone();
        }
        match &self.spill_path {
            Some(path) if !path.is_empty() => {
                format!("{}\n[输出已截断；完整输出：{path}]", self.text)
            }
            _ => format!("{}\n[输出已截断]", self.text),
        }
    }
}
