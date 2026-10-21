//! Tool-call and tool-result records.
//!
//! Every record is keyed by the official `callId` supplied by S02. S03 only
//! enriches the display: it parses the official argument/result payloads, but
//! never pairs calls itself, never treats pending arguments as executed, and
//! never fabricates an exit code or a diff coordinate.

use std::collections::BTreeMap;

use nano_conversation::{ToolCallView, ToolStatus};
use nano_decisions::approval::ApprovalOutcome;
use nano_decisions::contract::{parse_answers, parse_questions, AnswerItem, Question};
use serde_json::Value;

use crate::diff::{DiffState, DiffView};

/// The terminal status a tool card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayStatus {
    /// No result yet.
    Pending,
    /// A successful result with no contrary fact.
    Succeeded,
    /// A failure, non-zero exit, or explicit error.
    Failed,
    /// The call was interrupted or never started.
    Interrupted,
    /// The result is missing or cannot be classified.
    Unknown,
}

impl DisplayStatus {
    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "等待结果",
            Self::Succeeded => "完成",
            Self::Failed => "失败",
            Self::Interrupted => "已中断",
            Self::Unknown => "结果未知",
        }
    }
}

/// The shell exit fact recovered from the official result text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExitStatus {
    /// A real `[exit code: N]` marker.
    Code(i64),
    /// A real `[killed by signal: X]` marker.
    Signal(String),
    /// The command hit its timeout.
    TimedOut,
    /// The command was stopped.
    Stopped(String),
    /// No terminal marker was recorded; the exit code is unknown, not zero.
    Unknown,
}

impl ExitStatus {
    /// Short label. The unknown case is explicit on purpose.
    pub fn label(&self) -> String {
        match self {
            Self::Code(code) => format!("退出码 {code}"),
            Self::Signal(signal) => format!("信号 {signal}"),
            Self::TimedOut => "超时".to_string(),
            Self::Stopped(reason) => format!("已停止：{reason}"),
            Self::Unknown => "退出码未知".to_string(),
        }
    }
}

/// The failure identity attached by the official tool pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolErrorFact {
    /// Error name.
    pub name: String,
    /// Stable code, such as `TOOL_OUTCOME_UNKNOWN`.
    pub code: String,
    /// Optional human reason.
    pub reason: Option<String>,
}

/// The result metadata and failure identity from one `tool/result` event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResultFact {
    /// Tool-call identity.
    pub call_id: String,
    /// Official opaque result metadata (for example `FsDiffMeta`).
    pub meta: Option<Value>,
    /// Official failure identity.
    pub error: Option<ToolErrorFact>,
    /// Official result text, when the event carries it.
    pub text: Option<String>,
}

/// One recorded approval on a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalView {
    /// Tool the approval is about.
    pub tool_name: String,
    /// Raw reason.
    pub reason: Option<String>,
    /// Localized reason.
    pub display_reason: BTreeMap<String, String>,
    /// Closed outcome, when the audit pair completed.
    pub outcome: Option<ApprovalOutcome>,
    /// Someone asked; the outcome may still be unknown.
    pub asked: bool,
    /// A live card is waiting for this decision.
    pub pending: bool,
}

impl ApprovalView {
    /// Short outcome line.
    pub fn outcome_label(&self) -> &'static str {
        match self.outcome {
            Some(outcome) => outcome.label(),
            None if self.pending => "审批中",
            None if self.asked => "结果未知",
            None => "未请求",
        }
    }
}

/// Recorded question state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordedQuestionState {
    /// A live answerable request.
    Open,
    /// The foreground wait expired; the question remains answerable.
    Continued,
    /// An answer was handed to the bridge; not confirmed yet.
    Submitted,
    /// The request settled with a batch.
    Settled,
    /// The wait was cancelled or rejected.
    Cancelled,
}

impl RecordedQuestionState {
    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "待回答",
            Self::Continued => "可继续回答",
            Self::Submitted => "已提交",
            Self::Settled => "已回答",
            Self::Cancelled => "已取消",
        }
    }
}

/// One recorded `ask_user_question` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionRecord {
    /// Questions from the recorded call arguments.
    pub questions: Vec<Question>,
    /// Whether the recorded schema was the timed one.
    pub timed: bool,
    /// Recorded state.
    pub state: RecordedQuestionState,
    /// Recorded answers, when settled.
    pub answers: Vec<AnswerItem>,
}

impl QuestionRecord {
    /// Whether the record can be opened as a read-only card.
    pub fn can_review(&self) -> bool {
        !self.questions.is_empty()
            && matches!(
                self.state,
                RecordedQuestionState::Settled | RecordedQuestionState::Cancelled
            )
    }

    /// Whether the question can still be answered.
    pub fn answerable(&self) -> bool {
        !self.questions.is_empty()
            && matches!(
                self.state,
                RecordedQuestionState::Open | RecordedQuestionState::Continued
            )
    }
}

/// The tool-specific display data.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDisplay {
    /// A shell command.
    Bash {
        /// The command text, when the official arguments carry it.
        command: Option<String>,
        /// The description, when supplied.
        description: Option<String>,
        /// Output body with terminal markers removed.
        output: String,
        /// Terminal fact.
        exit: ExitStatus,
    },
    /// A file mutation.
    File {
        /// File path, when the result or arguments carry it.
        path: Option<String>,
        /// Diff information.
        diff: Option<DiffView>,
    },
    /// A user-question call.
    AskUser {
        /// Recorded question facts.
        record: QuestionRecord,
    },
    /// Another tool.
    Generic,
}

/// One enriched tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRecord {
    /// Official call identity.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// S02 lifecycle.
    pub status: ToolStatus,
    /// Display status after checking exit facts and failure identity.
    pub display_status: DisplayStatus,
    /// Owning turn.
    pub turn: Option<u64>,
    /// Owning step.
    pub step: Option<u64>,
    /// Raw arguments exactly as recorded.
    pub arguments: String,
    /// Result text.
    pub result: Option<String>,
    /// Result metadata and failure identity.
    pub result_fact: Option<ToolResultFact>,
    /// Approval facts when one was requested for this call.
    pub approval: Option<ApprovalView>,
    /// Tool-specific display.
    pub display: ToolDisplay,
}

impl ToolRecord {
    /// Build one record from S02 identity plus transcript enrichment.
    pub fn build(
        call: &ToolCallView,
        result_fact: Option<&ToolResultFact>,
        approval: Option<&ApprovalView>,
        question: Option<&QuestionRecord>,
    ) -> Self {
        let exit = if call.name == "bash" {
            call.result
                .as_deref()
                .map(|text| parse_bash_result(text).1.unwrap_or(ExitStatus::Unknown))
        } else {
            None
        };
        let display_status = display_status(call, result_fact, exit.as_ref());
        let display = build_display(call, result_fact, question, display_status);
        Self {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            status: call.status,
            display_status,
            turn: call.turn,
            step: call.step,
            arguments: call.arguments.clone(),
            result: call.result.clone(),
            result_fact: result_fact.cloned(),
            approval: approval.cloned(),
            display,
        }
    }

    /// One-line result summary for the collapsed card.
    pub fn summary(&self) -> Option<String> {
        match &self.display {
            ToolDisplay::Bash { output, .. } => {
                let first = output.lines().find(|line| !line.trim().is_empty());
                first.map(|line| line.trim().to_string())
            }
            ToolDisplay::File { diff, .. } => diff.as_ref().map(|diff| diff.stat_label()),
            ToolDisplay::AskUser { record } => Some(record.state.label().to_string()),
            ToolDisplay::Generic => self
                .result
                .as_deref()
                .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                .map(|line| line.trim().to_string()),
        }
    }
}

fn display_status(
    call: &ToolCallView,
    result_fact: Option<&ToolResultFact>,
    exit: Option<&ExitStatus>,
) -> DisplayStatus {
    if call.status == ToolStatus::Pending {
        return DisplayStatus::Pending;
    }
    if let Some(error) = result_fact.and_then(|fact| fact.error.as_ref()) {
        return match error.code.as_str() {
            "TOOL_OUTCOME_UNKNOWN" => DisplayStatus::Unknown,
            "TOOL_NOT_STARTED" => DisplayStatus::Interrupted,
            _ => DisplayStatus::Failed,
        };
    }
    if call.is_error || call.status == ToolStatus::Failed {
        return DisplayStatus::Failed;
    }
    if let Some(exit) = exit {
        return match exit {
            ExitStatus::Code(0) => DisplayStatus::Succeeded,
            ExitStatus::Code(_) | ExitStatus::TimedOut => DisplayStatus::Failed,
            ExitStatus::Signal(_) | ExitStatus::Stopped(_) => DisplayStatus::Interrupted,
            ExitStatus::Unknown => DisplayStatus::Unknown,
        };
    }
    if call.status == ToolStatus::Succeeded {
        DisplayStatus::Succeeded
    } else {
        DisplayStatus::Unknown
    }
}

fn build_display(
    call: &ToolCallView,
    result_fact: Option<&ToolResultFact>,
    question: Option<&QuestionRecord>,
    display_status: DisplayStatus,
) -> ToolDisplay {
    match call.name.as_str() {
        "bash" => {
            let arguments = serde_json::from_str::<Value>(&call.arguments).ok();
            let command = arguments
                .as_ref()
                .and_then(|value| value.get("command"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let description = arguments
                .as_ref()
                .and_then(|value| value.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let (output, exit) = parse_bash_result(call.result.as_deref().unwrap_or(""));
            ToolDisplay::Bash {
                command,
                description,
                output,
                exit: exit.unwrap_or(ExitStatus::Unknown),
            }
        }
        "write" | "edit" => {
            let path = serde_json::from_str::<Value>(&call.arguments)
                .ok()
                .and_then(|value| {
                    value
                        .get("file_path")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            let state = match display_status {
                DisplayStatus::Pending => DiffState::Pending,
                DisplayStatus::Unknown => DiffState::Unknown,
                DisplayStatus::Failed | DisplayStatus::Interrupted => DiffState::NotApplied,
                _ => DiffState::Applied,
            };
            let from_meta = result_fact
                .and_then(|fact| fact.meta.as_ref())
                .and_then(|meta| DiffView::from_result_meta(meta, state));
            let diff = from_meta.or_else(|| {
                DiffView::from_call_arguments(&call.arguments, &call.name).map(|mut diff| {
                    diff.state = state;
                    diff
                })
            });
            let path = path.or_else(|| diff.as_ref().and_then(|diff| diff.path.clone()));
            ToolDisplay::File { path, diff }
        }
        "ask_user_question" => {
            if let Some(record) = question {
                ToolDisplay::AskUser {
                    record: record.clone(),
                }
            } else {
                let questions = serde_json::from_str::<Value>(&call.arguments)
                    .ok()
                    .map(|value| parse_questions(value.get("questions")))
                    .unwrap_or_default();
                ToolDisplay::AskUser {
                    record: QuestionRecord {
                        questions,
                        timed: recorded_timeout(&call.arguments),
                        state: derive_question_state(call, None, None),
                        answers: parse_answers_from_result(call.result.as_deref())
                            .unwrap_or_default(),
                    },
                }
            }
        }
        _ => ToolDisplay::Generic,
    }
}

fn recorded_timeout(arguments: &str) -> bool {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .map(|value| value.get("timeout").is_some())
        .unwrap_or(false)
}

/// Derive the recorded question state from the official result and projection.
pub fn derive_question_state(
    call: &ToolCallView,
    settled: Option<&[AnswerItem]>,
    continued: Option<bool>,
) -> RecordedQuestionState {
    if settled.is_some() {
        return RecordedQuestionState::Settled;
    }
    if continued == Some(true) {
        return RecordedQuestionState::Continued;
    }
    let text = call.result.as_deref().unwrap_or("");
    if parse_answers_from_result(Some(text)).is_some() {
        return RecordedQuestionState::Settled;
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value.get("pending").and_then(Value::as_bool) == Some(true) {
            return RecordedQuestionState::Continued;
        }
    }
    if text.contains("aborted before the user answered")
        || text.contains("ASK_CANCELLED")
        || text.contains("cancelled")
    {
        return RecordedQuestionState::Cancelled;
    }
    if call.is_error {
        return RecordedQuestionState::Cancelled;
    }
    RecordedQuestionState::Open
}

/// Parse the blocking tool result `{"answers":[...]}`.
pub fn parse_answers_from_result(text: Option<&str>) -> Option<Vec<AnswerItem>> {
    let value: Value = serde_json::from_str(text?).ok()?;
    parse_answers(value.get("answers"))
}

/// Split a bash result into its body and terminal status. Terminal markers are
/// removed from the body; the text itself is not rewritten in the transcript.
pub fn parse_bash_result(text: &str) -> (String, Option<ExitStatus>) {
    if text.is_empty() {
        return (String::new(), None);
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    let mut status: Option<ExitStatus> = None;
    let mut marker_found = false;
    while let Some(line) = lines.last() {
        let trimmed = line.trim();
        if let Some(signal) = trimmed
            .strip_prefix("[killed by signal: ")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            if status.is_none() {
                status = Some(ExitStatus::Signal(signal.to_string()));
            }
            marker_found = true;
            lines.pop();
            continue;
        }
        if let Some(code) = trimmed
            .strip_prefix("[exit code: ")
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|code| code.parse::<i64>().ok())
        {
            if status.is_none() {
                status = Some(ExitStatus::Code(code));
            }
            marker_found = true;
            lines.pop();
            continue;
        }
        if trimmed.starts_with("[timed out after ") && trimmed.ends_with(']') {
            if status.is_none() {
                status = Some(ExitStatus::TimedOut);
            }
            marker_found = true;
            lines.pop();
            continue;
        }
        if let Some(reason) = trimmed
            .strip_prefix("[stopped: ")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            if status.is_none() {
                status = Some(ExitStatus::Stopped(reason.to_string()));
            }
            marker_found = true;
            lines.pop();
            continue;
        }
        break;
    }
    let mut body = lines.join("\n");
    if marker_found && body.is_empty() {
        body = String::new();
    }
    (body, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nano_conversation::{ToolCallView, ToolStatus};

    fn call(name: &str, arguments: &str, result: Option<&str>, is_error: bool) -> ToolCallView {
        ToolCallView {
            seq: 1,
            call_id: "c-1".into(),
            name: name.into(),
            arguments: arguments.into(),
            result: result.map(str::to_string),
            is_error,
            status: if result.is_some() {
                if is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Succeeded
                }
            } else {
                ToolStatus::Pending
            },
            turn: Some(1),
            step: Some(1),
        }
    }

    #[test]
    fn a_missing_exit_marker_is_unknown_not_zero() {
        let record = ToolRecord::build(
            &call("bash", r#"{"command":"echo hi"}"#, Some("hi\n"), false),
            None,
            None,
            None,
        );
        let ToolDisplay::Bash { exit, output, .. } = &record.display else {
            panic!("expected bash");
        };
        assert_eq!(*exit, ExitStatus::Unknown);
        assert_eq!(output, "hi");
        assert_eq!(record.display_status, DisplayStatus::Unknown);
        assert_eq!(record.display_status.label(), "结果未知");
    }

    #[test]
    fn a_nonzero_exit_is_a_failure_with_the_real_code() {
        let record = ToolRecord::build(
            &call(
                "bash",
                r#"{"command":"false"}"#,
                Some("oops\n[exit code: 2]"),
                false,
            ),
            None,
            None,
            None,
        );
        let ToolDisplay::Bash { exit, output, .. } = &record.display else {
            panic!("expected bash");
        };
        assert_eq!(*exit, ExitStatus::Code(2));
        assert_eq!(output, "oops");
        assert_eq!(record.display_status, DisplayStatus::Failed);
    }

    #[test]
    fn a_signal_is_shown_and_is_not_success() {
        let record = ToolRecord::build(
            &call(
                "bash",
                r#"{"command":"sleep"}"#,
                Some("[killed by signal: SIGKILL]"),
                false,
            ),
            None,
            None,
            None,
        );
        let ToolDisplay::Bash { exit, .. } = &record.display else {
            panic!("expected bash");
        };
        assert_eq!(*exit, ExitStatus::Signal("SIGKILL".into()));
        assert_eq!(record.display_status, DisplayStatus::Interrupted);
    }

    #[test]
    fn a_write_overwrite_without_a_before_image_shows_only_additions() {
        let result_fact = ToolResultFact {
            call_id: "c-1".into(),
            meta: Some(serde_json::json!({
                "operation": "update",
                "diffs": [{"path": "a.txt", "oldText": null, "newText": "x\ny"}]
            })),
            error: None,
            text: None,
        };
        let record = ToolRecord::build(
            &call(
                "write",
                r#"{"file_path":"a.txt","content":"x\ny"}"#,
                Some("updated"),
                false,
            ),
            Some(&result_fact),
            None,
            None,
        );
        let ToolDisplay::File { diff, .. } = &record.display else {
            panic!("expected file");
        };
        let diff = diff.as_ref().unwrap();
        assert_eq!(diff.added, Some(2));
        assert_eq!(diff.deleted, None);
        assert_eq!(diff.stat_label(), "+2");
        assert_eq!(diff.state, DiffState::Applied);
    }

    #[test]
    fn a_pending_edit_is_marked_pending_and_has_no_coordinates() {
        let record = ToolRecord::build(
            &call(
                "edit",
                r#"{"file_path":"a.rs","old_string":"a","new_string":"b"}"#,
                None,
                false,
            ),
            None,
            None,
            None,
        );
        let ToolDisplay::File { diff, .. } = &record.display else {
            panic!("expected file");
        };
        let diff = diff.as_ref().unwrap();
        assert_eq!(diff.state, DiffState::Pending);
        assert!(diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .all(|line| line.old_no.is_none() && line.new_no.is_none()));
    }

    #[test]
    fn tool_outcome_unknown_is_not_displayed_as_success() {
        let fact = ToolResultFact {
            call_id: "c-1".into(),
            meta: None,
            error: Some(ToolErrorFact {
                name: "ToolError".into(),
                code: "TOOL_OUTCOME_UNKNOWN".into(),
                reason: None,
            }),
            text: Some("outcome unknown".into()),
        };
        let record = ToolRecord::build(
            &call("bash", r#"{"command":"x"}"#, Some("outcome unknown"), true),
            Some(&fact),
            None,
            None,
        );
        assert_eq!(record.display_status, DisplayStatus::Unknown);
    }

    #[test]
    fn a_blocking_question_answer_settles_the_record() {
        let record = ToolRecord::build(
            &call(
                "ask_user_question",
                r#"{"questions":[{"id":"q","question":"Q","options":[{"label":"A"}]}]}"#,
                Some(r#"{"answers":[{"id":"q","selected":["A"]}]}"#),
                false,
            ),
            None,
            None,
            None,
        );
        let ToolDisplay::AskUser { record } = &record.display else {
            panic!("expected question");
        };
        assert_eq!(record.state, RecordedQuestionState::Settled);
        assert_eq!(record.answers[0].selected, vec!["A"]);
    }

    #[test]
    fn an_unknown_outcome_does_not_claim_the_diff_was_applied() {
        let fact = ToolResultFact {
            call_id: "c-1".into(),
            meta: None,
            error: Some(ToolErrorFact {
                name: "ToolError".into(),
                code: "TOOL_OUTCOME_UNKNOWN".into(),
                reason: None,
            }),
            text: Some("outcome unknown".into()),
        };
        let record = ToolRecord::build(
            &call(
                "edit",
                r#"{"file_path":"a.rs","old_string":"a","new_string":"b"}"#,
                Some("outcome unknown"),
                true,
            ),
            Some(&fact),
            None,
            None,
        );
        let ToolDisplay::File { diff, .. } = &record.display else {
            panic!("expected file");
        };
        assert_eq!(diff.as_ref().unwrap().state, DiffState::Unknown);
        assert_ne!(diff.as_ref().unwrap().state.label(), "已应用");
    }

    #[test]
    fn a_pending_timed_result_stays_answerable() {
        let record = ToolRecord::build(
            &call(
                "ask_user_question",
                r#"{"questions":[{"id":"q","question":"Q"}]}"#,
                Some(r#"{"pending":true,"callId":"c-1"}"#),
                false,
            ),
            None,
            None,
            None,
        );
        let ToolDisplay::AskUser { record } = &record.display else {
            panic!("expected question");
        };
        assert_eq!(record.state, RecordedQuestionState::Continued);
        assert!(record.answerable());
    }
}
