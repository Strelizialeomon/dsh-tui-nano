//! Address-owned decision state: which card owns the input seat, the queue of
//! later requests, the timed-question projection, and the actions the app must
//! send to the bridge.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use nano_conversation::SessionAddress;
use serde_json::Value;

use crate::approval::{ApprovalAction, ApprovalCard, ApprovalOutcome};
use crate::contract::{parse_answers, parse_questions, AnswerItem, Question};
use crate::question::{DecisionKey, QuestionAction, QuestionCard, TimedState};

/// One projected timed question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedQuestion {
    /// Tool-call identity.
    pub call_id: String,
    /// Official questions.
    pub questions: Vec<Question>,
    /// `open` or `continued`.
    pub state: String,
}

/// One settled timed question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledQuestion {
    /// Tool-call identity.
    pub call_id: String,
    /// Recorded answers.
    pub answers: Vec<AnswerItem>,
}

/// The official `userQuestions` projection, as delivered by the bridge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionProjection {
    /// Questions that can still take an answer.
    pub active: Vec<ProjectedQuestion>,
    /// Questions a reply settled.
    pub settled: Vec<SettledQuestion>,
}

/// One bridge call the app must send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionAction {
    /// Answer a live approval request.
    Approval {
        /// Bridge request identity.
        request_id: String,
        /// Official closed outcome.
        outcome: ApprovalOutcome,
    },
    /// Submit a question answer batch.
    QuestionAnswer {
        /// Live request identity, when present.
        request_id: Option<String>,
        /// Tool-call identity for the continued path.
        call_id: Option<String>,
        /// Official batch.
        answers: Vec<AnswerItem>,
    },
    /// Settle a timed foreground wait as timed out.
    QuestionExpire {
        /// Live request identity.
        request_id: Option<String>,
        /// Tool-call identity.
        call_id: Option<String>,
    },
    /// Cancel an unkeyed question wait.
    QuestionCancel {
        /// Live request identity.
        request_id: Option<String>,
    },
    /// Plan review: cancel the wait and return to the input seat.
    QuestionRequestChanges {
        /// Live request identity.
        request_id: Option<String>,
        /// Tool-call identity.
        call_id: Option<String>,
    },
}

/// One address's decision state.
#[derive(Debug, Clone)]
pub struct DecisionsModel {
    address: SessionAddress,
    approval: Option<ApprovalCard>,
    approval_queue: VecDeque<ApprovalCard>,
    questions: Vec<QuestionCard>,
    active_question: Option<usize>,
    read_only: Option<QuestionCard>,
    projection: QuestionProjection,
    actions: Vec<DecisionAction>,
    notice: Option<String>,
    error: Option<String>,
    last_countdown_secs: Option<u64>,
}

impl DecisionsModel {
    /// Build for one address.
    pub fn new(address: SessionAddress) -> Self {
        Self {
            address,
            approval: None,
            approval_queue: VecDeque::new(),
            questions: Vec::new(),
            active_question: None,
            read_only: None,
            projection: QuestionProjection::default(),
            actions: Vec::new(),
            notice: None,
            error: None,
            last_countdown_secs: None,
        }
    }

    /// Bound address.
    pub fn address(&self) -> &SessionAddress {
        &self.address
    }

    /// Current projection.
    pub fn projection(&self) -> &QuestionProjection {
        &self.projection
    }

    /// Active pending approval card.
    pub fn approval(&self) -> Option<&ApprovalCard> {
        self.approval.as_ref()
    }

    /// Currently editable question card.
    pub fn question(&self) -> Option<&QuestionCard> {
        self.active_question
            .and_then(|index| self.questions.get(index))
            .filter(|card| !card.hidden())
    }

    /// Read-only replay card.
    pub fn read_only(&self) -> Option<&QuestionCard> {
        self.read_only.as_ref().filter(|card| !card.hidden())
    }

    /// The card that owns the input seat, approval first.
    pub fn top(&self) -> Option<DecisionTop<'_>> {
        if let Some(approval) = &self.approval {
            return Some(DecisionTop::Approval(approval));
        }
        if let Some(card) = self.read_only() {
            return Some(DecisionTop::Question(card));
        }
        self.question().map(DecisionTop::Question)
    }

    /// Latest notice line.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Latest error line.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Clear transient status once the user acts elsewhere.
    pub fn clear_status(&mut self) {
        self.notice = None;
        self.error = None;
    }

    /// Clear only the informational notice.
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Drain bridge calls produced by key handling or countdown expiry.
    pub fn take_actions(&mut self) -> Vec<DecisionAction> {
        std::mem::take(&mut self.actions)
    }

    /// Apply one `nano.event` kind/payload. Returns whether display state
    /// changed.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        if !matches_address(payload, &self.address) {
            return false;
        }
        match kind {
            "decisions.approval" => self.apply_approval(payload),
            "decisions.question" => self.apply_question(payload),
            "decisions.projection" => self.apply_projection(payload),
            _ => false,
        }
    }

    /// One key press on the top card. Returns whether the screen changed.
    pub fn handle_key(&mut self, key: DecisionKey) -> bool {
        if self.approval.is_some() {
            let Some(card) = self.approval.as_mut() else {
                return false;
            };
            if let Some(action) = card.handle_key(key) {
                self.push_approval_action(action);
                return true;
            }
            return true;
        }
        if let Some(card) = self.read_only.as_mut() {
            match card.handle_key(key) {
                Some(QuestionAction::CloseReadOnly) => {
                    self.read_only = None;
                    return true;
                }
                Some(QuestionAction::Notice(message)) => {
                    self.notice = Some(message);
                    return true;
                }
                _ => return true,
            }
        }
        let Some(index) = self.active_question else {
            return false;
        };
        let Some(card) = self.questions.get_mut(index) else {
            return false;
        };
        if let Some(action) = card.handle_key(key) {
            self.apply_question_action(action);
        }
        self.ensure_active_question();
        true
    }

    /// Advance countdowns. Returns whether the screen needs a redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut dirty = false;
        if let Some(card) = self.approval.as_mut() {
            if let Some(action) = card.tick(now) {
                self.push_approval_action(action);
                dirty = true;
            }
        }
        // Every timed card keeps counting down, including a hidden card whose
        // request still owns a Host claim. Only the active card drives the
        // visible seconds indicator.
        let mut question_actions = Vec::new();
        for card in &mut self.questions {
            if let Some(action) = card.tick(now) {
                question_actions.push(action);
                dirty = true;
            }
        }
        for action in question_actions {
            self.apply_question_action(action);
        }
        if let Some(index) = self.active_question {
            let seconds = self
                .questions
                .get(index)
                .and_then(|card| card.timed().remaining_at(now))
                .map(|remaining| remaining.as_secs());
            if seconds != self.last_countdown_secs {
                self.last_countdown_secs = seconds;
                dirty = true;
            }
        }
        dirty
    }

    /// Reopen a hidden card, or build one from recorded facts.
    pub fn open_question(
        &mut self,
        call_id: String,
        questions: Vec<Question>,
        answers: Vec<AnswerItem>,
        answerable: bool,
        continued: bool,
    ) -> bool {
        if let Some(index) = self
            .questions
            .iter()
            .position(|card| card.call_id() == Some(call_id.as_str()))
        {
            if let Some(card) = self.questions.get_mut(index) {
                card.reveal();
                if continued {
                    card.mark_continued();
                }
            }
            self.active_question = Some(index);
            return true;
        }
        if answerable {
            if questions.is_empty() {
                return false;
            }
            let mut card = QuestionCard::new(None, Some(call_id), questions, None);
            if continued {
                card.mark_continued();
            }
            self.questions.push(card);
            self.active_question = Some(self.questions.len() - 1);
            return true;
        }
        self.open_read_only(Some(call_id), questions, answers)
    }

    /// Reopen or create a read-only question card from recorded facts.
    pub fn open_read_only(
        &mut self,
        call_id: Option<String>,
        questions: Vec<Question>,
        answers: Vec<AnswerItem>,
    ) -> bool {
        if let Some(call_id) = call_id.as_deref() {
            if let Some(index) = self
                .questions
                .iter()
                .position(|card| card.call_id() == Some(call_id))
            {
                if let Some(card) = self.questions.get_mut(index) {
                    card.reveal();
                }
                self.active_question = Some(index);
                return true;
            }
        }
        if questions.is_empty() {
            return false;
        }
        let mut card = QuestionCard::read_only(call_id, questions, answers);
        card.reveal();
        self.read_only = Some(card);
        true
    }

    /// Close the read-only card.
    pub fn close_read_only(&mut self) {
        self.read_only = None;
    }

    /// Apply the absolute decision snapshot read after a reconnect.
    pub fn apply_snapshot(&mut self, value: &Value) -> bool {
        if !matches_address(value, &self.address) {
            return false;
        }
        let mut approvals = Vec::new();
        if let Some(items) = value.get("approvals").and_then(Value::as_array) {
            for item in items {
                let Some(request_id) = item.get("requestId").and_then(Value::as_str) else {
                    continue;
                };
                if self
                    .approval
                    .as_ref()
                    .is_some_and(|card| card.request_id() == request_id)
                {
                    if let Some(card) = self.approval.take() {
                        approvals.push(card);
                    }
                } else if let Some(position) = self
                    .approval_queue
                    .iter()
                    .position(|card| card.request_id() == request_id)
                {
                    if let Some(card) = self.approval_queue.remove(position) {
                        approvals.push(card);
                    }
                } else {
                    let display_reason = item
                        .get("displayReason")
                        .and_then(Value::as_object)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|(key, value)| {
                                    Some((key.clone(), value.as_str()?.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    approvals.push(ApprovalCard::new(
                        request_id.to_string(),
                        item.get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string(),
                        item.get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        display_reason,
                        item.get("callId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        item.get("command")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        Instant::now(),
                    ));
                }
            }
        }
        self.approval = None;
        self.approval_queue.clear();
        let mut approvals = approvals.into_iter();
        self.approval = approvals.next();
        self.approval_queue.extend(approvals);

        let mut live_questions = Vec::new();
        if let Some(items) = value.get("questions").and_then(Value::as_array) {
            for item in items {
                let request_id = item
                    .get("requestId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let call_id = item
                    .get("callId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let existing = self.questions.iter().position(|card| {
                    (request_id.is_some() && card.request_id() == request_id.as_deref())
                        || (call_id.is_some() && card.call_id() == call_id.as_deref())
                });
                if let Some(index) = existing {
                    let mut card = self.questions.remove(index);
                    card.reveal();
                    if let Some(remaining) = item.get("remainingMs").and_then(Value::as_u64) {
                        card.set_remaining_ms(remaining, Instant::now());
                    }
                    live_questions.push(card);
                    continue;
                }
                let questions = parse_questions(item.get("questions"));
                if questions.is_empty() {
                    continue;
                }
                let timed = item
                    .get("remainingMs")
                    .and_then(Value::as_u64)
                    .map(|remaining| TimedState::start(remaining, Instant::now()));
                live_questions.push(QuestionCard::new(request_id, call_id, questions, timed));
            }
        }
        self.questions = live_questions;
        self.active_question = None;
        if let Some(projection) = value.get("projection") {
            self.apply_projection_value(projection);
        }
        self.ensure_active_question();
        true
    }

    /// One pointer click on the visible card.
    pub fn click(&mut self, click: DecisionClick) -> bool {
        if let Some(card) = self.approval.as_mut() {
            let outcome = match click {
                DecisionClick::ApprovalAllow => Some(ApprovalOutcome::AllowedOnce),
                DecisionClick::ApprovalReject => Some(ApprovalOutcome::Rejected),
                _ => None,
            };
            if let Some(outcome) = outcome {
                let action = card.answer_outcome(outcome);
                self.push_approval_action(action);
            }
            return true;
        }
        if self.read_only.is_some() {
            self.read_only = None;
            return true;
        }
        let Some(index) = self.active_question else {
            return false;
        };
        let Some(card) = self.questions.get_mut(index) else {
            return false;
        };
        let action = match click {
            DecisionClick::QuestionOption(option) => card.click_option(option),
            DecisionClick::QuestionOther => {
                card.click_other();
                None
            }
            DecisionClick::QuestionSubmit => card.click_submit(),
            DecisionClick::QuestionSkip => card.click_skip(),
            DecisionClick::QuestionPrev => {
                card.click_nav(false);
                None
            }
            DecisionClick::QuestionNext => {
                card.click_nav(true);
                None
            }
            DecisionClick::PlanView => card.click_plan(crate::question::PlanFocus::View),
            DecisionClick::PlanRequestChanges => {
                card.click_plan(crate::question::PlanFocus::RequestChanges)
            }
            DecisionClick::PlanApprove => card.click_plan(crate::question::PlanFocus::Approve),
            _ => None,
        };
        if let Some(action) = action {
            self.apply_question_action(action);
        }
        self.ensure_active_question();
        true
    }

    /// Paste text into an active question's Other field.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(index) = self.active_question else {
            return false;
        };
        let Some(card) = self.questions.get_mut(index) else {
            return false;
        };
        card.paste(text)
    }

    /// Scroll the active card's long detail or command region.
    pub fn scroll(&mut self, delta: isize) -> bool {
        if let Some(card) = self.approval.as_mut() {
            card.scroll_command(delta);
            return true;
        }
        if let Some(card) = self.read_only.as_mut() {
            card.scroll_detail(delta);
            return true;
        }
        if let Some(index) = self.active_question {
            if let Some(card) = self.questions.get_mut(index) {
                card.scroll_detail(delta);
                return true;
            }
        }
        false
    }

    /// Return a failed bridge answer to editing, keeping the request identity.
    pub fn action_failed(&mut self, action: &DecisionAction, message: impl Into<String>) {
        let message = message.into();
        match action {
            DecisionAction::Approval { request_id, .. } => {
                if let Some(card) = self.approval.as_mut() {
                    if card.request_id() == request_id {
                        card.answer_failed(message.clone());
                    }
                }
            }
            DecisionAction::QuestionAnswer {
                request_id,
                call_id,
                ..
            }
            | DecisionAction::QuestionExpire {
                request_id,
                call_id,
            }
            | DecisionAction::QuestionRequestChanges {
                request_id,
                call_id,
            } => {
                if let Some(card) = self.questions.iter_mut().find(|card| {
                    (request_id.is_some() && card.request_id() == request_id.as_deref())
                        || (call_id.is_some() && card.call_id() == call_id.as_deref())
                }) {
                    card.answer_failed(message.clone());
                }
            }
            DecisionAction::QuestionCancel { request_id } => {
                if let Some(card) = self
                    .questions
                    .iter_mut()
                    .find(|card| request_id.is_some() && card.request_id() == request_id.as_deref())
                {
                    card.answer_failed(message.clone());
                }
            }
        }
        self.error = Some(message);
    }

    /// Bind a different conversation address, dropping every old card.
    pub fn switch_address(&mut self, address: SessionAddress) {
        if self.address != address {
            self.address = address;
            self.clear();
        }
    }

    /// Forget every card on an address switch.
    pub fn clear(&mut self) {
        self.approval = None;
        self.approval_queue.clear();
        self.questions.clear();
        self.active_question = None;
        self.read_only = None;
        self.projection = QuestionProjection::default();
        self.actions.clear();
        self.notice = None;
        self.error = None;
        self.last_countdown_secs = None;
    }

    fn apply_approval(&mut self, payload: &Value) -> bool {
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if request_id.is_empty() {
            return false;
        }
        if state == "settled" {
            let outcome = payload
                .get("outcome")
                .and_then(Value::as_str)
                .and_then(ApprovalOutcome::from_str);
            let Some(outcome) = outcome else {
                return false;
            };
            let mut changed = false;
            let active_matches = self
                .approval
                .as_ref()
                .is_some_and(|card| card.request_id() == request_id);
            if active_matches {
                if let Some(card) = self.approval.as_mut() {
                    card.settle(outcome);
                }
                self.approval = None;
                changed = true;
            }
            if let Some(position) = self
                .approval_queue
                .iter()
                .position(|card| card.request_id() == request_id)
            {
                if let Some(mut card) = self.approval_queue.remove(position) {
                    card.settle(outcome);
                }
                changed = true;
            }
            if changed {
                self.promote_approval();
                self.notice = Some(format!("审批结果：{}", outcome.label()));
            }
            return changed;
        }
        if self
            .approval
            .as_ref()
            .is_some_and(|card| card.request_id() == request_id)
            || self
                .approval_queue
                .iter()
                .any(|card| card.request_id() == request_id)
        {
            return false;
        }
        let tool_name = payload
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let reason = payload
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let display_reason = payload
            .get("displayReason")
            .and_then(Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let call_id = payload
            .get("callId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let command = payload
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string);
        let card = ApprovalCard::new(
            request_id,
            tool_name,
            reason,
            display_reason,
            call_id,
            command,
            Instant::now(),
        );
        if self.approval.is_none() {
            self.approval = Some(card);
        } else {
            self.approval_queue.push_back(card);
        }
        true
    }

    fn apply_question(&mut self, payload: &Value) -> bool {
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let call_id = payload
            .get("callId")
            .and_then(Value::as_str)
            .map(str::to_string);
        match state {
            "pending" => {
                if request_id.is_none() && call_id.is_none() {
                    return false;
                }
                if let Some(index) = self.questions.iter().position(|card| {
                    card.request_id() == request_id.as_deref()
                        || (call_id.is_some() && card.call_id() == call_id.as_deref())
                }) {
                    if let Some(remaining) = payload.get("remainingMs").and_then(Value::as_u64) {
                        if let Some(card) = self.questions.get_mut(index) {
                            card.set_remaining_ms(remaining, Instant::now());
                        }
                        return true;
                    }
                    return false;
                }
                let questions = parse_questions(payload.get("questions"));
                if questions.is_empty() {
                    return false;
                }
                let timed = payload
                    .get("remainingMs")
                    .and_then(Value::as_u64)
                    .map(|remaining| TimedState::start(remaining, Instant::now()))
                    .filter(|_| payload.get("timed").and_then(Value::as_bool) != Some(false));
                let card = QuestionCard::new(request_id, call_id, questions, timed);
                self.questions.push(card);
                self.ensure_active_question();
                true
            }
            "settled" => {
                let answers = parse_answers(payload.get("answers")).unwrap_or_default();
                let index = self.questions.iter().position(|card| {
                    card.request_id() == request_id.as_deref() && request_id.is_some()
                        || call_id.is_some() && card.call_id() == call_id.as_deref()
                });
                if let Some(index) = index {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.settle(&answers);
                    }
                    self.ensure_active_question();
                    self.notice = Some("答卷已提交".to_string());
                    return true;
                }
                false
            }
            "submitted" => {
                let index = self.questions.iter().position(|card| {
                    card.request_id() == request_id.as_deref() && request_id.is_some()
                        || call_id.is_some() && card.call_id() == call_id.as_deref()
                });
                if let Some(index) = index {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.mark_submitting();
                    }
                    self.notice = Some("答卷已提交，等待确认".to_string());
                    return true;
                }
                false
            }
            "expired" => {
                let index = self.questions.iter().position(|card| {
                    card.request_id() == request_id.as_deref() && request_id.is_some()
                        || call_id.is_some() && card.call_id() == call_id.as_deref()
                });
                if let Some(index) = index {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.mark_continued();
                    }
                    return true;
                }
                false
            }
            "cancelled" => {
                let index = self.questions.iter().position(|card| {
                    card.request_id() == request_id.as_deref() && request_id.is_some()
                        || call_id.is_some() && card.call_id() == call_id.as_deref()
                });
                if let Some(index) = index {
                    self.questions.remove(index);
                    self.ensure_active_question();
                    self.notice = Some("问卷已取消".to_string());
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn apply_projection(&mut self, payload: &Value) -> bool {
        let changed = self.apply_projection_value(payload);
        self.ensure_active_question();
        changed
    }

    fn apply_projection_value(&mut self, payload: &Value) -> bool {
        let mut projection = QuestionProjection::default();
        if let Some(active) = payload.get("active").and_then(Value::as_array) {
            for entry in active {
                let Some(call_id) = entry.get("callId").and_then(Value::as_str) else {
                    continue;
                };
                let questions = parse_questions(entry.get("questions"));
                projection.active.push(ProjectedQuestion {
                    call_id: call_id.to_string(),
                    questions,
                    state: entry
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("open")
                        .to_string(),
                });
            }
        }
        if let Some(settled) = payload.get("settled").and_then(Value::as_array) {
            for entry in settled {
                let Some(call_id) = entry.get("callId").and_then(Value::as_str) else {
                    continue;
                };
                projection.settled.push(SettledQuestion {
                    call_id: call_id.to_string(),
                    answers: parse_answers(entry.get("answers")).unwrap_or_default(),
                });
            }
        }
        let changed = projection != self.projection;
        self.projection = projection;
        for projected in &self.projection.active {
            if projected.state == "continued" {
                if let Some(index) = self
                    .questions
                    .iter()
                    .position(|card| card.call_id() == Some(projected.call_id.as_str()))
                {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.mark_continued();
                    }
                }
            }
        }
        for settled in &self.projection.settled {
            if let Some(index) = self
                .questions
                .iter()
                .position(|card| card.call_id() == Some(settled.call_id.as_str()))
            {
                if let Some(card) = self.questions.get_mut(index) {
                    card.settle(&settled.answers);
                }
            }
        }
        changed
    }

    fn apply_question_action(&mut self, action: QuestionAction) {
        match action {
            QuestionAction::Answer {
                request_id,
                call_id,
                answers,
            } => {
                if let Some(index) = self.active_question {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.mark_submitting();
                    }
                }
                self.actions.push(DecisionAction::QuestionAnswer {
                    request_id,
                    call_id,
                    answers,
                });
            }
            QuestionAction::Expire {
                request_id,
                call_id,
            } => {
                self.actions.push(DecisionAction::QuestionExpire {
                    request_id,
                    call_id,
                });
            }
            QuestionAction::Cancel { request_id } => {
                self.actions
                    .push(DecisionAction::QuestionCancel { request_id });
                if let Some(index) = self.active_question.take() {
                    self.questions.remove(index);
                }
                self.ensure_active_question();
            }
            QuestionAction::RequestChanges {
                request_id,
                call_id,
            } => {
                if let Some(index) = self.active_question {
                    if let Some(card) = self.questions.get_mut(index) {
                        card.settle(&[]);
                    }
                    self.questions.remove(index);
                }
                self.ensure_active_question();
                self.actions.push(DecisionAction::QuestionRequestChanges {
                    request_id,
                    call_id,
                });
                self.notice = Some("已请求修改；请在输入框说明要改什么（未提交批准）".to_string());
            }
            QuestionAction::Hide => {
                self.notice = Some("问卷已收起；可从工具行重新打开".to_string());
                self.ensure_active_question();
            }
            QuestionAction::CloseReadOnly => {
                self.read_only = None;
            }
            QuestionAction::Notice(message) => {
                self.notice = Some(message);
            }
        }
    }

    fn push_approval_action(&mut self, action: ApprovalAction) {
        match action {
            ApprovalAction::Answer {
                request_id,
                outcome,
            } => {
                self.actions.push(DecisionAction::Approval {
                    request_id,
                    outcome,
                });
            }
            ApprovalAction::Notice(message) => {
                self.notice = Some(message);
            }
        }
    }

    fn promote_approval(&mut self) {
        if self.approval.is_none() {
            if let Some(card) = self.approval_queue.front().cloned() {
                self.approval_queue.pop_front();
                self.approval = Some(card);
            }
        }
    }

    fn ensure_active_question(&mut self) {
        let Some(index) = self.active_question else {
            self.active_question = self
                .questions
                .iter()
                .position(|card| !card.hidden() && card.state().interactive());
            return;
        };
        let still_active = self
            .questions
            .get(index)
            .is_some_and(|card| !card.hidden() && card.state().interactive());
        if !still_active {
            self.active_question = self
                .questions
                .iter()
                .position(|card| !card.hidden() && card.state().interactive());
        }
    }
}

/// One pointer target on the visible decision card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionClick {
    /// Grant one-shot permission.
    ApprovalAllow,
    /// Reject the action.
    ApprovalReject,
    /// Choose one question option.
    QuestionOption(usize),
    /// Focus the Other row.
    QuestionOther,
    /// Submit or confirm the current page.
    QuestionSubmit,
    /// Skip the current question.
    QuestionSkip,
    /// Previous question page.
    QuestionPrev,
    /// Next question page.
    QuestionNext,
    /// Expand the full plan.
    PlanView,
    /// Request changes (cancel the wait).
    PlanRequestChanges,
    /// Approve the plan.
    PlanApprove,
}

/// The card currently owning the input seat.
#[derive(Debug, Clone, Copy)]
pub enum DecisionTop<'a> {
    /// Approval card.
    Approval(&'a ApprovalCard),
    /// Question card.
    Question(&'a QuestionCard),
}

impl<'a> DecisionTop<'a> {
    /// Whether this is an approval card.
    pub fn is_approval(&self) -> bool {
        matches!(self, Self::Approval(_))
    }
}

fn matches_address(payload: &Value, address: &SessionAddress) -> bool {
    let Some(value) = payload.get("address") else {
        return false;
    };
    let Some(root) = value.get("rootSessionId").and_then(Value::as_str) else {
        return false;
    };
    if root != address.root_session_id {
        return false;
    }
    match (
        value.get("childSessionId").and_then(Value::as_str),
        address.child.as_ref(),
    ) {
        (None, None) => true,
        (Some(child), Some(current)) => child == current.child_session_id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn root() -> SessionAddress {
        SessionAddress::root("root")
    }

    fn model() -> DecisionsModel {
        DecisionsModel::new(root())
    }

    fn question_event(request_id: &str, call_id: Option<&str>) -> Value {
        json!({
            "address": {"rootSessionId": "root"},
            "state": "pending",
            "requestId": request_id,
            "callId": call_id,
            "timed": false,
            "questions": [{
                "id": "q1",
                "question": "Pick",
                "options": [{"label": "A"}, {"label": "B"}]
            }]
        })
    }

    #[test]
    fn a_question_takes_the_input_seat_and_answers_by_request_id() {
        let mut model = model();
        assert!(model.apply_event("decisions.question", &question_event("r-1", None)));
        assert!(model.top().is_some());
        let action = model
            .handle_key(DecisionKey::Enter)
            .then(|| model.take_actions())
            .unwrap();
        assert!(matches!(
            action.as_slice(),
            [DecisionAction::QuestionAnswer { request_id: Some(id), answers, .. }] if id == "r-1" && answers[0].selected == vec!["A"]
        ));
    }

    #[test]
    fn events_for_another_address_are_ignored() {
        let mut model = model();
        let payload = json!({
            "address": {"rootSessionId": "other"},
            "state": "pending",
            "requestId": "r-1",
            "questions": [{"id": "q", "question": "Q"}]
        });
        assert!(!model.apply_event("decisions.question", &payload));
        assert!(model.top().is_none());
    }

    #[test]
    fn an_approval_card_is_preferred_over_a_question() {
        let mut model = model();
        model.apply_event("decisions.question", &question_event("r-1", None));
        model.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        assert!(model.top().unwrap().is_approval());
    }

    #[test]
    fn later_questions_wait_for_the_active_one() {
        let mut model = model();
        model.apply_event("decisions.question", &question_event("r-1", None));
        model.apply_event("decisions.question", &question_event("r-2", None));
        let active = model.question().unwrap();
        assert_eq!(active.request_id(), Some("r-1"));
        model.handle_key(DecisionKey::Enter);
        model.take_actions();
        // The first card was submitted; the second becomes active.
        assert_eq!(model.question().unwrap().request_id(), Some("r-2"));
    }

    #[test]
    fn a_settled_projection_closes_the_editable_card() {
        let mut model = model();
        model.apply_event("decisions.question", &question_event("r-1", Some("c-1")));
        model.apply_event(
            "decisions.projection",
            &json!({
                "address": {"rootSessionId": "root"},
                "active": [],
                "settled": [{"callId": "c-1", "answers": [{"id": "q1", "selected": ["A"]}]}]
            }),
        );
        assert!(model.question().is_none());
    }

    #[test]
    fn a_reconnect_snapshot_preserves_an_in_progress_question_draft() {
        let mut model = model();
        model.apply_event("decisions.question", &question_event("r-1", Some("c-1")));
        model.handle_key(DecisionKey::Down);
        model.handle_key(DecisionKey::Down);
        model.handle_key(DecisionKey::Char('x'));
        let snapshot = json!({
            "address": {"rootSessionId": "root"},
            "approvals": [],
            "questions": [{
                "requestId": "r-1",
                "callId": "c-1",
                "timed": true,
                "remainingMs": 30_000,
                "questions": [{
                    "id": "q1", "question": "Pick",
                    "options": [{"label": "A"}, {"label": "B"}]
                }]
            }],
            "projection": {"active": [], "settled": []}
        });
        assert!(model.apply_snapshot(&snapshot));
        let card = model.question().unwrap();
        assert_eq!(card.draft(0).unwrap().custom, "x");
        assert!(card.timed().remaining_ms.is_some());
    }

    #[test]
    fn switching_address_drops_every_old_card_and_projection() {
        let mut model = model();
        model.apply_event("decisions.question", &question_event("r-1", Some("c-1")));
        model.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        model.switch_address(SessionAddress::root("other"));
        assert!(model.top().is_none());
        assert!(model.projection().active.is_empty());
        assert!(model.projection().settled.is_empty());
    }

    #[test]
    fn a_read_only_card_can_be_reopened_by_call_id() {
        let mut model = model();
        let questions = parse_questions(Some(&json!([
            {"id": "q1", "question": "Q", "options": [{"label": "A"}]}
        ])));
        assert!(model.open_read_only(
            Some("c-1".into()),
            questions,
            vec![AnswerItem {
                id: "q1".into(),
                selected: vec!["A".into()],
                custom: None
            }]
        ));
        assert!(model.read_only().is_some());
        model.handle_key(DecisionKey::Esc);
        assert!(model.read_only().is_none());
    }

    #[test]
    fn a_countdown_expiry_emits_one_expire_action() {
        let mut model = model();
        let payload = json!({
            "address": {"rootSessionId": "root"},
            "state": "pending",
            "requestId": "r-1",
            "callId": "c-1",
            "timed": true,
            "remainingMs": 10,
            "questions": [{"id": "q1", "question": "Q", "options": [{"label": "A"}]}]
        });
        model.apply_event("decisions.question", &payload);
        let actions = model.tick(Instant::now() + Duration::from_millis(50));
        assert!(actions);
        let actions = model.take_actions();
        assert!(matches!(
            actions.as_slice(),
            [DecisionAction::QuestionExpire { request_id: Some(id), call_id: Some(call) }] if id == "r-1" && call == "c-1"
        ));
        assert!(!model.tick(Instant::now() + Duration::from_millis(100)));
    }

    #[test]
    fn a_withdrawn_approval_cannot_be_answered_later() {
        let mut model = model();
        model.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "a-1",
                "toolName": "bash"
            }),
        );
        model.apply_event(
            "decisions.approval",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "settled",
                "requestId": "a-1",
                "toolName": "bash",
                "outcome": "cancelled"
            }),
        );
        assert!(model.approval().is_none());
        assert!(model.top().is_none());
    }

    #[test]
    fn plan_review_defaults_to_request_changes_and_does_not_approve_on_enter() {
        let mut model = model();
        model.apply_event(
            "decisions.question",
            &json!({
                "address": {"rootSessionId": "root"},
                "state": "pending",
                "requestId": "r-1",
                "callId": "c-1",
                "questions": [{
                    "id": "plan",
                    "question": "Review plan",
                    "detail": "# Plan",
                    "intent": {"kind": "plan-review", "approve": "Approve"},
                    "options": [{"label": "Approve"}, {"label": "Request changes"}]
                }]
            }),
        );
        model.handle_key(DecisionKey::Enter);
        let actions = model.take_actions();
        assert!(matches!(
            actions.as_slice(),
            [DecisionAction::QuestionRequestChanges { .. }]
        ));
    }
}
