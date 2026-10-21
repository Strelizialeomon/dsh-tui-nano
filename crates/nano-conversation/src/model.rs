//! The conversation state machine shared by the app and its tests.
//!
//! One model owns exactly one address. It merges three separate sources into a
//! display projection without ever treating display state as history:
//! durable official events, the absolute inbox/status projections, and live
//! assistant-stream previews.

use serde_json::{json, Value};

use crate::address::SessionAddress;
use crate::events::{
    parse_conversation_event, parse_session_event, ConversationEvent, ConversationStatus,
};
use crate::history::{ConversationHistory, ConversationItem};
use crate::ids::new_request_id;
use crate::preview::{PreviewAccumulator, PreviewView};
use crate::queue::InboxView;
use crate::submissions::{Delivery, EchoView, SubmissionTracker};

/// Why a submission could not be started locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// Nothing but whitespace.
    Empty,
    /// A subagent address is reserved for S12.
    ChildAddress,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("内容为空，草稿已保留"),
            Self::ChildAddress => formatter.write_str("子代理输入属于 S12，本版不提供"),
        }
    }
}

/// One prepared submission.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitPlan {
    /// Business identity to preserve across retries and reconnects.
    pub request_id: String,
    /// Text snapshot submitted.
    pub text: String,
    /// Delivery mode.
    pub delivery: Delivery,
    /// Request parameters.
    pub params: Value,
}

/// Live projection for rendering: previews first, then local echoes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveView {
    /// Active assistant attempts.
    pub previews: Vec<PreviewView>,
    /// Local echoes that no durable message has retired yet.
    pub echoes: Vec<EchoView>,
}

/// One conversation's complete display state.
#[derive(Debug, Clone)]
pub struct ConversationModel {
    address: SessionAddress,
    status: ConversationStatus,
    seq: u64,
    inbox: InboxView,
    history: ConversationHistory,
    previews: PreviewAccumulator,
    submissions: SubmissionTracker,
    replay_truncated: bool,
    last_error: Option<String>,
    last_notice: Option<String>,
}

impl ConversationModel {
    /// Create a model for one address.
    pub fn new(address: SessionAddress) -> Self {
        Self {
            address,
            status: ConversationStatus::Idle,
            seq: 0,
            inbox: InboxView::default(),
            history: ConversationHistory::default(),
            previews: PreviewAccumulator::default(),
            submissions: SubmissionTracker::default(),
            replay_truncated: false,
            last_error: None,
            last_notice: None,
        }
    }

    /// Address this model serves.
    pub fn address(&self) -> &SessionAddress {
        &self.address
    }

    /// Current Agent status.
    pub fn status(&self) -> ConversationStatus {
        self.status
    }

    /// Last durable sequence seen.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Official pending input.
    pub fn inbox(&self) -> &InboxView {
        &self.inbox
    }

    /// Durable history.
    pub fn history(&self) -> &ConversationHistory {
        &self.history
    }

    /// Live previews.
    pub fn previews(&self) -> &PreviewAccumulator {
        &self.previews
    }

    /// Local submission tracker.
    pub fn submissions(&self) -> &SubmissionTracker {
        &self.submissions
    }

    /// Whether the replay window was smaller than the durable history.
    pub fn replay_truncated(&self) -> bool {
        self.replay_truncated
    }

    /// Last non-fatal error worth showing in the status bar.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Last short notice worth showing in the status bar.
    pub fn last_notice(&self) -> Option<&str> {
        self.last_notice.as_deref()
    }

    /// Clear the transient status message.
    pub fn clear_notice(&mut self) {
        self.last_notice = None;
        self.last_error = None;
    }

    /// Apply one `nano.event` kind/payload pair. Returns whether display state
    /// changed.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        if kind == "session.event" {
            return self.apply_session_event(payload);
        }
        match parse_conversation_event(kind, payload) {
            Ok(ConversationEvent::Status { address, status }) if address.matches(&self.address) => {
                let changed = self.status != status;
                self.status = status;
                changed
            }
            Ok(ConversationEvent::Inbox { address, inbox }) if address.matches(&self.address) => {
                let changed = self.inbox != inbox;
                self.inbox = inbox;
                self.submissions.reconcile(&self.inbox, &self.history);
                changed
            }
            Ok(ConversationEvent::Stream { address, frame }) if address.matches(&self.address) => {
                let _retired = self.previews.apply(&frame);
                true
            }
            Ok(_) => false,
            Err(error) => {
                self.last_error = Some(error.0);
                true
            }
        }
    }

    fn apply_session_event(&mut self, payload: &Value) -> bool {
        let event = match parse_session_event(payload) {
            Ok(event) => event,
            Err(error) => {
                self.last_error = Some(error.0);
                return true;
            }
        };
        if !event.address.matches(&self.address) {
            return false;
        }
        self.seq = self.seq.max(event.seq);
        let effect = self.history.apply(&event);
        if let Some(fact) = effect.user_message {
            if let Some(rpc_id) = fact.rpc_id {
                self.submissions.durable_user_message(&rpc_id);
            }
        }
        if let Some((turn, step)) = effect.assistant_settled {
            self.previews.retire_turn_step(turn, step);
        }
        if let Some((turn, step)) = effect.attempt_settled {
            self.previews.retire_turn_step(turn, step);
        }
        true
    }

    /// Apply the absolute baseline read after connecting.
    pub fn apply_snapshot(&mut self, value: &Value) -> Result<(), String> {
        let address: crate::events::WireAddress = value
            .get("address")
            .ok_or_else(|| "snapshot.address is missing".to_string())
            .and_then(|address| {
                serde_json::from_value(address.clone()).map_err(|error| format!("snapshot.address: {error}"))
            })?;
        if !address.matches(&self.address) {
            return Err("snapshot belongs to another conversation".to_string());
        }
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| "snapshot.status is missing".to_string())?;
        let status = match status {
            "idle" => ConversationStatus::Idle,
            "running" => ConversationStatus::Running,
            other => return Err(format!("unknown snapshot status {other}")),
        };
        let seq = value
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| "snapshot.seq is missing".to_string())?;
        let inbox = value
            .get("inbox")
            .map(InboxView::from_value)
            .ok_or_else(|| "snapshot.inbox is missing".to_string())?;
        self.status = status;
        self.seq = self.seq.max(seq);
        self.inbox = inbox;
        self.submissions.reconcile(&self.inbox, &self.history);
        Ok(())
    }

    /// Record how much of the durable log the replay covered.
    pub fn apply_replay_baseline(&mut self, seq: u64, truncated: bool) {
        self.seq = self.seq.max(seq);
        self.replay_truncated = truncated;
    }

    /// Prepare one submission and insert its local echo.
    pub fn begin_submit(
        &mut self,
        text: &str,
        delivery: Delivery,
    ) -> Result<SubmitPlan, SubmitError> {
        if !self.address.is_root() {
            return Err(SubmitError::ChildAddress);
        }
        if !text.chars().any(|character| !character.is_whitespace()) {
            return Err(SubmitError::Empty);
        }
        let request_id = new_request_id();
        let params = self.submit_params(&request_id, text, delivery);
        self.submissions.begin(request_id.clone(), text, delivery);
        Ok(SubmitPlan {
            request_id,
            text: text.to_string(),
            delivery,
            params,
        })
    }

    /// The `nano.conversation.submit` params for a plan.
    pub fn submit_params(&self, request_id: &str, text: &str, delivery: Delivery) -> Value {
        json!({
            "address": self.address.wire_value(),
            "requestId": request_id,
            "delivery": delivery.as_str(),
            "content": [{ "type": "text", "text": text }],
        })
    }

    /// The host accepted the submission.
    pub fn submit_accepted(&mut self, request_id: &str, message_id: &str, duplicate: bool) {
        self.submissions.accepted(request_id, message_id, duplicate);
        self.last_notice = Some(if duplicate {
            "已接收（同一 requestId，未重复发送）".to_string()
        } else {
            "已接收".to_string()
        });
    }

    /// The host explicitly failed the submission.
    pub fn submit_failed(&mut self, request_id: &str, detail: impl Into<String>) {
        let detail = detail.into();
        self.submissions.failed(request_id, detail.clone());
        self.last_error = Some(format!("发送失败：{detail}"));
    }

    /// The result is unknown; do not resend with a new id.
    pub fn submit_unknown(&mut self, request_id: &str, detail: impl Into<String>) {
        let detail = detail.into();
        self.submissions.unknown(request_id, detail.clone());
        self.last_notice = Some(format!("结果未知：{detail}；先查询官方状态，不自动重发"));
    }

    /// The transport ended: drop previews and mark in-flight submissions unknown.
    pub fn mark_disconnected(&mut self) {
        self.previews.clear();
        let ids: Vec<String> = self
            .submissions
            .echoes()
            .into_iter()
            .filter(|echo| echo.state == crate::submissions::SubmissionState::Submitting)
            .map(|echo| echo.request_id)
            .collect();
        for request_id in ids {
            self.submissions.unknown(&request_id, "连接断开");
        }
    }

    /// Recheck unknown submissions against the latest official facts.
    pub fn reconcile(&mut self) {
        self.submissions.reconcile(&self.inbox, &self.history);
    }

    /// Live display projection.
    pub fn live(&self) -> LiveView {
        LiveView {
            previews: self.previews.views(),
            echoes: self.submissions.echoes(),
        }
    }

    /// Durable display items.
    pub fn items(&self) -> &[ConversationItem] {
        self.history.items()
    }

    /// Queue edit params. Text-only by contract; the host rejects attachment
    /// items, and the view should never offer editing for them.
    pub fn queue_edit_params(&self, message_id: &str, text: &str) -> Value {
        json!({
            "address": self.address.wire_value(),
            "messageId": message_id,
            "content": [{ "type": "text", "text": text }],
        })
    }

    /// Queue delete params.
    pub fn queue_delete_params(&self, message_id: &str) -> Value {
        json!({
            "address": self.address.wire_value(),
            "messageId": message_id,
        })
    }

    /// Queue steer params.
    pub fn queue_steer_params(&self, message_id: &str) -> Value {
        json!({
            "address": self.address.wire_value(),
            "messageId": message_id,
        })
    }

    /// Stop params. The host hardcodes `keepInbox: true`.
    pub fn cancel_params(&self) -> Value {
        json!({ "address": self.address.wire_value() })
    }

    /// Snapshot params.
    pub fn snapshot_params(&self) -> Value {
        json!({ "address": self.address.wire_value() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::WireAddress;
    use serde_json::json;

    fn model() -> ConversationModel {
        ConversationModel::new(SessionAddress::root("root"))
    }

    fn durable(model: &mut ConversationModel, seq: u64, event_type: &str, data: serde_json::Value) {
        let payload = json!({
            "address": {"rootSessionId": "root"},
            "seq": seq,
            "type": event_type,
            "data": data,
        });
        assert!(model.apply_event("session.event", &payload));
    }

    #[test]
    fn snapshot_from_another_address_is_refused() {
        let mut model = model();
        let error = model
            .apply_snapshot(&json!({
                "address": {"rootSessionId": "other"},
                "seq": 1,
                "status": "idle",
                "inbox": {"nextTurn": [], "nextStep": []}
            }))
            .unwrap_err();
        assert!(error.contains("another conversation"));
    }

    #[test]
    fn submit_echo_is_retired_by_the_durable_user_message() {
        let mut model = model();
        let plan = model.begin_submit("hello", Delivery::Queue).unwrap();
        assert_eq!(model.live().echoes.len(), 1);
        durable(
            &mut model,
            1,
            "user/message",
            json!({
                "id": "m-1",
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
                "source": {"kind": "user", "rpcId": plan.request_id}
            }),
        );
        assert!(model.live().echoes.is_empty());
        assert_eq!(model.items().len(), 1);
    }

    #[test]
    fn durable_assistant_message_retires_the_matching_preview() {
        let mut model = model();
        let frame = json!({
            "address": {"rootSessionId": "root"},
            "frame": {"type": "start", "attemptId": "a-1", "revision": 1, "turn": 1, "step": 1}
        });
        model.apply_event("conversation.stream", &frame);
        let chunk = json!({
            "address": {"rootSessionId": "root"},
            "frame": {
                "type": "chunk",
                "attemptId": "a-1",
                "revision": 1,
                "index": 0,
                "chunk": {"type": "text-delta", "index": 0, "text": "partial"}
            }
        });
        model.apply_event("conversation.stream", &chunk);
        assert_eq!(model.live().previews.len(), 1);
        durable(
            &mut model,
            2,
            "assistant/message",
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "a-message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "final"}]
                },
                "stream": []
            }),
        );
        assert!(model.live().previews.is_empty());
        assert_eq!(model.items().len(), 1);
    }

    #[test]
    fn an_unknown_submission_reconciles_from_the_official_inbox() {
        let mut model = model();
        let plan = model.begin_submit("hello", Delivery::Queue).unwrap();
        model.submit_unknown(&plan.request_id, "timeout");
        let inbox = json!({
            "address": {"rootSessionId": "root"},
            "inbox": {
                "nextTurn": [{
                    "id": "m-1",
                    "role": "user",
                    "content": [{"type": "text", "text": "hello"}],
                    "source": {"kind": "user", "rpcId": plan.request_id}
                }],
                "nextStep": []
            }
        });
        model.apply_event("conversation.inbox", &inbox);
        assert!(model.submissions().unknowns().is_empty());
    }

    #[test]
    fn disconnect_marks_in_flight_submissions_unknown_and_drops_previews() {
        let mut model = model();
        model.begin_submit("hello", Delivery::Queue).unwrap();
        model.mark_disconnected();
        assert_eq!(model.live().echoes[0].state.label(), "未确认");
        assert!(model.previews().is_empty());
    }

    #[test]
    fn a_child_address_cannot_submit_in_s02() {
        let mut model = ConversationModel::new(SessionAddress::child(
            "root",
            "root",
            "child",
            crate::SubagentMode::Continuable,
        ));
        assert_eq!(model.begin_submit("x", Delivery::Queue), Err(SubmitError::ChildAddress));
    }

    #[test]
    fn an_event_for_another_address_does_not_change_state() {
        let mut model = model();
        let payload = json!({
            "address": {"rootSessionId": "other"},
            "status": "running"
        });
        assert!(!model.apply_event("conversation.status", &payload));
        assert_eq!(model.status(), crate::ConversationStatus::Idle);
        let _ = WireAddress { root_session_id: "root".into(), child_session_id: None };
    }
}
