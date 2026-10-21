//! The reading-layer display model.
//!
//! Entries are keyed by S02's official identities (`seq`, `message_id`, or
//! `callId`). S03 enriches them with display-only facts from the same durable
//! events — reasoning text, tool-result metadata, approval audit, and the timed
//! question projection — but never builds a second conversation history.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use nano_conversation::{
    ConversationItem, ConversationModel, MessageKind, MessageView, SessionAddress, ToolCallView,
};
use nano_decisions::approval::ApprovalOutcome;
use nano_decisions::contract::{parse_answers, parse_questions, AnswerItem};
use nano_decisions::model::QuestionProjection;
use serde_json::Value;

use crate::tool::{
    derive_question_state, parse_answers_from_result, ApprovalView, QuestionRecord,
    RecordedQuestionState, ToolRecord, ToolResultFact,
};

/// Maximum enrichment entries kept per map. The durable log remains the full
/// history; older display-only facts are dropped first.
const MAX_ENRICHMENT: usize = 4_096;

/// D04: which transcript capabilities a consumer may expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSource {
    /// One-shot history or an external leaf: render and scroll only.
    ReadOnly,
    /// A continuable conversation: expansion, scroll, and decision entry points.
    Interactive,
}

impl TranscriptSource {
    /// Whether the view reacts to expansion clicks.
    pub fn interactive(self) -> bool {
        self == Self::Interactive
    }
}

/// One rendered entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Stable identity: message id, `seq-N`, `call-<callId>`, or preview id.
    pub identity: String,
    /// Display content.
    pub kind: EntryKind,
    /// Display revision hash; the renderer reuses cached height only when this
    /// and the width/expansion state are unchanged.
    pub fingerprint: u64,
}

/// Display content of one entry.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// A human prompt.
    User {
        /// Visible text.
        text: String,
    },
    /// A model answer.
    Assistant {
        /// Visible text.
        text: String,
        /// Reasoning text, kept separate from the answer.
        reasoning: String,
        /// Reasoning duration in milliseconds, when the stream recorded it.
        reasoning_ms: Option<u64>,
        /// The turn was cancelled mid-stream.
        interrupted: bool,
    },
    /// A turn-level failure or interruption notice.
    Notice {
        /// Notice text.
        text: String,
        /// Failure styling.
        is_error: bool,
    },
    /// Injected context or a synthetic bash record.
    Context {
        /// Visible text.
        text: String,
        /// Synthetic bash rather than an injected notice.
        bash: bool,
    },
    /// One tool call and its result.
    Tool(ToolRecord),
    /// A live assistant attempt.
    Preview(PreviewEntry),
    /// A local echo that has not landed as a durable message.
    Echo {
        /// Submitted text.
        text: String,
        /// Submission-state label.
        state: String,
    },
}

/// Live assistant preview as the reading layer displays it.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewEntry {
    /// Attempt identity.
    pub attempt_id: String,
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// Visible text.
    pub text: String,
    /// Reasoning text.
    pub reasoning: String,
    /// Reasoning elapsed time while streaming.
    pub reasoning_ms: Option<u64>,
    /// Tool arguments still arriving; never treated as executed.
    pub tools: Vec<PreviewTool>,
    /// A dense-index or revision gap was observed.
    pub gap: bool,
    /// The preview was cut by its byte bound.
    pub truncated: bool,
}

/// One tool call inside a live preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewTool {
    /// Tool name, when seen.
    pub name: Option<String>,
    /// Arguments completed their block.
    pub complete: bool,
}

/// Display-only facts gathered from durable events and decision events.
#[derive(Debug, Clone, Default)]
pub struct Enrichment {
    assistant: BTreeMap<u64, AssistantFact>,
    tool_results: BTreeMap<String, ToolResultFact>,
    approvals: BTreeMap<String, ApprovalFact>,
    approval_by_call: BTreeMap<String, String>,
    approval_outcome_by_call: BTreeMap<String, ApprovalOutcome>,
    question_answers: BTreeMap<String, Vec<AnswerItem>>,
    question_continued: BTreeSet<String>,
    question_submitted: BTreeSet<String>,
    question_cancelled: BTreeSet<String>,
    question_timed: BTreeSet<String>,
    question_questions: BTreeMap<String, Vec<nano_decisions::contract::Question>>,
    projection: QuestionProjection,
}

#[derive(Debug, Clone, PartialEq)]
struct AssistantFact {
    text: String,
    reasoning: String,
    reasoning_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalFact {
    tool_name: String,
    call_id: Option<String>,
    reason: Option<String>,
    display_reason: BTreeMap<String, String>,
    pending: bool,
}

/// The reading-layer model for one address.
#[derive(Debug, Clone)]
pub struct TranscriptModel {
    address: SessionAddress,
    source: TranscriptSource,
    enrichment: Enrichment,
    pub(crate) entries: Vec<Entry>,
    expansion: HashSet<String>,
    preview_started: HashMap<String, Instant>,
    pub(crate) revision: u64,
    aliases: HashMap<String, String>,
    last_error: Option<String>,
}

impl TranscriptModel {
    /// Build a model for one address and source kind.
    pub fn new(address: SessionAddress, source: TranscriptSource) -> Self {
        Self {
            address,
            source,
            enrichment: Enrichment::default(),
            entries: Vec::new(),
            expansion: HashSet::new(),
            preview_started: HashMap::new(),
            revision: 0,
            aliases: HashMap::new(),
            last_error: None,
        }
    }

    /// Bound address.
    pub fn address(&self) -> &SessionAddress {
        &self.address
    }

    /// D04 source kind.
    pub fn source(&self) -> TranscriptSource {
        self.source
    }

    /// Monotonic display revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Current entries.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Last wire-parsing error worth showing.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Resolve a retired preview identity to the durable message that replaced
    /// it, so a reading anchor survives the preview-to-message handoff.
    pub fn alias(&self, identity: &str) -> Option<&str> {
        self.aliases.get(identity).map(String::as_str)
    }

    /// Whether one entry is expanded.
    pub fn is_expanded(&self, identity: &str) -> bool {
        self.expansion.contains(identity)
    }

    /// Toggle one entry when this source is interactive.
    pub fn toggle(&mut self, identity: &str) -> bool {
        if !self.source.interactive() {
            return false;
        }
        if !self.entries.iter().any(|entry| entry.identity == identity) {
            return false;
        }
        if !self.expansion.remove(identity) {
            self.expansion.insert(identity.to_string());
        }
        self.revision += 1;
        true
    }

    /// Reveal an entry (for example a tool row that reopens a question).
    pub fn reveal(&mut self, identity: &str) {
        if self.source.interactive() {
            self.expansion.insert(identity.to_string());
            self.revision += 1;
        }
    }

    /// The recorded question behind one tool call.
    pub fn question(&self, call_id: &str) -> Option<QuestionRecord> {
        self.entries.iter().find_map(|entry| match &entry.kind {
            EntryKind::Tool(record) if record.call_id == call_id => match &record.display {
                crate::tool::ToolDisplay::AskUser { record } => Some(record.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    /// Apply one profile event (`session.event` or a decisions kind).
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        match kind {
            "session.event" => self.apply_session_event(payload),
            "decisions.approval" => self.apply_approval_event(payload),
            "decisions.question" => self.apply_question_event(payload),
            "decisions.projection" => self.apply_projection_event(payload),
            _ => false,
        }
    }

    /// Rebuild entries from S02's projection. Returns whether the visible
    /// entries changed.
    pub fn sync(&mut self, model: &ConversationModel, now: Instant) -> bool {
        let mut entries = Vec::new();
        for item in model.items() {
            entries.push(self.entry_for_item(item));
        }
        let live = model.live();
        for preview in live.previews {
            let started = self
                .preview_started
                .entry(preview.attempt_id.clone())
                .or_insert(now);
            let reasoning_ms = if preview.reasoning.trim().is_empty() {
                None
            } else {
                Some(now.saturating_duration_since(*started).as_millis() as u64)
            };
            let tools = preview
                .tools
                .into_iter()
                .map(|tool| PreviewTool {
                    name: tool.name,
                    complete: tool.complete,
                })
                .collect();
            entries.push(Entry {
                identity: format!("preview-{}", preview.attempt_id),
                fingerprint: 0,
                kind: EntryKind::Preview(PreviewEntry {
                    attempt_id: preview.attempt_id,
                    turn: preview.turn,
                    step: preview.step,
                    text: preview.text,
                    reasoning: preview.reasoning,
                    reasoning_ms,
                    tools,
                    gap: preview.gap,
                    truncated: preview.truncated,
                }),
            });
        }
        // Drop live-start records for attempts that no longer exist.
        let live_ids: HashSet<String> = entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Preview(preview) => Some(format!("preview-{}", preview.attempt_id)),
                _ => None,
            })
            .collect();
        self.preview_started
            .retain(|attempt, _| live_ids.contains(&format!("preview-{attempt}")));
        for echo in live.echoes {
            entries.push(Entry {
                identity: format!("echo-{}", echo.request_id),
                fingerprint: 0,
                kind: EntryKind::Echo {
                    text: echo.text,
                    state: echo.state.label().to_string(),
                },
            });
        }
        for entry in &mut entries {
            entry.fingerprint = fingerprint(entry);
        }
        let changed = entries != self.entries;
        if changed {
            self.revision += 1;
            let identities: HashSet<&str> = entries
                .iter()
                .map(|entry| entry.identity.as_str())
                .collect();
            self.expansion
                .retain(|identity| identities.contains(identity.as_str()));
            self.entries = entries;
        }
        changed
    }

    fn entry_for_item(&self, item: &ConversationItem) -> Entry {
        match item {
            ConversationItem::Message(message) => self.message_entry(message),
            ConversationItem::ToolCall(call) => {
                let result_fact = self.enrichment.tool_results.get(&call.call_id);
                let approval = self.approval_view(call);
                let question = self.question_record(call);
                let record =
                    ToolRecord::build(call, result_fact, approval.as_ref(), question.as_ref());
                Entry {
                    identity: format!("call-{}", call.call_id),
                    fingerprint: 0,
                    kind: EntryKind::Tool(record),
                }
            }
        }
    }

    fn message_entry(&self, message: &MessageView) -> Entry {
        let identity = message
            .message_id
            .clone()
            .unwrap_or_else(|| format!("seq-{}", message.seq));
        let kind = match message.kind {
            MessageKind::User => EntryKind::User {
                text: message.text.clone(),
            },
            MessageKind::Assistant => {
                let fact = self.enrichment.assistant.get(&message.seq);
                EntryKind::Assistant {
                    text: fact
                        .map(|fact| fact.text.clone())
                        .unwrap_or_else(|| message.text.clone()),
                    reasoning: fact.map(|fact| fact.reasoning.clone()).unwrap_or_default(),
                    reasoning_ms: fact.and_then(|fact| fact.reasoning_ms),
                    interrupted: message.interrupted,
                }
            }
            MessageKind::Notice => EntryKind::Notice {
                text: message.text.clone(),
                is_error: message.is_error,
            },
            MessageKind::SyntheticBash => EntryKind::Context {
                text: message.text.clone(),
                bash: true,
            },
            MessageKind::Injected => EntryKind::Context {
                text: message.text.clone(),
                bash: false,
            },
            MessageKind::ToolResult => EntryKind::Notice {
                text: message.text.clone(),
                is_error: message.is_error,
            },
        };
        Entry {
            identity,
            fingerprint: 0,
            kind,
        }
    }

    fn approval_view(&self, call: &ToolCallView) -> Option<ApprovalView> {
        let id = self.enrichment.approval_by_call.get(&call.call_id)?;
        let fact = self.enrichment.approvals.get(id)?;
        Some(ApprovalView {
            tool_name: fact.tool_name.clone(),
            reason: fact.reason.clone(),
            display_reason: fact.display_reason.clone(),
            outcome: self
                .enrichment
                .approval_outcome_by_call
                .get(&call.call_id)
                .copied(),
            asked: true,
            pending: fact.pending,
        })
    }

    fn question_record(&self, call: &ToolCallView) -> Option<QuestionRecord> {
        if call.name != "ask_user_question" {
            return None;
        }
        let questions = serde_json::from_str::<Value>(&call.arguments)
            .ok()
            .map(|value| parse_questions(value.get("questions")))
            .filter(|questions| !questions.is_empty())
            .or_else(|| {
                self.enrichment
                    .question_questions
                    .get(&call.call_id)
                    .cloned()
            })
            .unwrap_or_default();
        let settled = self.enrichment.question_answers.get(&call.call_id);
        let continued = self.enrichment.question_continued.contains(&call.call_id)
            || self
                .enrichment
                .projection
                .active
                .iter()
                .any(|entry| entry.call_id == call.call_id && entry.state == "continued");
        let cancelled = self.enrichment.question_cancelled.contains(&call.call_id);
        let mut state = if continued {
            // A queued reply can still be discarded; the projection says the
            // question is answerable, so the row must stay clickable.
            RecordedQuestionState::Continued
        } else if cancelled {
            RecordedQuestionState::Cancelled
        } else {
            derive_question_state(call, settled.map(Vec::as_slice), Some(false))
        };
        if state == RecordedQuestionState::Open
            && self.enrichment.question_submitted.contains(&call.call_id)
            && settled.is_none()
        {
            state = RecordedQuestionState::Submitted;
        }
        Some(QuestionRecord {
            questions,
            timed: self.enrichment.question_timed.contains(&call.call_id)
                || serde_json::from_str::<Value>(&call.arguments)
                    .ok()
                    .is_some_and(|value| value.get("timeout").is_some()),
            state,
            answers: settled.cloned().unwrap_or_else(|| {
                parse_answers_from_result(call.result.as_deref()).unwrap_or_default()
            }),
        })
    }

    fn apply_session_event(&mut self, payload: &Value) -> bool {
        let event = match nano_conversation::parse_session_event(payload) {
            Ok(event) => event,
            Err(error) => {
                self.last_error = Some(error.0);
                return true;
            }
        };
        if !event.address.matches(&self.address) {
            return false;
        }
        let mut changed = false;
        match event.event_type.as_str() {
            "assistant/message" => {
                let fact = assistant_fact(&event.data);
                insert_bounded(&mut self.enrichment.assistant, event.seq, fact);
                if let (Some(message_id), Some(turn), Some(step)) = (
                    event
                        .data
                        .get("message")
                        .and_then(|message| message.get("id"))
                        .and_then(Value::as_str),
                    event.data.get("turn").and_then(Value::as_u64),
                    event.data.get("step").and_then(Value::as_u64),
                ) {
                    let replaced: Vec<String> = self
                        .entries
                        .iter()
                        .filter_map(|entry| match &entry.kind {
                            EntryKind::Preview(preview)
                                if preview.turn == turn && preview.step == step =>
                            {
                                Some(entry.identity.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    for identity in replaced {
                        self.aliases.insert(identity, message_id.to_string());
                    }
                    while self.aliases.len() > MAX_ENRICHMENT {
                        if let Some(key) = self.aliases.keys().next().cloned() {
                            self.aliases.remove(&key);
                        } else {
                            break;
                        }
                    }
                }
                changed = true;
            }
            "tool/result" => {
                if let Some(fact) = tool_result_fact(&event.data) {
                    if fact.text.as_deref().is_some_and(is_pending_question) {
                        self.enrichment
                            .question_continued
                            .insert(fact.call_id.clone());
                    }
                    insert_bounded(
                        &mut self.enrichment.tool_results,
                        fact.call_id.clone(),
                        fact,
                    );
                    changed = true;
                }
            }
            "approval/asked" => {
                let id = event
                    .data
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() {
                    let call_id = event
                        .data
                        .get("callId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(call_id) = &call_id {
                        self.enrichment
                            .approval_by_call
                            .insert(call_id.clone(), id.to_string());
                    }
                    insert_bounded(
                        &mut self.enrichment.approvals,
                        id.to_string(),
                        ApprovalFact {
                            tool_name: event
                                .data
                                .get("toolName")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string(),
                            call_id,
                            reason: event
                                .data
                                .get("reason")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            display_reason: BTreeMap::new(),
                            pending: false,
                        },
                    );
                    changed = true;
                }
            }
            "approval/decided" => {
                let id = event
                    .data
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let outcome = event
                    .data
                    .get("outcome")
                    .and_then(Value::as_str)
                    .and_then(ApprovalOutcome::from_str);
                if let (Some(outcome), Some(fact)) = (outcome, self.enrichment.approvals.get(id)) {
                    if let Some(call_id) = &fact.call_id {
                        self.enrichment
                            .approval_outcome_by_call
                            .insert(call_id.clone(), outcome);
                        if let Some(approval) = self.enrichment.approvals.get_mut(id) {
                            approval.pending = false;
                        }
                    }
                    changed = true;
                }
            }
            "user/message" => {
                let source_kind = event
                    .data
                    .get("source")
                    .and_then(|source| source.get("kind"))
                    .and_then(Value::as_str);
                if source_kind == Some("user-question-reply") {
                    if let Some(call_id) = event
                        .data
                        .get("source")
                        .and_then(|source| source.get("callId"))
                        .and_then(Value::as_str)
                    {
                        if let Some(answers) = reply_answers(&event.data) {
                            insert_bounded(
                                &mut self.enrichment.question_answers,
                                call_id.to_string(),
                                answers,
                            );
                            self.enrichment.question_continued.remove(call_id);
                            changed = true;
                        }
                    }
                }
            }
            _ => {}
        }
        if changed {
            self.last_error = None;
        }
        changed
    }

    fn apply_approval_event(&mut self, payload: &Value) -> bool {
        if !crate::address_matches(payload, &self.address) {
            return false;
        }
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let call_id = payload
            .get("callId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if state == "withdrawn" {
            if let Some(id) = self
                .enrichment
                .approval_by_call
                .get(&request_id)
                .cloned()
                .or_else(|| {
                    call_id
                        .as_deref()
                        .and_then(|call_id| self.enrichment.approval_by_call.get(call_id).cloned())
                })
            {
                if let Some(fact) = self.enrichment.approvals.get_mut(&id) {
                    fact.pending = false;
                }
                return true;
            }
            return false;
        }
        if state == "settled" {
            let outcome = payload
                .get("outcome")
                .and_then(Value::as_str)
                .and_then(ApprovalOutcome::from_str);
            if let (Some(outcome), Some(call_id)) = (outcome, call_id) {
                self.enrichment
                    .approval_outcome_by_call
                    .insert(call_id.clone(), outcome);
                if let Some(id) = self.enrichment.approval_by_call.get(&call_id) {
                    if let Some(fact) = self.enrichment.approvals.get_mut(id) {
                        fact.pending = false;
                    }
                }
                return true;
            }
            return false;
        }
        if request_id.is_empty() {
            return false;
        }
        let display_reason = payload
            .get("displayReason")
            .and_then(Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(call_id) = &call_id {
            self.enrichment
                .approval_by_call
                .insert(call_id.clone(), request_id.clone());
        }
        insert_bounded(
            &mut self.enrichment.approvals,
            request_id,
            ApprovalFact {
                tool_name: payload
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string(),
                call_id,
                reason: payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                display_reason,
                pending: true,
            },
        );
        true
    }

    fn apply_question_event(&mut self, payload: &Value) -> bool {
        if !crate::address_matches(payload, &self.address) {
            return false;
        }
        let call_id = payload
            .get("callId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(call_id) = call_id else {
            return false;
        };
        let state = payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        match state {
            "pending" => {
                if payload.get("timed").and_then(Value::as_bool) == Some(true)
                    || payload.get("remainingMs").is_some()
                {
                    self.enrichment.question_timed.insert(call_id.clone());
                }
                let questions = parse_questions(payload.get("questions"));
                if !questions.is_empty() {
                    insert_bounded(&mut self.enrichment.question_questions, call_id, questions);
                }
                true
            }
            "submitted" => {
                if let Some(answers) = parse_answers(payload.get("answers")) {
                    insert_bounded(
                        &mut self.enrichment.question_answers,
                        call_id.clone(),
                        answers,
                    );
                } else {
                    self.enrichment.question_submitted.insert(call_id.clone());
                }
                self.enrichment.question_continued.remove(&call_id);
                true
            }
            "settled" => {
                let answers = parse_answers(payload.get("answers")).unwrap_or_default();
                insert_bounded(
                    &mut self.enrichment.question_answers,
                    call_id.clone(),
                    answers,
                );
                self.enrichment.question_continued.remove(&call_id);
                self.enrichment.question_submitted.remove(&call_id);
                true
            }
            "continued" => {
                self.enrichment.question_continued.insert(call_id.clone());
                self.enrichment.question_cancelled.remove(&call_id);
                self.enrichment.question_submitted.remove(&call_id);
                true
            }
            "expired" => {
                self.enrichment.question_continued.insert(call_id);
                true
            }
            "cancelled" | "withdrawn" => {
                self.enrichment.question_cancelled.insert(call_id);
                true
            }
            _ => false,
        }
    }

    fn apply_projection_event(&mut self, payload: &Value) -> bool {
        if !crate::address_matches(payload, &self.address) {
            return false;
        }
        let mut projection = QuestionProjection::default();
        if let Some(active) = payload.get("active").and_then(Value::as_array) {
            for entry in active {
                let Some(call_id) = entry.get("callId").and_then(Value::as_str) else {
                    continue;
                };
                projection
                    .active
                    .push(nano_decisions::model::ProjectedQuestion {
                        call_id: call_id.to_string(),
                        questions: parse_questions(entry.get("questions")),
                        state: entry
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("open")
                            .to_string(),
                    });
                if entry.get("state").and_then(Value::as_str) == Some("continued") {
                    self.enrichment
                        .question_continued
                        .insert(call_id.to_string());
                    self.enrichment.question_cancelled.remove(call_id);
                    self.enrichment.question_submitted.remove(call_id);
                }
            }
        }
        if let Some(settled) = payload.get("settled").and_then(Value::as_array) {
            for entry in settled {
                let Some(call_id) = entry.get("callId").and_then(Value::as_str) else {
                    continue;
                };
                let answers = parse_answers(entry.get("answers")).unwrap_or_default();
                projection
                    .settled
                    .push(nano_decisions::model::SettledQuestion {
                        call_id: call_id.to_string(),
                        answers: answers.clone(),
                    });
                insert_bounded(
                    &mut self.enrichment.question_answers,
                    call_id.to_string(),
                    answers,
                );
                self.enrichment.question_continued.remove(call_id);
            }
        }
        let changed = projection != self.enrichment.projection;
        self.enrichment.projection = projection;
        changed
    }
}

fn assistant_fact(data: &Value) -> AssistantFact {
    let content = data
        .get("message")
        .and_then(|message| message.get("content"));
    let (text, reasoning) = text_and_reasoning(content);
    AssistantFact {
        text,
        reasoning,
        reasoning_ms: reasoning_duration_ms(data.get("stream")),
    }
}

fn text_and_reasoning(content: Option<&Value>) -> (String, String) {
    let Some(blocks) = content.and_then(Value::as_array) else {
        return (String::new(), String::new());
    };
    let mut text = String::new();
    let mut reasoning = String::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            Some("reasoning") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    reasoning.push_str(value);
                }
            }
            Some("tool-call") | Some("tool-addition") | Some("tool-removal") => {}
            Some("image") => text.push_str("[image]"),
            Some("file") => text.push_str("[file]"),
            Some(other) => {
                text.push('[');
                text.push_str(other);
                text.push(']');
            }
            None => text.push_str("[unknown]"),
        }
    }
    (text, reasoning)
}

fn reasoning_duration_ms(stream: Option<&Value>) -> Option<u64> {
    let records = stream?.as_array()?;
    let mut first: Option<u64> = None;
    let mut last: Option<u64> = None;
    for record in records {
        match record.get("type").and_then(Value::as_str) {
            Some("reasoning-chunks") => {
                let time0 = record.get("time0").and_then(Value::as_u64)?;
                let total: u64 = record
                    .get("dt")
                    .and_then(Value::as_array)
                    .map(|deltas| deltas.iter().filter_map(Value::as_u64).sum())
                    .unwrap_or(0);
                first = Some(first.map_or(time0, |value| value.min(time0)));
                last = Some(last.map_or(time0 + total, |value| value.max(time0 + total)));
            }
            Some("chunk") => {
                let chunk = record.get("chunk")?;
                if chunk.get("type").and_then(Value::as_str) == Some("reasoning-delta") {
                    let time = record.get("time").and_then(Value::as_u64)?;
                    first = Some(first.map_or(time, |value| value.min(time)));
                    last = Some(last.map_or(time, |value| value.max(time)));
                }
            }
            _ => {}
        }
    }
    match (first, last) {
        (Some(first), Some(last)) if last > first => Some(last - first),
        _ => None,
    }
}

fn tool_result_fact(data: &Value) -> Option<ToolResultFact> {
    let message = data.get("message")?;
    let call_id = message
        .get("toolCallId")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
                .and_then(|block| block.get("toolCallId"))
                .and_then(Value::as_str)
        })?
        .to_string();
    let error = data.get("error").map(|error| crate::tool::ToolErrorFact {
        name: error
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("ToolError")
            .to_string(),
        code: error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
        reason: error
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    });
    let text = message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        });
    Some(ToolResultFact {
        call_id,
        meta: data.get("meta").cloned(),
        error,
        text,
    })
}

fn reply_answers(data: &Value) -> Option<Vec<AnswerItem>> {
    let text = data
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|block| block.get("text").and_then(Value::as_str))?;
    let value: Value = serde_json::from_str(text).ok()?;
    parse_answers(value.get("answers"))
}

fn is_pending_question(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get("pending").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn insert_bounded<K: Ord, V>(map: &mut BTreeMap<K, V>, key: K, value: V) {
    map.insert(key, value);
    while map.len() > MAX_ENRICHMENT {
        map.pop_first();
    }
}

fn fingerprint(entry: &Entry) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entry.identity.hash(&mut hasher);
    match &entry.kind {
        EntryKind::User { text } => {
            0u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        EntryKind::Assistant {
            text,
            reasoning,
            reasoning_ms,
            interrupted,
        } => {
            1u8.hash(&mut hasher);
            text.hash(&mut hasher);
            reasoning.hash(&mut hasher);
            reasoning_ms.hash(&mut hasher);
            interrupted.hash(&mut hasher);
        }
        EntryKind::Notice { text, is_error } => {
            2u8.hash(&mut hasher);
            text.hash(&mut hasher);
            is_error.hash(&mut hasher);
        }
        EntryKind::Context { text, bash } => {
            3u8.hash(&mut hasher);
            text.hash(&mut hasher);
            bash.hash(&mut hasher);
        }
        EntryKind::Tool(record) => {
            4u8.hash(&mut hasher);
            record.call_id.hash(&mut hasher);
            record.name.hash(&mut hasher);
            record.status.hash(&mut hasher);
            record.display_status.hash(&mut hasher);
            record.arguments.hash(&mut hasher);
            record.result.hash(&mut hasher);
            record
                .result_fact
                .as_ref()
                .and_then(|fact| fact.meta.as_ref())
                .map(Value::to_string)
                .hash(&mut hasher);
            record
                .approval
                .as_ref()
                .map(|approval| (approval.asked, approval.outcome))
                .hash(&mut hasher);
            match &record.display {
                crate::tool::ToolDisplay::Bash { output, exit, .. } => {
                    output.hash(&mut hasher);
                    exit.hash(&mut hasher);
                }
                crate::tool::ToolDisplay::File { diff, .. } => {
                    diff.as_ref()
                        .map(|diff| {
                            (
                                diff.path.clone(),
                                diff.added,
                                diff.deleted,
                                diff.state,
                                diff.total_lines,
                                diff.coarse,
                            )
                        })
                        .hash(&mut hasher);
                }
                crate::tool::ToolDisplay::AskUser { record } => {
                    record.state.hash(&mut hasher);
                    record.answers.hash(&mut hasher);
                    record.questions.len().hash(&mut hasher);
                }
                crate::tool::ToolDisplay::Generic => {}
            }
        }
        EntryKind::Preview(preview) => {
            5u8.hash(&mut hasher);
            preview.text.hash(&mut hasher);
            preview.reasoning.hash(&mut hasher);
            preview.reasoning_ms.hash(&mut hasher);
            preview.gap.hash(&mut hasher);
            preview.truncated.hash(&mut hasher);
            preview.tools.len().hash(&mut hasher);
        }
        EntryKind::Echo { text, state } => {
            6u8.hash(&mut hasher);
            text.hash(&mut hasher);
            state.hash(&mut hasher);
        }
    }
    hasher.finish()
}
