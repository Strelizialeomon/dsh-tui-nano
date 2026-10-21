//! Approval card state.
//!
//! The card offers only the official closed outcomes. The default focus is
//! Reject; Enter therefore never grants one-shot permission by accident.
//! Closing the card maps to `cancelled` (fail closed), and a local timeout also
//! maps to `cancelled` because the official vocabulary has no `timeout`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::question::DecisionKey;

/// The official closed approval outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalOutcome {
    /// The only grant; applies to this request once.
    AllowedOnce,
    /// The user rejected the action.
    Rejected,
    /// The request was withdrawn, closed, or timed out locally.
    Cancelled,
    /// No answerer was available; fail closed.
    Unavailable,
}

impl ApprovalOutcome {
    /// Official wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }

    /// Parse one official outcome.
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "allowed-once" => Some(Self::AllowedOnce),
            "rejected" => Some(Self::Rejected),
            "cancelled" => Some(Self::Cancelled),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }

    /// Short user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowedOnce => "已允许一次",
            Self::Rejected => "已拒绝",
            Self::Cancelled => "已取消",
            Self::Unavailable => "不可用（按拒绝处理）",
        }
    }
}

/// Focused answer on a pending approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalFocus {
    /// Grant one-shot permission.
    Allow,
    /// Reject the action.
    Reject,
}

impl Default for ApprovalFocus {
    fn default() -> Self {
        Self::Reject
    }
}

/// Card lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalCardState {
    /// Waiting for an answer.
    Pending,
    /// The answer was handed to the bridge; not confirmed yet.
    Submitting,
    /// The official request settled with this outcome.
    Settled(ApprovalOutcome),
}

/// One action the app must send to the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAction {
    /// Answer the live request.
    Answer {
        /// The bridge-issued request identity.
        request_id: String,
        /// Official closed outcome.
        outcome: ApprovalOutcome,
    },
    /// Informational line for the status bar.
    Notice(String),
}

/// Local fallback timeout. The official approval seam has no timeout; a TUI
/// that keeps waiting forever after the user walked away would block the turn,
/// so S03 maps its own expiry to `cancelled` (fail closed).
pub const APPROVAL_LOCAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// One approval card.
#[derive(Debug, Clone)]
pub struct ApprovalCard {
    request_id: String,
    tool_name: String,
    reason: Option<String>,
    display_reason: BTreeMap<String, String>,
    call_id: Option<String>,
    command: Option<String>,
    focus: ApprovalFocus,
    state: ApprovalCardState,
    command_offset: usize,
    deadline: Option<Instant>,
    notice: Option<String>,
}

impl ApprovalCard {
    /// Build a pending card.
    pub fn new(
        request_id: String,
        tool_name: String,
        reason: Option<String>,
        display_reason: BTreeMap<String, String>,
        call_id: Option<String>,
        command: Option<String>,
        now: Instant,
    ) -> Self {
        Self {
            request_id,
            tool_name,
            reason,
            display_reason,
            call_id,
            command,
            focus: ApprovalFocus::default(),
            state: ApprovalCardState::Pending,
            command_offset: 0,
            deadline: Some(now + APPROVAL_LOCAL_TIMEOUT),
            notice: None,
        }
    }

    /// Bridge-issued request identity.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Tool name from the official request.
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Official reason, when supplied.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Localized display reason, preferring Chinese then English.
    pub fn display_reason(&self) -> Option<&str> {
        self.display_reason
            .get("zh")
            .or_else(|| self.display_reason.get("en"))
            .map(String::as_str)
    }

    /// Exact tool call the request belongs to.
    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }

    /// Linked command, only when the official call carries one.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// Focused answer.
    pub fn focus(&self) -> ApprovalFocus {
        self.focus
    }

    /// Lifecycle.
    pub fn state(&self) -> ApprovalCardState {
        self.state
    }

    /// Command scroll offset.
    pub fn command_offset(&self) -> usize {
        self.command_offset
    }

    /// Whether the card still accepts an answer.
    pub fn pending(&self) -> bool {
        self.state == ApprovalCardState::Pending
    }

    /// Short notice line.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Remaining local fallback time.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Mark the answer as handed to the bridge.
    pub fn mark_submitting(&mut self) {
        if self.state == ApprovalCardState::Pending {
            self.state = ApprovalCardState::Submitting;
        }
    }

    /// The bridge refused the answer; the request may still be pending.
    pub fn answer_failed(&mut self, message: impl Into<String>) {
        if self.state == ApprovalCardState::Submitting {
            self.state = ApprovalCardState::Pending;
        }
        self.notice = Some(message.into());
    }

    /// The official request settled with this outcome.
    pub fn settle(&mut self, outcome: ApprovalOutcome) {
        self.state = ApprovalCardState::Settled(outcome);
        self.deadline = None;
    }

    /// Local timeout: map to `cancelled`, never to an allow.
    pub fn tick(&mut self, now: Instant) -> Option<ApprovalAction> {
        if self.state != ApprovalCardState::Pending {
            return None;
        }
        let deadline = self.deadline?;
        if now < deadline {
            return None;
        }
        self.state = ApprovalCardState::Submitting;
        self.deadline = None;
        self.notice = Some("本地等待超时；按取消处理（不产生允许）".to_string());
        Some(ApprovalAction::Answer {
            request_id: self.request_id.clone(),
            outcome: ApprovalOutcome::Cancelled,
        })
    }

    /// One key press.
    pub fn handle_key(&mut self, key: DecisionKey) -> Option<ApprovalAction> {
        if !self.pending() {
            return None;
        }
        match key {
            DecisionKey::Up | DecisionKey::Down | DecisionKey::Tab | DecisionKey::BackTab => {
                self.focus = match self.focus {
                    ApprovalFocus::Allow => ApprovalFocus::Reject,
                    ApprovalFocus::Reject => ApprovalFocus::Allow,
                };
                None
            }
            DecisionKey::Char('1') => {
                self.focus = ApprovalFocus::Allow;
                None
            }
            DecisionKey::Char('2') => {
                self.focus = ApprovalFocus::Reject;
                None
            }
            DecisionKey::PageUp => {
                self.command_offset = self.command_offset.saturating_sub(4);
                None
            }
            DecisionKey::PageDown => {
                self.command_offset = self.command_offset.saturating_add(4);
                None
            }
            DecisionKey::Enter => Some(self.answer(match self.focus {
                ApprovalFocus::Allow => ApprovalOutcome::AllowedOnce,
                ApprovalFocus::Reject => ApprovalOutcome::Rejected,
            })),
            // Closing is not an allow; it withdraws the request.
            DecisionKey::Esc => Some(self.answer(ApprovalOutcome::Cancelled)),
            _ => None,
        }
    }

    /// Scroll the linked command region.
    pub fn scroll_command(&mut self, delta: isize) {
        if delta < 0 {
            self.command_offset = self.command_offset.saturating_sub(delta.unsigned_abs());
        } else {
            self.command_offset = self.command_offset.saturating_add(delta as usize);
        }
    }

    /// Public form of one explicit answer, used by pointer clicks.
    pub fn answer_outcome(&mut self, outcome: ApprovalOutcome) -> ApprovalAction {
        self.answer(outcome)
    }

    fn answer(&mut self, outcome: ApprovalOutcome) -> ApprovalAction {
        self.state = ApprovalCardState::Submitting;
        self.notice = Some(match outcome {
            ApprovalOutcome::AllowedOnce => "已提交「允许一次」，等待官方确认".to_string(),
            ApprovalOutcome::Rejected => "已提交「拒绝」".to_string(),
            ApprovalOutcome::Cancelled => "已取消本次审批（未允许）".to_string(),
            ApprovalOutcome::Unavailable => "审批不可用（未允许）".to_string(),
        });
        ApprovalAction::Answer {
            request_id: self.request_id.clone(),
            outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> ApprovalCard {
        ApprovalCard::new(
            "r-1".into(),
            "bash".into(),
            Some("needs wider sandbox".into()),
            BTreeMap::new(),
            Some("c-1".into()),
            Some("rm -rf build".into()),
            Instant::now(),
        )
    }

    #[test]
    fn default_focus_is_reject_and_enter_never_allows() {
        let mut card = card();
        assert_eq!(card.focus(), ApprovalFocus::Reject);
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        assert_eq!(
            action,
            ApprovalAction::Answer {
                request_id: "r-1".into(),
                outcome: ApprovalOutcome::Rejected,
            }
        );
    }

    #[test]
    fn allow_requires_an_explicit_focus_change() {
        let mut card = card();
        card.handle_key(DecisionKey::Up);
        assert_eq!(card.focus(), ApprovalFocus::Allow);
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        assert!(matches!(
            action,
            ApprovalAction::Answer {
                outcome: ApprovalOutcome::AllowedOnce,
                ..
            }
        ));
    }

    #[test]
    fn closing_maps_to_cancelled_not_allowed() {
        let mut card = card();
        let action = card.handle_key(DecisionKey::Esc).unwrap();
        assert!(matches!(
            action,
            ApprovalAction::Answer {
                outcome: ApprovalOutcome::Cancelled,
                ..
            }
        ));
    }

    #[test]
    fn a_local_timeout_maps_to_cancelled() {
        let mut card = card();
        let action = card
            .tick(Instant::now() + APPROVAL_LOCAL_TIMEOUT + Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            action,
            ApprovalAction::Answer {
                outcome: ApprovalOutcome::Cancelled,
                ..
            }
        ));
        assert!(!card.pending());
    }

    #[test]
    fn a_settled_request_can_not_be_answered_again() {
        let mut card = card();
        card.settle(ApprovalOutcome::Rejected);
        assert!(card.handle_key(DecisionKey::Enter).is_none());
    }

    #[test]
    fn the_localized_display_reason_wins_over_the_raw_reason() {
        let mut reasons = BTreeMap::new();
        reasons.insert("en".to_string(), "Ask to widen".to_string());
        reasons.insert("zh".to_string(), "请求放宽权限".to_string());
        let card = ApprovalCard::new(
            "r".into(),
            "bash".into(),
            Some("sandbox".into()),
            reasons,
            None,
            None,
            Instant::now(),
        );
        assert_eq!(card.display_reason(), Some("请求放宽权限"));
    }
}
