//! Wire projections owned by the official job registry.
//!
//! The bridge never invents job state: these types deserialize the official
//! `JobView` projection (plus the durable `job/registered` tool-call link the
//! projection itself omits). No Nano-side task table exists.

use serde::Deserialize;

/// Lifecycle state of one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Live work.
    Running,
    /// A stop was requested; the terminal fact has not arrived.
    Stopping,
    /// Finished normally.
    Completed,
    /// Stopped by a kill request or teardown.
    Killed,
    /// The producer reported a failure.
    Failed,
    /// A status this build does not know; displayed raw, never guessed.
    Unknown,
}

impl JobStatus {
    /// Parse the official status string.
    pub fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "stopping" => Self::Stopping,
            "completed" => Self::Completed,
            "killed" => Self::Killed,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    /// Chinese display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Stopping => "停止中",
            Self::Completed => "完成",
            Self::Killed => "被终止",
            Self::Failed => "失败",
            Self::Unknown => "未知状态",
        }
    }

    /// Whether no further state change is expected.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }

    /// Whether the registry still considers the job live.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

/// Durable tool-call correlation copied from `job/registered`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobToolCall {
    /// Exact invocation, including a PTC sub-call.
    pub call_id: String,
    /// Root model-requested invocation.
    pub root_call_id: String,
}

/// Output-ring coordinates reported by the official registry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutput {
    /// Offset the next chunk starts at.
    #[serde(default)]
    pub total: u64,
    /// Oldest retained byte; greater than zero means retention dropped the head.
    #[serde(default)]
    pub earliest: u64,
    /// Complete-stream spill files currently kept by pull sources.
    #[serde(default)]
    pub spill_paths: Vec<String>,
}

/// One read-only job projection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    /// Registry-issued `<kind>-<short-uuid>`.
    pub id: String,
    /// Producer kind (for example `bash`).
    pub kind: String,
    /// One-line label.
    pub label: String,
    /// Owning session; absent for an unowned job.
    #[serde(default)]
    pub owner: Option<String>,
    /// Raw official status.
    #[serde(default)]
    pub status: String,
    /// Live progress line.
    #[serde(default)]
    pub progress: Option<String>,
    /// Terminal reason or recorded kill reason.
    #[serde(default)]
    pub detail: Option<String>,
    /// Registration time in epoch milliseconds.
    #[serde(default)]
    pub started_at: u64,
    /// Settlement time in epoch milliseconds.
    #[serde(default)]
    pub finished_at: Option<u64>,
    /// Output-ring coordinates.
    #[serde(default)]
    pub output: JobOutput,
    /// Durable creating tool invocation, added by the bridge.
    #[serde(default)]
    pub tool_call: Option<JobToolCall>,
}

impl JobView {
    /// Parsed lifecycle state.
    pub fn status(&self) -> JobStatus {
        JobStatus::parse(&self.status)
    }

    /// Whether the job can still change state.
    pub fn is_active(&self) -> bool {
        self.status().is_active()
    }

    /// One-line display status including progress or terminal detail.
    pub fn status_line(&self) -> String {
        let mut line = self.status().label().to_string();
        if let Some(progress) = &self.progress {
            if !progress.is_empty() {
                line.push_str(" · ");
                line.push_str(progress);
            }
        }
        if let Some(detail) = &self.detail {
            if !detail.is_empty() {
                line.push_str(" · ");
                line.push_str(detail);
            }
        }
        line
    }

    /// Owning call identity for the list without expanding the projection.
    pub fn call_label(&self) -> Option<&str> {
        self.tool_call
            .as_ref()
            .map(|call| call.root_call_id.as_str())
    }
}

/// One retained output chunk at an absolute byte offset.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobChunk {
    /// Absolute byte offset of the chunk's first byte.
    pub at: u64,
    /// Chunk text exactly as appended.
    #[serde(default)]
    pub text: String,
    /// Stream label (`stdout`, `stderr`, `log`).
    #[serde(default)]
    pub channel: Option<String>,
    /// Bytes immediately before this chunk were lost.
    #[serde(default)]
    pub gap_before: bool,
}

/// One `nano.jobs.follow` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutputRead {
    /// Retained chunks overlapping the requested range.
    #[serde(default)]
    pub chunks: Vec<JobChunk>,
    /// Authoritative resume offset.
    #[serde(default)]
    pub next: u64,
    /// The requested offset slid below the retained window.
    #[serde(default)]
    pub lossy: bool,
}

/// One `nano.shell.exec` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellResult {
    /// Exit code; missing when a signal or preparation expiry ended the run.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Terminating signal name.
    #[serde(default)]
    pub signal: Option<String>,
    /// The executor deadline was the first cause of termination.
    #[serde(default)]
    pub timed_out: bool,
    /// The caller aborted the run.
    #[serde(default)]
    pub aborted: bool,
    /// Collected stdout.
    #[serde(default)]
    pub stdout: CollectedOutput,
    /// Collected stderr.
    #[serde(default)]
    pub stderr: CollectedOutput,
    /// Injected synthetic message id, when injection succeeded.
    #[serde(default)]
    pub injected_message_id: Option<String>,
    /// Injection failure, when the command ran but the context could not be added.
    #[serde(default)]
    pub inject_error: Option<String>,
}

/// One collected output stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedOutput {
    /// Collected text; the tail when truncated.
    #[serde(default)]
    pub text: String,
    /// Whether bytes were dropped from `text`.
    #[serde(default)]
    pub truncated: bool,
    /// Complete-stream spill file when available.
    #[serde(default)]
    pub spill_path: Option<String>,
}

impl ShellResult {
    /// Outcome label for the feed and status line.
    pub fn outcome_label(&self) -> String {
        if self.timed_out {
            if let Some(signal) = &self.signal {
                return format!("超时（信号 {signal}）");
            }
            return match self.exit_code {
                Some(code) => format!("超时（退出码 {code}）"),
                None => "超时（退出码未知）".to_string(),
            };
        }
        if self.aborted {
            return match &self.signal {
                Some(signal) => format!("已中止（信号 {signal}）"),
                None => "已中止".to_string(),
            };
        }
        if let Some(signal) = &self.signal {
            return format!("被信号 {signal} 终止");
        }
        match self.exit_code {
            Some(code) => format!("退出码 {code}"),
            None => "退出码未知".to_string(),
        }
    }
}
