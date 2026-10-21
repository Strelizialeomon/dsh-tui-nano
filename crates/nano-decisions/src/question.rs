//! Question card state: single/multi select, Other, paging, batch submit,
//! Skip, timed countdown, continued answers, plan review, and read-only replay.
//!
//! The card owns only local draft and focus state. Answers always use the
//! official `{ id, selected, custom? }` batch shape and are handed to the
//! caller as an action; the card never invents a result the bridge cannot send.

use std::time::{Duration, Instant};

use crate::contract::{
    answers_to_wire, validate_answer_batch, AnswerItem, PlanReviewIntent, Question,
};

/// Keys the card understands. The app maps terminal keys to this vocabulary so
/// the state machine stays independent of the terminal backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKey {
    /// Move focus up.
    Up,
    /// Move focus down.
    Down,
    /// Previous question page.
    Left,
    /// Next question page.
    Right,
    /// Next page / next field.
    Tab,
    /// Previous page / previous field.
    BackTab,
    /// Confirm the focused choice, commit Other, or submit the batch.
    Enter,
    /// Close the card.
    Esc,
    /// Toggle a multi-select choice.
    Space,
    /// Delete one character in the Other field.
    Backspace,
    /// Insert one character into the Other field.
    Char(char),
    /// Skip the current question with the official empty result.
    Skip,
    /// Stop the countdown / hold the question indefinitely.
    MoreTime,
    /// Scroll up inside a long detail or command region.
    PageUp,
    /// Scroll down inside a long detail or command region.
    PageDown,
}

/// One action the app must send to the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAction {
    /// Submit one answer batch.
    Answer {
        /// Live request identity, when this card came from a live request.
        request_id: Option<String>,
        /// Tool-call identity for the continued path.
        call_id: Option<String>,
        /// The official batch.
        answers: Vec<AnswerItem>,
    },
    /// The local countdown reached zero; settle the foreground wait as timed
    /// out. The question remains answerable through `call_id`.
    Expire {
        /// Live request identity.
        request_id: Option<String>,
        /// Tool-call identity.
        call_id: Option<String>,
    },
    /// Close an unkeyed request, which cancels the wait.
    Cancel {
        /// Live request identity.
        request_id: Option<String>,
    },
    /// Plan review: cancel the wait and return the user to the input seat.
    RequestChanges {
        /// Live request identity.
        request_id: Option<String>,
        /// Tool-call identity.
        call_id: Option<String>,
    },
    /// Hide a tool-call-keyed card without answering.
    Hide,
    /// Dismiss a read-only card.
    CloseReadOnly,
    /// Informational line for the status bar.
    Notice(String),
}

/// Card lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionCardState {
    /// Editable draft.
    Draft,
    /// An answer was handed to the bridge; not confirmed yet.
    Submitting,
    /// The official projection or bridge settled the question.
    Settled,
    /// Read-only replay with recorded answers.
    ReadOnly,
    /// The local timer expired; the question is answerable through the
    /// continued path.
    Continued,
}

impl QuestionCardState {
    /// Whether keys may edit the card.
    pub fn interactive(self) -> bool {
        matches!(self, Self::Draft | Self::Continued)
    }
}

/// Local countdown state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimedState {
    /// Remaining duration reported by the Host when the card claimed the wait.
    pub remaining_ms: Option<u64>,
    /// Local deadline while the countdown runs.
    pub deadline: Option<Instant>,
    /// The user froze the countdown.
    pub indefinite: bool,
    /// The foreground wait already expired; answers use the continued path.
    pub continued: bool,
}

impl TimedState {
    /// Start a countdown from the Host-claimed remaining duration.
    pub fn start(remaining_ms: u64, now: Instant) -> Self {
        Self {
            remaining_ms: Some(remaining_ms),
            deadline: Some(now + Duration::from_millis(remaining_ms)),
            indefinite: false,
            continued: false,
        }
    }

    /// Remaining duration at `now`, for display.
    pub fn remaining_at(&self, now: Instant) -> Option<Duration> {
        if self.continued || self.indefinite {
            return None;
        }
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Stop the countdown because the user interacted or asked for more time.
    pub fn hold(&mut self) {
        self.indefinite = true;
        self.deadline = None;
    }
}

/// One question's draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// Selected option labels.
    pub selected: Vec<String>,
    /// Other text.
    pub custom: String,
    /// Caret position in the Other text, in characters.
    pub caret: usize,
}

impl Draft {
    fn answered(&self) -> bool {
        !self.selected.is_empty() || !self.custom.trim().is_empty()
    }

    fn to_answer(&self, question: &Question) -> AnswerItem {
        AnswerItem {
            id: question.id.clone(),
            selected: self.selected.clone(),
            custom: Some(self.custom.clone()).filter(|text| !text.trim().is_empty()),
        }
    }
}

/// One editable question card.
#[derive(Debug, Clone)]
pub struct QuestionCard {
    request_id: Option<String>,
    call_id: Option<String>,
    questions: Vec<Question>,
    drafts: Vec<Draft>,
    skipped: Vec<bool>,
    page: usize,
    focus: usize,
    error: Option<String>,
    notice: Option<String>,
    state: QuestionCardState,
    timed: TimedState,
    plan_expanded: bool,
    detail_offset: usize,
    review_offset: usize,
    hidden: bool,
    plan_focus: PlanFocus,
}

/// Focus order on a plan-review card. The default is the non-approving action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFocus {
    /// Expand the full plan.
    View,
    /// Cancel the wait and return to the input seat for change feedback.
    RequestChanges,
    /// Answer with the asker's approval label.
    Approve,
}

impl Default for PlanFocus {
    fn default() -> Self {
        Self::RequestChanges
    }
}

impl QuestionCard {
    /// Build a live card.
    pub fn new(
        request_id: Option<String>,
        call_id: Option<String>,
        questions: Vec<Question>,
        timed: Option<TimedState>,
    ) -> Self {
        let drafts = vec![Draft::default(); questions.len()];
        let skipped = vec![false; questions.len()];
        Self {
            request_id,
            call_id,
            questions,
            drafts,
            skipped,
            page: 0,
            focus: 0,
            error: None,
            notice: None,
            state: QuestionCardState::Draft,
            timed: timed.unwrap_or_default(),
            plan_expanded: false,
            detail_offset: 0,
            review_offset: 0,
            hidden: false,
            plan_focus: PlanFocus::default(),
        }
    }

    /// Build a read-only card from a recorded call and its answers.
    pub fn read_only(
        call_id: Option<String>,
        questions: Vec<Question>,
        answers: Vec<AnswerItem>,
    ) -> Self {
        let drafts = questions
            .iter()
            .map(|question| {
                let answer = answers.iter().find(|answer| answer.id == question.id);
                Draft {
                    selected: answer
                        .map(|answer| answer.selected.clone())
                        .unwrap_or_default(),
                    custom: answer
                        .and_then(|answer| answer.custom.clone())
                        .unwrap_or_default(),
                    caret: 0,
                }
            })
            .collect();
        let skipped = questions
            .iter()
            .map(|question| {
                answers
                    .iter()
                    .find(|answer| answer.id == question.id)
                    .map(|answer| !answer.answered())
                    .unwrap_or(true)
            })
            .collect();
        Self {
            request_id: None,
            call_id,
            questions,
            drafts,
            skipped,
            page: 0,
            focus: 0,
            error: None,
            notice: None,
            state: QuestionCardState::ReadOnly,
            timed: TimedState::default(),
            plan_expanded: false,
            detail_offset: 0,
            review_offset: 0,
            hidden: false,
            plan_focus: PlanFocus::Approve,
        }
    }

    /// Live request identity.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Tool-call identity.
    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }

    /// Questions in official order.
    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    /// Current page; `questions.len()` is the submit page.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Current focus inside the option list; `options.len()` is Other.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// Current draft for one page.
    pub fn draft(&self, page: usize) -> Option<&Draft> {
        self.drafts.get(page)
    }

    /// Whether one page was explicitly skipped.
    pub fn skipped(&self, page: usize) -> bool {
        self.skipped.get(page).copied().unwrap_or(false)
    }

    /// Current error line.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Short notice line.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Lifecycle.
    pub fn state(&self) -> QuestionCardState {
        self.state
    }

    /// Countdown facts.
    pub fn timed(&self) -> &TimedState {
        &self.timed
    }

    /// Whether the card was hidden with Esc but its request is still pending.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Whether this is a compact plan-review card.
    pub fn plan_review(&self) -> Option<(&Question, &PlanReviewIntent)> {
        if self.questions.len() != 1 {
            return None;
        }
        let question = &self.questions[0];
        let intent = question.plan_review()?;
        if question.detail.is_none() || !question.has_label(&intent.approve) {
            return None;
        }
        Some((question, intent))
    }

    /// Focused plan-review action.
    pub fn plan_focus(&self) -> PlanFocus {
        self.plan_focus
    }

    /// Whether the full plan is expanded.
    pub fn plan_expanded(&self) -> bool {
        self.plan_expanded
    }

    /// Detail scroll offset.
    pub fn detail_offset(&self) -> usize {
        self.detail_offset
    }

    /// Submit-page review scroll offset.
    pub fn review_offset(&self) -> usize {
        self.review_offset
    }

    /// Re-show a hidden card. A settled card reopens read-only.
    pub fn reveal(&mut self) {
        self.hidden = false;
        if self.state == QuestionCardState::Settled {
            self.state = QuestionCardState::ReadOnly;
        }
    }

    /// Update the Host-reported remaining duration (the claim frame may arrive
    /// after the first pending event).
    pub fn set_remaining_ms(&mut self, remaining_ms: u64, now: Instant) {
        if self.state != QuestionCardState::Draft || self.timed.continued || self.timed.indefinite {
            return;
        }
        self.timed = TimedState::start(remaining_ms, now);
    }

    /// Mark the answer as handed to the bridge.
    pub fn mark_submitting(&mut self) {
        self.state = QuestionCardState::Submitting;
        self.error = None;
    }

    /// The bridge refused or failed the answer; return to editing.
    pub fn answer_failed(&mut self, message: impl Into<String>) {
        self.state = QuestionCardState::Draft;
        self.error = Some(message.into());
    }

    /// The bridge settled the question with this batch.
    pub fn settle(&mut self, answers: &[AnswerItem]) {
        for (index, question) in self.questions.iter().enumerate() {
            if let Some(answer) = answers.iter().find(|answer| answer.id == question.id) {
                if let Some(draft) = self.drafts.get_mut(index) {
                    draft.selected = answer.selected.clone();
                    draft.custom = answer.custom.clone().unwrap_or_default();
                }
                if let Some(skipped) = self.skipped.get_mut(index) {
                    *skipped = !answer.answered();
                }
            }
        }
        self.state = QuestionCardState::Settled;
        self.hidden = true;
        self.timed.continued = true;
        self.timed.deadline = None;
    }

    /// The question became continued after a timed-out foreground wait.
    pub fn mark_continued(&mut self) {
        self.timed.continued = true;
        self.timed.deadline = None;
        if self.state == QuestionCardState::Draft {
            self.state = QuestionCardState::Continued;
        }
    }

    /// Advance the local countdown. Returns an expiry action once.
    pub fn tick(&mut self, now: Instant) -> Option<QuestionAction> {
        if self.state != QuestionCardState::Draft || self.timed.indefinite || self.timed.continued {
            return None;
        }
        let deadline = self.timed.deadline?;
        if now < deadline {
            return None;
        }
        self.timed.continued = true;
        self.timed.deadline = None;
        self.state = QuestionCardState::Continued;
        self.notice = Some("倒计时结束；Agent 可以继续，问题仍可回答".to_string());
        Some(QuestionAction::Expire {
            request_id: self.request_id.clone(),
            call_id: self.call_id.clone(),
        })
    }

    /// One key press. Returns an action only when the card leaves this state.
    pub fn handle_key(&mut self, key: DecisionKey) -> Option<QuestionAction> {
        self.error = None;
        match self.state {
            QuestionCardState::ReadOnly => return self.handle_read_only_key(key),
            QuestionCardState::Settled | QuestionCardState::Submitting => return None,
            _ => {}
        }
        if self.plan_review().is_some() {
            return self.handle_plan_key(key);
        }
        match key {
            DecisionKey::Esc => return self.close_action(),
            DecisionKey::Skip => return self.skip_current(),
            DecisionKey::MoreTime => {
                self.timed.hold();
                self.notice = Some("已停止倒计时；问题保持可回答".to_string());
                return None;
            }
            DecisionKey::PageUp => {
                self.detail_offset = self.detail_offset.saturating_sub(4);
                return None;
            }
            DecisionKey::PageDown => {
                self.detail_offset = self.detail_offset.saturating_add(4);
                return None;
            }
            DecisionKey::Left | DecisionKey::BackTab => {
                self.navigate_page(false);
                return None;
            }
            DecisionKey::Right | DecisionKey::Tab => {
                self.navigate_page(true);
                return None;
            }
            DecisionKey::Up => {
                self.move_focus(-1);
                return None;
            }
            DecisionKey::Down => {
                self.move_focus(1);
                return None;
            }
            DecisionKey::Space => {
                if self.page < self.questions.len() && self.questions[self.page].multi_select {
                    self.toggle_focus();
                }
                return None;
            }
            DecisionKey::Char(' ') => {
                if self.page < self.questions.len()
                    && self.questions[self.page].multi_select
                    && self.focus < self.questions[self.page].options.len()
                {
                    self.toggle_focus();
                } else {
                    self.insert_char(' ');
                }
                return None;
            }
            DecisionKey::Char(character) => {
                if character.is_ascii_digit() && character != '0' {
                    let index = character as usize - '1' as usize;
                    return self.direct_select(index);
                }
                self.insert_char(character);
                return None;
            }
            DecisionKey::Backspace => {
                self.backspace();
                return None;
            }
            DecisionKey::Enter => {
                if self.page == self.questions.len() {
                    return self.submit();
                }
                return self.confirm_current();
            }
        }
    }

    fn handle_read_only_key(&mut self, key: DecisionKey) -> Option<QuestionAction> {
        match key {
            DecisionKey::Esc | DecisionKey::Enter => Some(QuestionAction::CloseReadOnly),
            DecisionKey::Up | DecisionKey::PageUp => {
                self.detail_offset = self.detail_offset.saturating_sub(4);
                None
            }
            DecisionKey::Down | DecisionKey::PageDown => {
                self.detail_offset = self.detail_offset.saturating_add(4);
                None
            }
            _ => None,
        }
    }

    fn handle_plan_key(&mut self, key: DecisionKey) -> Option<QuestionAction> {
        match key {
            DecisionKey::Esc => return self.close_action(),
            DecisionKey::Up => {
                self.plan_focus = match self.plan_focus {
                    PlanFocus::View => PlanFocus::Approve,
                    PlanFocus::RequestChanges => PlanFocus::View,
                    PlanFocus::Approve => PlanFocus::RequestChanges,
                };
                None
            }
            DecisionKey::Down => {
                self.plan_focus = match self.plan_focus {
                    PlanFocus::View => PlanFocus::RequestChanges,
                    PlanFocus::RequestChanges => PlanFocus::Approve,
                    PlanFocus::Approve => PlanFocus::View,
                };
                None
            }
            DecisionKey::PageUp => {
                self.detail_offset = self.detail_offset.saturating_sub(4);
                None
            }
            DecisionKey::PageDown => {
                self.detail_offset = self.detail_offset.saturating_add(4);
                None
            }
            DecisionKey::MoreTime => {
                self.timed.hold();
                self.notice = Some("已停止倒计时；问题保持可回答".to_string());
                None
            }
            DecisionKey::Enter => match self.plan_focus {
                PlanFocus::View => {
                    self.plan_expanded = !self.plan_expanded;
                    None
                }
                PlanFocus::RequestChanges => Some(QuestionAction::RequestChanges {
                    request_id: self.request_id.clone(),
                    call_id: self.call_id.clone(),
                }),
                PlanFocus::Approve => {
                    let intent = self.questions[0].plan_review()?.clone();
                    self.submit_items(vec![AnswerItem {
                        id: self.questions[0].id.clone(),
                        selected: vec![intent.approve],
                        custom: None,
                    }])
                }
            },
            DecisionKey::Char('1') => {
                self.plan_focus = PlanFocus::View;
                None
            }
            DecisionKey::Char('2') => {
                self.plan_focus = PlanFocus::RequestChanges;
                None
            }
            DecisionKey::Char('3') => {
                self.plan_focus = PlanFocus::Approve;
                None
            }
            DecisionKey::Tab | DecisionKey::Right => {
                self.plan_focus = match self.plan_focus {
                    PlanFocus::View => PlanFocus::RequestChanges,
                    PlanFocus::RequestChanges => PlanFocus::Approve,
                    PlanFocus::Approve => PlanFocus::View,
                };
                None
            }
            DecisionKey::BackTab | DecisionKey::Left => {
                self.plan_focus = match self.plan_focus {
                    PlanFocus::View => PlanFocus::Approve,
                    PlanFocus::RequestChanges => PlanFocus::View,
                    PlanFocus::Approve => PlanFocus::RequestChanges,
                };
                None
            }
            _ => None,
        }
    }

    fn close_action(&mut self) -> Option<QuestionAction> {
        if self.call_id.is_some() {
            self.hidden = true;
            Some(QuestionAction::Hide)
        } else {
            Some(QuestionAction::Cancel {
                request_id: self.request_id.clone(),
            })
        }
    }

    fn current_question(&self) -> Option<&Question> {
        self.questions.get(self.page)
    }

    fn current_draft_mut(&mut self) -> Option<&mut Draft> {
        self.drafts.get_mut(self.page)
    }

    fn move_focus(&mut self, delta: isize) {
        if self.page == self.questions.len() {
            if delta < 0 {
                self.review_offset = self.review_offset.saturating_sub(1);
            } else {
                self.review_offset = self.review_offset.saturating_add(1);
            }
            return;
        }
        let Some(question) = self.current_question() else {
            return;
        };
        let count = question.options.len() + 1;
        let current = self.focus.min(count - 1) as isize;
        self.focus = ((current + delta).rem_euclid(count as isize)) as usize;
    }

    fn navigate_page(&mut self, forward: bool) {
        if self.questions.len() <= 1 {
            return;
        }
        let count = self.questions.len() + 1;
        if self.page == self.questions.len() {
            if forward {
                self.page = 0;
            } else {
                self.page = self.questions.len().saturating_sub(1);
            }
            self.focus = 0;
            self.error = None;
            return;
        }
        if forward {
            if self.page + 1 < count {
                self.page += 1;
            }
        } else if self.page > 0 {
            self.page -= 1;
        }
        self.focus = 0;
        self.error = None;
    }

    fn toggle_focus(&mut self) {
        let Some(question) = self.current_question() else {
            return;
        };
        let Some(option) = question.options.get(self.focus) else {
            return;
        };
        let label = option.label.clone();
        let Some(draft) = self.current_draft_mut() else {
            return;
        };
        if let Some(index) = draft.selected.iter().position(|item| item == &label) {
            draft.selected.remove(index);
        } else {
            draft.selected.push(label);
        }
        if let Some(skipped) = self.skipped.get_mut(self.page) {
            *skipped = false;
        }
        self.hold_countdown();
    }

    fn direct_select(&mut self, index: usize) -> Option<QuestionAction> {
        let Some(question) = self.current_question() else {
            return None;
        };
        if index >= question.options.len() {
            return None;
        }
        let multi_select = question.multi_select;
        self.focus = index;
        if multi_select {
            self.toggle_focus();
            return None;
        }
        self.select_single(index);
        self.advance()
    }

    fn select_single(&mut self, index: usize) {
        let label = self
            .current_question()
            .and_then(|question| question.options.get(index))
            .map(|option| option.label.clone());
        let Some(label) = label else {
            return;
        };
        if let Some(draft) = self.current_draft_mut() {
            draft.selected = vec![label];
            draft.custom.clear();
            draft.caret = 0;
        }
        if let Some(skipped) = self.skipped.get_mut(self.page) {
            *skipped = false;
        }
        self.hold_countdown();
    }

    fn confirm_current(&mut self) -> Option<QuestionAction> {
        let Some(question) = self.current_question() else {
            return None;
        };
        let option_count = question.options.len();
        let multi_select = question.multi_select;
        if self.focus == option_count {
            let text = self
                .drafts
                .get(self.page)
                .map(|draft| draft.custom.trim().to_string())
                .unwrap_or_default();
            let has_selection = self
                .drafts
                .get(self.page)
                .map(|draft| !draft.selected.is_empty())
                .unwrap_or(false);
            if text.is_empty() {
                if multi_select && has_selection {
                    return self.advance();
                }
                self.error = Some("请填写其他答案，或选择一个选项".to_string());
                return None;
            }
            if let Some(draft) = self.current_draft_mut() {
                draft.custom = text;
                if !multi_select {
                    draft.selected.clear();
                }
            }
            if let Some(skipped) = self.skipped.get_mut(self.page) {
                *skipped = false;
            }
            self.hold_countdown();
            return self.advance();
        }
        if multi_select {
            let answered = self
                .drafts
                .get(self.page)
                .map(Draft::answered)
                .unwrap_or(false)
                || self.skipped(self.page);
            if answered {
                return self.advance();
            }
            self.error = Some("请至少选择一项".to_string());
            return None;
        }
        let index = self.focus;
        self.select_single(index);
        self.advance()
    }

    fn insert_char(&mut self, character: char) {
        if self.page >= self.questions.len() {
            return;
        }
        let option_count = self.questions[self.page].options.len();
        if self.focus != option_count {
            return;
        }
        let single = !self.questions[self.page].multi_select;
        if let Some(draft) = self.current_draft_mut() {
            if single {
                draft.selected.clear();
            }
            let caret = draft.caret.min(draft.custom.chars().count());
            let byte = char_index(draft.custom.as_str(), caret);
            draft.custom.insert(byte, character);
            draft.caret = caret + 1;
        }
        if let Some(skipped) = self.skipped.get_mut(self.page) {
            *skipped = false;
        }
        self.hold_countdown();
    }

    fn backspace(&mut self) {
        if self.page >= self.questions.len() {
            return;
        }
        let option_count = self.questions[self.page].options.len();
        if self.focus != option_count {
            return;
        }
        if let Some(draft) = self.current_draft_mut() {
            let caret = draft.caret.min(draft.custom.chars().count());
            if caret == 0 {
                return;
            }
            let start = char_index(draft.custom.as_str(), caret - 1);
            let end = char_index(draft.custom.as_str(), caret);
            draft.custom.replace_range(start..end, "");
            draft.caret = caret - 1;
        }
        if let Some(skipped) = self.skipped.get_mut(self.page) {
            *skipped = false;
        }
        self.hold_countdown();
    }

    fn hold_countdown(&mut self) {
        if self.timed.deadline.is_some() {
            self.timed.hold();
            self.notice = Some("已暂停倒计时；问题保持可回答".to_string());
        }
    }

    fn skip_current(&mut self) -> Option<QuestionAction> {
        if self.page >= self.questions.len() {
            return None;
        }
        self.drafts[self.page] = Draft::default();
        self.skipped[self.page] = true;
        self.advance()
    }

    fn advance(&mut self) -> Option<QuestionAction> {
        if self.questions.len() == 1 {
            return self.submit();
        }
        if self.page + 1 < self.questions.len() {
            self.page += 1;
            self.focus = 0;
            self.error = None;
            None
        } else {
            self.page = self.questions.len();
            self.focus = 0;
            self.error = None;
            None
        }
    }

    fn submit(&mut self) -> Option<QuestionAction> {
        for (index, question) in self.questions.iter().enumerate() {
            let _ = question;
            if !self.drafts[index].answered() && !self.skipped(index) {
                self.page = index;
                self.focus = 0;
                self.error = Some("请先回答这一题".to_string());
                return None;
            }
        }
        let answers: Vec<AnswerItem> = self
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| self.drafts[index].to_answer(question))
            .collect();
        self.submit_items(answers)
    }

    fn submit_items(&mut self, answers: Vec<AnswerItem>) -> Option<QuestionAction> {
        if let Err(error) = validate_answer_batch(&self.questions, &answers) {
            self.error = Some(error.to_string());
            return None;
        }
        self.state = QuestionCardState::Submitting;
        Some(QuestionAction::Answer {
            request_id: self.request_id.clone(),
            call_id: self.call_id.clone(),
            answers,
        })
    }

    /// Scroll the detail or review region.
    pub fn scroll_detail(&mut self, delta: isize) {
        let target = if self.page == self.questions.len() {
            &mut self.review_offset
        } else {
            &mut self.detail_offset
        };
        if delta < 0 {
            *target = target.saturating_sub(delta.unsigned_abs());
        } else {
            *target = target.saturating_add(delta as usize);
        }
    }

    /// Paste into the Other field, collapsing newlines into spaces.
    pub fn paste(&mut self, text: &str) -> bool {
        if self.page >= self.questions.len() {
            return false;
        }
        let option_count = self.questions[self.page].options.len();
        if self.focus != option_count {
            return false;
        }
        let single = !self.questions[self.page].multi_select;
        let text = text.replace(['\r', '\n'], " ");
        if let Some(draft) = self.current_draft_mut() {
            if single {
                draft.selected.clear();
            }
            let caret = draft.caret.min(draft.custom.chars().count());
            let byte = char_index(draft.custom.as_str(), caret);
            draft.custom.insert_str(byte, &text);
            draft.caret = caret + text.chars().count();
        }
        if let Some(skipped) = self.skipped.get_mut(self.page) {
            *skipped = false;
        }
        self.hold_countdown();
        true
    }

    /// Pointer: choose one option row.
    pub fn click_option(&mut self, index: usize) -> Option<QuestionAction> {
        self.focus = index;
        let Some(question) = self.current_question() else {
            return None;
        };
        if question.multi_select {
            self.toggle_focus();
            None
        } else {
            self.select_single(index);
            self.advance()
        }
    }

    /// Pointer: focus the Other row.
    pub fn click_other(&mut self) -> bool {
        let Some(question) = self.current_question() else {
            return false;
        };
        self.focus = question.options.len();
        self.error = None;
        true
    }

    /// Pointer: submit or confirm the current page.
    pub fn click_submit(&mut self) -> Option<QuestionAction> {
        if self.page == self.questions.len() {
            self.submit()
        } else {
            self.confirm_current()
        }
    }

    /// Pointer: skip the current question.
    pub fn click_skip(&mut self) -> Option<QuestionAction> {
        self.skip_current()
    }

    /// Pointer: move one page.
    pub fn click_nav(&mut self, forward: bool) -> bool {
        self.navigate_page(forward);
        true
    }

    /// Pointer: choose one plan-review action.
    pub fn click_plan(&mut self, focus: PlanFocus) -> Option<QuestionAction> {
        self.plan_focus = focus;
        match focus {
            PlanFocus::View => {
                self.plan_expanded = !self.plan_expanded;
                None
            }
            PlanFocus::RequestChanges => Some(QuestionAction::RequestChanges {
                request_id: self.request_id.clone(),
                call_id: self.call_id.clone(),
            }),
            PlanFocus::Approve => {
                let intent = self.questions[0].plan_review()?.clone();
                self.submit_items(vec![AnswerItem {
                    id: self.questions[0].id.clone(),
                    selected: vec![intent.approve],
                    custom: None,
                }])
            }
        }
    }

    /// Wire batch for the current drafts, used by the renderer's review page.
    pub fn review_answers(&self) -> Vec<AnswerItem> {
        self.questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                if self.skipped(index) {
                    return AnswerItem::skipped(question.id.clone());
                }
                self.drafts
                    .get(index)
                    .map(|draft| draft.to_answer(question))
                    .unwrap_or_else(|| AnswerItem::skipped(question.id.clone()))
            })
            .collect()
    }

    /// The wire value for one batch.
    pub fn answers_wire(&self, answers: &[AnswerItem]) -> serde_json::Value {
        answers_to_wire(answers)
    }
}

fn char_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_questions;
    use serde_json::json;

    fn questions(value: serde_json::Value) -> Vec<Question> {
        parse_questions(Some(&value))
    }

    fn single() -> QuestionCard {
        QuestionCard::new(
            Some("r".into()),
            None,
            questions(json!([{
                "id": "q", "question": "Pick",
                "options": [{"label": "A"}, {"label": "B"}]
            }])),
            None,
        )
    }

    fn multi() -> QuestionCard {
        QuestionCard::new(
            Some("r".into()),
            None,
            questions(json!([{
                "id": "q", "question": "Pick many", "multi_select": true,
                "options": [{"label": "A"}, {"label": "B"}]
            }])),
            None,
        )
    }

    fn batch() -> QuestionCard {
        QuestionCard::new(
            Some("r".into()),
            None,
            questions(json!([
                {"id": "q1", "question": "One", "options": [{"label": "A"}]},
                {"id": "q2", "question": "Two", "options": [{"label": "B"}]}
            ])),
            None,
        )
    }

    #[test]
    fn a_number_key_selects_and_submits_a_single_choice() {
        let mut card = single();
        let action = card.handle_key(DecisionKey::Char('2')).unwrap();
        let QuestionAction::Answer { answers, .. } = action else {
            panic!("expected answer");
        };
        assert_eq!(answers[0].selected, vec!["B"]);
        assert_eq!(answers[0].id, "q");
    }

    #[test]
    fn multi_select_toggles_and_enter_submits_without_adding_the_focus() {
        let mut card = multi();
        card.handle_key(DecisionKey::Space);
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        let QuestionAction::Answer { answers, .. } = action else {
            panic!("expected answer");
        };
        assert_eq!(answers[0].selected, vec!["A"]);
    }

    #[test]
    fn other_text_is_exclusive_for_a_single_choice() {
        let mut card = single();
        card.handle_key(DecisionKey::Down);
        card.handle_key(DecisionKey::Down);
        assert_eq!(card.focus(), 2);
        card.handle_key(DecisionKey::Char('x'));
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        let QuestionAction::Answer { answers, .. } = action else {
            panic!("expected answer");
        };
        assert!(answers[0].selected.is_empty());
        assert_eq!(answers[0].custom.as_deref(), Some("x"));
    }

    #[test]
    fn a_batch_goes_through_a_submit_page_and_blocks_missing_answers() {
        let mut card = batch();
        // Reach the submit page without answering either question.
        card.handle_key(DecisionKey::Tab);
        assert_eq!(card.page(), 1);
        card.handle_key(DecisionKey::Tab);
        assert_eq!(card.page(), 2);
        // Submit with missing answers: return to the first incomplete page.
        assert!(card.handle_key(DecisionKey::Enter).is_none());
        assert_eq!(card.page(), 0);
        assert!(card.error().is_some());
        // Answer q1, then q2, then submit the batch from the submit page.
        card.handle_key(DecisionKey::Enter);
        assert_eq!(card.page(), 1);
        card.handle_key(DecisionKey::Enter);
        assert_eq!(card.page(), 2);
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        let QuestionAction::Answer { answers, .. } = action else {
            panic!("expected answer");
        };
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].id, "q1");
        assert_eq!(answers[1].id, "q2");
    }

    #[test]
    fn skip_submits_the_official_empty_result() {
        let mut card = single();
        let action = card.handle_key(DecisionKey::Skip).unwrap();
        let QuestionAction::Answer { answers, .. } = action else {
            panic!("expected answer");
        };
        assert!(answers[0].selected.is_empty());
        assert_eq!(answers[0].custom, None);
    }

    #[test]
    fn closing_a_keyed_card_hides_and_an_unkeyed_card_cancels() {
        let mut keyed = single();
        keyed.call_id = Some("c".into());
        assert_eq!(
            keyed.handle_key(DecisionKey::Esc),
            Some(QuestionAction::Hide)
        );
        assert!(keyed.hidden());
        let mut unkeyed = single();
        assert_eq!(
            unkeyed.handle_key(DecisionKey::Esc),
            Some(QuestionAction::Cancel {
                request_id: Some("r".into())
            })
        );
    }

    #[test]
    fn a_timed_card_expires_once_and_then_answers_through_the_continued_path() {
        let now = Instant::now();
        let mut card = QuestionCard::new(
            Some("r".into()),
            Some("c".into()),
            questions(json!([{
                "id": "q", "question": "Pick", "options": [{"label": "A"}]
            }])),
            Some(TimedState::start(5, now)),
        );
        let action = card.tick(now + Duration::from_millis(10)).unwrap();
        assert!(matches!(action, QuestionAction::Expire { .. }));
        assert!(card.tick(now + Duration::from_millis(20)).is_none());
        assert_eq!(card.state(), QuestionCardState::Continued);
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        let QuestionAction::Answer { call_id, .. } = action else {
            panic!("expected answer");
        };
        assert_eq!(call_id.as_deref(), Some("c"));
    }

    #[test]
    fn a_failed_submission_rearms_after_the_question_becomes_continued() {
        let now = Instant::now();
        let mut card = QuestionCard::new(
            Some("r".into()),
            Some("c".into()),
            questions(json!([{
                "id": "q", "question": "Pick", "options": [{"label": "A"}]
            }])),
            Some(TimedState::start(5, now)),
        );
        card.mark_submitting();
        card.answer_failed("not-continued");
        card.mark_continued();
        let action = card.handle_key(DecisionKey::Enter).unwrap();
        assert!(matches!(
            action,
            QuestionAction::Answer {
                call_id: Some(_),
                ..
            }
        ));
    }
}
