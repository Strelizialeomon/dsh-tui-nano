//! Durable conversation history derived from official `session.event` frames.
//!
//! This is a display projection, not a second history: every item keeps the
//! official sequence, message id, or tool call id. Reconnecting rebuilds it
//! from the replay instead of trusting anything local.

use serde_json::Value;

use crate::events::SessionEvent;
use crate::queue::{rpc_id_of, text_of};
use crate::MAX_HISTORY_ITEMS;

/// Which official object a displayed message represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// A human prompt (`user/message` with a user source).
    User,
    /// A model-produced assistant message.
    Assistant,
    /// A tool-role result that could not be paired with its call.
    ToolResult,
    /// A Nano local bash record injected as a user message. Never an approval.
    SyntheticBash,
    /// A user-role message from another producer, such as an injected notice.
    Injected,
    /// A turn-level notice such as a failure or an interruption.
    Notice,
}

/// One durable message or standalone tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    /// Durable session sequence.
    pub seq: u64,
    /// Official object identity.
    pub message_id: Option<String>,
    /// What produced it.
    pub kind: MessageKind,
    /// Concatenated visible text.
    pub text: String,
    /// Official `rpcId` on a user prompt.
    pub rpc_id: Option<String>,
    /// Owning turn, when recorded.
    pub turn: Option<u64>,
    /// Owning step, when recorded.
    pub step: Option<u64>,
    /// The turn was cancelled mid-stream; this is a prefix, not a full answer.
    pub interrupted: bool,
    /// Tool result identity when this is an unpaired result.
    pub tool_call_id: Option<String>,
    /// Tool failure flag for an unpaired result.
    pub is_error: bool,
}

/// Tool call lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolStatus {
    /// The call is recorded but no result has arrived.
    Pending,
    /// A successful result was recorded.
    Succeeded,
    /// The result carries `isError: true`.
    Failed,
    /// A result arrived without a usable success/failure fact.
    Unknown,
}

/// One tool call paired with its result when available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallView {
    /// Durable sequence of the call event.
    pub seq: u64,
    /// Official call identity used to pair the result.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Raw JSON arguments exactly as produced.
    pub arguments: String,
    /// Result text, when a result arrived.
    pub result: Option<String>,
    /// Failure flag from the result.
    pub is_error: bool,
    /// Lifecycle.
    pub status: ToolStatus,
    /// Owning turn.
    pub turn: Option<u64>,
    /// Owning step.
    pub step: Option<u64>,
}

/// One item in the durable conversation feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationItem {
    /// A message-shaped item.
    Message(MessageView),
    /// A tool call with its result.
    ToolCall(ToolCallView),
}

impl ConversationItem {
    /// Stable display identity.
    pub fn identity(&self) -> String {
        match self {
            Self::Message(message) => message
                .message_id
                .clone()
                .unwrap_or_else(|| format!("seq-{}", message.seq)),
            Self::ToolCall(call) => format!("call-{}", call.call_id),
        }
    }
}

/// What one durable event changed outside the history itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryEffect {
    /// A user message became durable; retire the matching echo.
    pub user_message: Option<UserMessageFact>,
    /// An assistant message settled this step; retire its live preview.
    pub assistant_settled: Option<(u64, u64)>,
    /// An attempt settled without a visible message; retire its preview.
    pub attempt_settled: Option<(u64, u64)>,
}

/// The durable identity of one accepted prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageFact {
    /// Durable sequence.
    pub seq: u64,
    /// Official message id.
    pub message_id: String,
    /// Official request correlation id, when the producer supplied one.
    pub rpc_id: Option<String>,
}

/// Bounded durable display history.
#[derive(Debug, Clone)]
pub struct ConversationHistory {
    items: Vec<ConversationItem>,
    max_items: usize,
    truncated: bool,
}

impl Default for ConversationHistory {
    fn default() -> Self {
        Self::new(MAX_HISTORY_ITEMS)
    }
}

impl ConversationHistory {
    /// Build with an explicit item bound.
    pub fn new(max_items: usize) -> Self {
        Self {
            items: Vec::new(),
            max_items,
            truncated: false,
        }
    }

    /// Items in durable order.
    pub fn items(&self) -> &[ConversationItem] {
        &self.items
    }

    /// Whether older items were dropped by the display bound.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Number of retained items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Find a durable prompt by request correlation id.
    pub fn find_request(&self, request_id: &str) -> Option<&MessageView> {
        self.items.iter().find_map(|item| match item {
            ConversationItem::Message(message) if message.rpc_id.as_deref() == Some(request_id) => {
                Some(message)
            }
            _ => None,
        })
    }

    /// Apply one official durable event.
    pub fn apply(&mut self, event: &SessionEvent) -> HistoryEffect {
        let mut effect = HistoryEffect::default();
        match event.event_type.as_str() {
            "user/message" => {
                if let Some(message) = self.user_message(event.seq, &event.data) {
                    effect.user_message = Some(UserMessageFact {
                        seq: message.seq,
                        message_id: message.message_id.clone().unwrap_or_default(),
                        rpc_id: message.rpc_id.clone(),
                    });
                    self.push(ConversationItem::Message(message));
                }
            }
            "assistant/message" => {
                let turn = event.data.get("turn").and_then(Value::as_u64);
                let step = event.data.get("step").and_then(Value::as_u64);
                if let Some(message) = self.assistant_message(event.seq, &event.data) {
                    self.push(ConversationItem::Message(message));
                }
                if let (Some(turn), Some(step)) = (turn, step) {
                    effect.assistant_settled = Some((turn, step));
                }
            }
            "assistant/attempt" => {
                let turn = event.data.get("turn").and_then(Value::as_u64);
                let step = event.data.get("step").and_then(Value::as_u64);
                if let (Some(turn), Some(step)) = (turn, step) {
                    effect.attempt_settled = Some((turn, step));
                }
            }
            "tool/call" => {
                if let Some(call) = tool_call(event) {
                    self.push(ConversationItem::ToolCall(call));
                }
            }
            "turn/end" => {
                if let Some(message) = self.turn_end(event.seq, &event.data) {
                    self.push(ConversationItem::Message(message));
                }
            }
            "tool/result" => {
                let call_id = tool_result_call_id(&event.data);
                if let Some(call_id) = call_id {
                    if !self.attach_tool_result(&event.data, &call_id) {
                        if let Some(message) =
                            self.unpaired_tool_result(event.seq, &event.data, &call_id)
                        {
                            self.push(ConversationItem::Message(message));
                        }
                    }
                }
            }
            _ => {}
        }
        effect
    }

    /// Drop all display state before rebuilding from replay.
    pub fn clear(&mut self) {
        self.items.clear();
        self.truncated = false;
    }

    fn push(&mut self, item: ConversationItem) {
        self.items.push(item);
        while self.items.len() > self.max_items {
            self.items.remove(0);
            self.truncated = true;
        }
    }

    fn user_message(&self, seq: u64, data: &Value) -> Option<MessageView> {
        let message_id = data.get("id").and_then(Value::as_str).map(str::to_string);
        let content = data.get("content");
        let text = text_of(content);
        let source_kind = data
            .get("source")
            .and_then(|source| source.get("kind"))
            .and_then(Value::as_str);
        let kind = if source_kind == Some("bash-mode") {
            MessageKind::SyntheticBash
        } else if source_kind == Some("user") {
            // A real user prompt is never reclassified by its text, even when
            // it discusses the Nano bash tags.
            MessageKind::User
        } else if source_kind.is_some() && is_synthetic_bash(&text) {
            MessageKind::SyntheticBash
        } else {
            MessageKind::Injected
        };
        Some(MessageView {
            seq,
            message_id,
            kind,
            text,
            rpc_id: rpc_id_of(data.get("source")),
            turn: None,
            step: None,
            interrupted: false,
            tool_call_id: None,
            is_error: false,
        })
    }

    fn assistant_message(&self, seq: u64, data: &Value) -> Option<MessageView> {
        let message = data.get("message")?;
        Some(MessageView {
            seq,
            message_id: message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind: MessageKind::Assistant,
            text: text_of(message.get("content")),
            rpc_id: None,
            turn: data.get("turn").and_then(Value::as_u64),
            step: data.get("step").and_then(Value::as_u64),
            interrupted: data
                .get("interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tool_call_id: None,
            is_error: false,
        })
    }

    fn turn_end(&self, seq: u64, data: &Value) -> Option<MessageView> {
        let reason = data.get("reason")?;
        let kind = reason.get("kind").and_then(Value::as_str)?;
        if kind == "completed" {
            return None;
        }
        let (text, is_error) = if kind == "error" {
            let message = reason
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("回合失败");
            (message.to_string(), true)
        } else {
            let label = match kind {
                "aborted" => "回合已取消",
                "interrupted" => "回合被中断",
                "blocked" => "回合被阻断",
                "max-tokens" => "回合达到输出上限",
                "forked" => "回合被 fork",
                other => other,
            };
            (label.to_string(), false)
        };
        Some(MessageView {
            seq,
            message_id: None,
            kind: MessageKind::Notice,
            text,
            rpc_id: None,
            turn: data.get("turn").and_then(Value::as_u64),
            step: None,
            interrupted: false,
            tool_call_id: None,
            is_error,
        })
    }

    fn attach_tool_result(&mut self, data: &Value, call_id: &str) -> bool {
        let Some(call) = self.items.iter_mut().find_map(|item| match item {
            ConversationItem::ToolCall(call) if call.call_id == call_id => Some(call),
            _ => None,
        }) else {
            return false;
        };
        let is_error = data
            .get("message")
            .and_then(|message| message.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        call.result = Some(text_of(
            data.get("message")
                .and_then(|message| message.get("content")),
        ));
        call.is_error = is_error;
        call.status = if is_error {
            ToolStatus::Failed
        } else if data
            .get("message")
            .and_then(|message| message.get("isError"))
            .is_some()
        {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Unknown
        };
        true
    }

    fn unpaired_tool_result(&self, seq: u64, data: &Value, call_id: &str) -> Option<MessageView> {
        let message = data.get("message")?;
        let is_error = message
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(MessageView {
            seq,
            message_id: message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind: MessageKind::ToolResult,
            text: text_of(message.get("content")),
            rpc_id: None,
            turn: data.get("turn").and_then(Value::as_u64),
            step: data.get("step").and_then(Value::as_u64),
            interrupted: false,
            tool_call_id: Some(call_id.to_string()),
            is_error,
        })
    }
}

fn tool_call(event: &SessionEvent) -> Option<ToolCallView> {
    let call_id = event.data.get("callId")?.as_str()?.to_string();
    Some(ToolCallView {
        seq: event.seq,
        call_id,
        name: event
            .data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string(),
        arguments: event
            .data
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: None,
        is_error: false,
        status: ToolStatus::Pending,
        turn: event.data.get("turn").and_then(Value::as_u64),
        step: event.data.get("step").and_then(Value::as_u64),
    })
}

fn tool_result_call_id(data: &Value) -> Option<String> {
    let message = data.get("message")?;
    if let Some(call_id) = message.get("toolCallId").and_then(Value::as_str) {
        return Some(call_id.to_string());
    }
    // V3 compatibility: the identity lived on the first content block.
    message
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("toolCallId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn is_synthetic_bash(text: &str) -> bool {
    const TAGS: [&str; 3] = ["<bash-input>", "<bash-stdout>", "<bash-stderr>"];
    TAGS.iter().any(|tag| text.contains(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(seq: u64, event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            address: crate::events::WireAddress {
                root_session_id: "root".into(),
                child_session_id: None,
                parent_session_id: None,
                mode: None,
            },
            seq,
            event_type: event_type.into(),
            data,
        }
    }

    #[test]
    fn user_and_assistant_messages_keep_official_identity() {
        let mut history = ConversationHistory::default();
        let effect = history.apply(&event(
            1,
            "user/message",
            json!({
                "id": "u-1",
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
                "source": {"kind": "user", "rpcId": "r-1"}
            }),
        ));
        assert_eq!(effect.user_message.unwrap().message_id, "u-1");
        history.apply(&event(
            2,
            "assistant/message",
            json!({
                "turn": 1,
                "step": 1,
                "message": {"id": "a-1", "role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                "stream": []
            }),
        ));
        assert_eq!(history.items().len(), 2);
        assert_eq!(history.find_request("r-1").unwrap().seq, 1);
        let ConversationItem::Message(assistant) = &history.items()[1] else {
            panic!("expected assistant message");
        };
        assert_eq!(assistant.text, "hello");
        assert_eq!(assistant.kind, MessageKind::Assistant);
    }

    #[test]
    fn tool_call_and_result_pair_by_call_id_in_v4_and_v3_shapes() {
        let mut history = ConversationHistory::default();
        history.apply(&event(
            1,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c-1", "name": "bash", "arguments": "{}"}),
        ));
        history.apply(&event(
            2,
            "tool/result",
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "t-1",
                    "role": "tool",
                    "toolCallId": "c-1",
                    "isError": false,
                    "content": [{"type": "text", "text": "ok"}]
                }
            }),
        ));
        let ConversationItem::ToolCall(call) = &history.items()[0] else {
            panic!("expected tool call");
        };
        assert_eq!(call.status, ToolStatus::Succeeded);
        assert_eq!(call.result.as_deref(), Some("ok"));

        history.apply(&event(
            3,
            "tool/result",
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "t-2",
                    "role": "tool",
                    "isError": true,
                    "content": [{"type": "text", "text": "bad", "toolCallId": "c-2"}]
                }
            }),
        ));
        let ConversationItem::Message(message) = &history.items()[1] else {
            panic!("expected unpaired result");
        };
        assert_eq!(message.tool_call_id.as_deref(), Some("c-2"));
        assert!(message.is_error);
    }

    #[test]
    fn synthetic_bash_is_never_a_user_prompt() {
        let mut history = ConversationHistory::default();
        history.apply(&event(
            1,
            "user/message",
            json!({
                "id": "u-1",
                "role": "user",
                "content": [{"type": "text", "text": "<bash-input>ls</bash-input>"}],
                "source": {"kind": "bash-mode"}
            }),
        ));
        let ConversationItem::Message(message) = &history.items()[0] else {
            panic!("expected message");
        };
        assert_eq!(message.kind, MessageKind::SyntheticBash);
    }

    #[test]
    fn turn_failures_are_visible_but_completion_is_not() {
        let mut history = ConversationHistory::default();
        history.apply(&event(
            1,
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
        ));
        assert!(history.items().is_empty());
        history.apply(&event(
            2,
            "turn/end",
            json!({"turn": 2, "reason": {"kind": "error", "error": {"message": "no provider/model"}}}),
        ));
        let ConversationItem::Message(message) = &history.items()[0] else {
            panic!("expected notice");
        };
        assert_eq!(message.kind, MessageKind::Notice);
        assert!(message.is_error);
        assert_eq!(message.text, "no provider/model");
        history.apply(&event(
            3,
            "turn/end",
            json!({"turn": 3, "reason": {"kind": "aborted"}}),
        ));
        assert_eq!(history.items().len(), 2);
        assert!(!matches!(history.items()[1], ConversationItem::Message(ref m) if m.is_error));
    }

    #[test]
    fn a_user_message_that_mentions_bash_tags_is_still_a_user_message() {
        let mut history = ConversationHistory::default();
        history.apply(&event(
            1,
            "user/message",
            json!({
                "id": "u-1",
                "role": "user",
                "content": [{"type": "text", "text": "how does <bash-input> work?"}],
                "source": {"kind": "user"}
            }),
        ));
        let ConversationItem::Message(message) = &history.items()[0] else {
            panic!("expected message");
        };
        assert_eq!(message.kind, MessageKind::User);
    }

    #[test]
    fn history_is_bounded_and_marks_truncation() {
        let mut history = ConversationHistory::new(2);
        for seq in 0..4 {
            history.apply(&event(
                seq,
                "user/message",
                json!({
                    "id": format!("u-{seq}"),
                    "role": "user",
                    "content": [{"type": "text", "text": "x"}],
                    "source": {"kind": "user"}
                }),
            ));
        }
        assert_eq!(history.len(), 2);
        assert!(history.truncated());
    }
}
