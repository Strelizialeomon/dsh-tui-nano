//! Submission identity and the local echo lifecycle.
//!
//! An echo is display-only. It is never a history source and it disappears as
//! soon as the durable `user/message` with the same `rpcId` arrives. A request
//! whose result is unknown is never resent with a new id.

use std::collections::BTreeMap;

use crate::history::ConversationHistory;
use crate::queue::InboxView;

/// Official delivery semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// An ordinary next-turn prompt (`queue` / `followup`).
    Queue,
    /// Next-step input while the turn is running (`steer`).
    Steer,
}

impl Delivery {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queue => "排队",
            Self::Steer => "插话",
        }
    }
}

/// What the UI may claim about one submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionState {
    /// The request is in flight; no official receipt yet.
    Submitting,
    /// The Agent accepted the message into its inbox.
    Accepted {
        /// The same requestId was already accepted; no second message exists.
        duplicate: bool,
    },
    /// The result is unknown (timeout, disconnect). Query the official facts.
    Unknown {
        /// Why the result is unknown.
        detail: String,
    },
    /// The host explicitly failed the request.
    Failed {
        /// Sanitized reason.
        detail: String,
    },
}

impl SubmissionState {
    /// User-facing label that never claims more than the official fact.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Submitting => "提交中",
            Self::Accepted { .. } => "已接收",
            Self::Unknown { .. } => "未确认",
            Self::Failed { .. } => "未发送",
        }
    }
}

/// One local submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// Business identity; never regenerated.
    pub request_id: String,
    /// Official message id, once known.
    pub message_id: Option<String>,
    /// Original text snapshot.
    pub text: String,
    /// Delivery mode used for this request.
    pub delivery: Delivery,
    /// Current state.
    pub state: SubmissionState,
    /// Whether the durable message (or a failure) retired the echo.
    pub settled: bool,
}

/// Read-only echo projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoView {
    /// Business identity.
    pub request_id: String,
    /// Original text.
    pub text: String,
    /// Delivery.
    pub delivery: Delivery,
    /// State.
    pub state: SubmissionState,
}

/// Bounded submission tracker.
#[derive(Debug, Clone)]
pub struct SubmissionTracker {
    order: Vec<String>,
    by_id: BTreeMap<String, Submission>,
    max: usize,
}

impl Default for SubmissionTracker {
    fn default() -> Self {
        Self::new(128)
    }
}

impl SubmissionTracker {
    /// Build with an explicit bound.
    pub fn new(max: usize) -> Self {
        Self {
            order: Vec::new(),
            by_id: BTreeMap::new(),
            max,
        }
    }

    /// Record a new in-flight submission and return its echo.
    pub fn begin(
        &mut self,
        request_id: impl Into<String>,
        text: impl Into<String>,
        delivery: Delivery,
    ) -> EchoView {
        let request_id = request_id.into();
        let submission = Submission {
            request_id: request_id.clone(),
            message_id: None,
            text: text.into(),
            delivery,
            state: SubmissionState::Submitting,
            settled: false,
        };
        self.by_id.insert(request_id.clone(), submission.clone());
        self.order.push(request_id);
        self.trim();
        echo_of(&submission)
    }

    /// The host accepted the submission.
    pub fn accepted(&mut self, request_id: &str, message_id: &str, duplicate: bool) {
        if let Some(submission) = self.by_id.get_mut(request_id) {
            submission.message_id = Some(message_id.to_string());
            submission.state = SubmissionState::Accepted { duplicate };
        }
    }

    /// The host explicitly failed the submission.
    pub fn failed(&mut self, request_id: &str, detail: impl Into<String>) {
        if let Some(submission) = self.by_id.get_mut(request_id) {
            submission.state = SubmissionState::Failed {
                detail: detail.into(),
            };
            submission.settled = true;
        }
    }

    /// The result never arrived.
    pub fn unknown(&mut self, request_id: &str, detail: impl Into<String>) {
        if let Some(submission) = self.by_id.get_mut(request_id) {
            submission.state = SubmissionState::Unknown {
                detail: detail.into(),
            };
        }
    }

    /// Retire the echo when the durable message with this `rpcId` arrives.
    pub fn durable_user_message(&mut self, rpc_id: &str) {
        if let Some(submission) = self.by_id.get_mut(rpc_id) {
            submission.settled = true;
        }
    }

    /// Look up one submission.
    pub fn get(&self, request_id: &str) -> Option<&Submission> {
        self.by_id.get(request_id)
    }

    /// Visible echoes in submission order.
    pub fn echoes(&self) -> Vec<EchoView> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .filter(|submission| !submission.settled)
            .map(echo_of)
            .collect()
    }

    /// Unknown submissions that still need an official fact check.
    pub fn unknowns(&self) -> Vec<&Submission> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .filter(|submission| matches!(submission.state, SubmissionState::Unknown { .. }))
            .collect()
    }

    /// Reconcile unknown submissions against official pending and durable facts.
    /// A found message becomes accepted; a missing one stays unknown.
    pub fn reconcile(&mut self, inbox: &InboxView, history: &ConversationHistory) {
        let ids: Vec<String> = self
            .unknowns()
            .into_iter()
            .map(|submission| submission.request_id.clone())
            .collect();
        for request_id in ids {
            let found = inbox
                .find_by_request(&request_id)
                .map(|item| item.id.clone())
                .or_else(|| {
                    history
                        .find_request(&request_id)
                        .and_then(|message| message.message_id.clone())
                });
            if let Some(message_id) = found {
                self.accepted(&request_id, &message_id, false);
                self.durable_user_message(&request_id);
            }
        }
    }

    /// A turn reached idle while an accepted message never became durable:
    /// mark the echo unknown instead of leaving it as a permanent "accepted"
    /// bubble. Returns how many echoes changed.
    pub fn retire_missing(&mut self, inbox: &InboxView, history: &ConversationHistory) -> usize {
        let ids: Vec<String> = self
            .order
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .filter(|submission| {
                !submission.settled && matches!(submission.state, SubmissionState::Accepted { .. })
            })
            .map(|submission| submission.request_id.clone())
            .collect();
        let mut changed = 0;
        for request_id in ids {
            let Some(submission) = self.by_id.get(&request_id) else {
                continue;
            };
            let in_inbox = submission
                .message_id
                .as_ref()
                .is_some_and(|message_id| inbox.find(message_id).is_some())
                || inbox.find_by_request(&request_id).is_some();
            let in_history = history.find_request(&request_id).is_some();
            if in_inbox || in_history {
                continue;
            }
            if let Some(submission) = self.by_id.get_mut(&request_id) {
                submission.state = SubmissionState::Unknown {
                    detail: "回合结束前未落盘".to_string(),
                };
                changed += 1;
            }
        }
        changed
    }

    fn trim(&mut self) {
        while self.order.len() > self.max {
            if let Some(id) = self.order.first().cloned() {
                self.order.remove(0);
                self.by_id.remove(&id);
            } else {
                break;
            }
        }
    }
}

fn echo_of(submission: &Submission) -> EchoView {
    EchoView {
        request_id: submission.request_id.clone(),
        text: submission.text.clone(),
        delivery: submission.delivery,
        state: submission.state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SessionEvent;
    use crate::events::WireAddress;
    use serde_json::json;

    #[test]
    fn an_accepted_echo_is_retired_by_the_durable_message() {
        let mut tracker = SubmissionTracker::default();
        tracker.begin("r-1", "hi", Delivery::Queue);
        tracker.accepted("r-1", "m-1", false);
        assert_eq!(tracker.echoes().len(), 1);
        tracker.durable_user_message("r-1");
        assert!(tracker.echoes().is_empty());
    }

    #[test]
    fn a_failed_echo_is_removed_but_unknown_stays_visible() {
        let mut tracker = SubmissionTracker::default();
        tracker.begin("r-1", "hi", Delivery::Queue);
        tracker.failed("r-1", "offline");
        assert!(tracker.echoes().is_empty());
        tracker.begin("r-2", "hi", Delivery::Queue);
        tracker.unknown("r-2", "timeout");
        let echoes = tracker.echoes();
        assert_eq!(echoes.len(), 1);
        assert_eq!(echoes[0].state.label(), "未确认");
        assert_eq!(tracker.unknowns().len(), 1);
    }

    #[test]
    fn an_accepted_message_that_never_lands_becomes_unknown_at_idle() {
        let mut tracker = SubmissionTracker::default();
        tracker.begin("r-1", "hi", Delivery::Queue);
        tracker.accepted("r-1", "m-1", false);
        let changed =
            tracker.retire_missing(&InboxView::default(), &ConversationHistory::default());
        assert_eq!(changed, 1);
        assert_eq!(tracker.echoes()[0].state.label(), "未确认");
    }

    #[test]
    fn reconciliation_uses_the_official_history_not_a_resend() {
        let mut tracker = SubmissionTracker::default();
        tracker.begin("r-1", "hi", Delivery::Queue);
        tracker.unknown("r-1", "timeout");
        let mut history = ConversationHistory::default();
        history.apply(&SessionEvent {
            address: WireAddress {
                root_session_id: "root".into(),
                child_session_id: None,
                parent_session_id: None,
                mode: None,
            },
            seq: 1,
            event_type: "user/message".into(),
            data: json!({
                "id": "m-1",
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
                "source": {"kind": "user", "rpcId": "r-1"}
            }),
        });
        tracker.reconcile(&InboxView::default(), &history);
        assert!(tracker.unknowns().is_empty());
        assert!(tracker.echoes().is_empty());
    }
}
