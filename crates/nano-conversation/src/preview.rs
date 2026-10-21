//! Live assistant-stream previews.
//!
//! A preview is presentation data, never history. It is replaced by the
//! durable `assistant/message`, retired when the attempt settles, and dropped
//! on reconnect where the durable log is the only baseline.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::events::StreamFrame;
use crate::{MAX_PREVIEW_BYTES, MAX_TOTAL_PREVIEW_BYTES};

/// One tool call assembled from dense `tool-call-delta` chunks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolPreview {
    /// Chunk-local block index.
    pub index: u64,
    /// Provider-issued call identity, when seen.
    pub call_id: Option<String>,
    /// Tool name, when seen.
    pub name: Option<String>,
    /// Raw JSON arguments accumulated verbatim.
    pub arguments: String,
    /// Whether a `block-end` completed this call.
    pub complete: bool,
}

/// One live attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Attempt identity.
    pub attempt_id: String,
    /// Revision within the Agent lifecycle.
    pub revision: u64,
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// Visible text accumulated from text deltas.
    pub text: String,
    /// Reasoning text, kept separate from visible text.
    pub reasoning: String,
    /// Tool calls by block index.
    pub tools: BTreeMap<u64, ToolPreview>,
    /// Next expected dense chunk index.
    pub expected_index: u64,
    /// A dense-index or revision gap was observed.
    pub gap: bool,
    /// The preview hit its byte bound.
    pub truncated: bool,
    /// Bytes retained for this attempt.
    pub bytes: usize,
    /// Whether the attempt has emitted its terminal finish.
    pub finished: bool,
    /// Settlement reported by the end frame.
    pub outcome: Option<PreviewOutcome>,
}

/// How an attempt settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewOutcome {
    /// A durable assistant message or attempt was committed.
    Committed,
    /// The attempt was abandoned without a durable settlement.
    Abandoned,
    /// The host did not report a recognizable settlement.
    Unknown,
}

/// A preview that left the live set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRetirement {
    /// Attempt identity.
    pub attempt_id: String,
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// Settlement.
    pub outcome: PreviewOutcome,
}

/// Read-only projection for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewView {
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
    /// Tool calls in block order.
    pub tools: Vec<ToolPreview>,
    /// Whether the preview is known to be incomplete.
    pub gap: bool,
    /// Whether the preview was cut by its byte bound.
    pub truncated: bool,
}

/// Bounded live preview set.
#[derive(Debug, Clone)]
pub struct PreviewAccumulator {
    previews: BTreeMap<String, Preview>,
    total_bytes: usize,
    max_bytes: usize,
    max_total_bytes: usize,
}

impl Default for PreviewAccumulator {
    fn default() -> Self {
        Self::new(MAX_PREVIEW_BYTES, MAX_TOTAL_PREVIEW_BYTES)
    }
}

impl PreviewAccumulator {
    /// Build with explicit bounds.
    pub fn new(max_bytes: usize, max_total_bytes: usize) -> Self {
        Self {
            previews: BTreeMap::new(),
            total_bytes: 0,
            max_bytes,
            max_total_bytes,
        }
    }

    /// Number of live attempts.
    pub fn len(&self) -> usize {
        self.previews.len()
    }

    /// Whether no attempt is live.
    pub fn is_empty(&self) -> bool {
        self.previews.is_empty()
    }

    /// Read-only views in turn/step order.
    pub fn views(&self) -> Vec<PreviewView> {
        let mut previews: Vec<&Preview> = self.previews.values().collect();
        previews.sort_by_key(|preview| (preview.turn, preview.step, preview.attempt_id.clone()));
        previews
            .into_iter()
            .map(|preview| PreviewView {
                attempt_id: preview.attempt_id.clone(),
                turn: preview.turn,
                step: preview.step,
                text: preview.text.clone(),
                reasoning: preview.reasoning.clone(),
                tools: preview.tools.values().cloned().collect(),
                gap: preview.gap,
                truncated: preview.truncated,
            })
            .collect()
    }

    /// Apply one live frame. Unknown attempts and unknown chunk types are
    /// ignored; a dense-index gap marks the preview incomplete.
    pub fn apply(&mut self, frame: &StreamFrame) -> Option<PreviewRetirement> {
        match frame {
            StreamFrame::Start {
                attempt_id,
                revision,
                turn,
                step,
            } => {
                self.remove(attempt_id);
                let preview = Preview {
                    attempt_id: attempt_id.clone(),
                    revision: *revision,
                    turn: *turn,
                    step: *step,
                    text: String::new(),
                    reasoning: String::new(),
                    tools: BTreeMap::new(),
                    expected_index: 0,
                    gap: false,
                    truncated: false,
                    bytes: 0,
                    finished: false,
                    outcome: None,
                };
                self.previews.insert(attempt_id.clone(), preview);
                None
            }
            StreamFrame::Chunk {
                attempt_id,
                revision,
                index,
                chunk,
                ..
            } => {
                let Some(preview) = self.previews.get_mut(attempt_id) else {
                    // Unknown attempt: live frames are ignored; its durable
                    // settlement still arrives through session.event.
                    return None;
                };
                if preview.revision != *revision {
                    // A new revision replaces the attempt. Rebuild from this
                    // frame; a non-zero first index means a real gap.
                    let turn = preview.turn;
                    let step = preview.step;
                    self.remove(attempt_id);
                    let gap = *index != 0;
                    self.previews.insert(
                        attempt_id.clone(),
                        Preview {
                            attempt_id: attempt_id.clone(),
                            revision: *revision,
                            turn,
                            step,
                            text: String::new(),
                            reasoning: String::new(),
                            tools: BTreeMap::new(),
                            expected_index: index + 1,
                            gap,
                            truncated: false,
                            bytes: 0,
                            finished: false,
                            outcome: None,
                        },
                    );
                    if !gap {
                        let max_bytes = self.max_bytes;
                        let max_total_bytes = self.max_total_bytes;
                        if let Some(next) = self.previews.get_mut(attempt_id) {
                            Self::apply_chunk(
                                next,
                                chunk,
                                max_bytes,
                                max_total_bytes,
                                &mut self.total_bytes,
                            );
                        }
                    }
                    return None;
                }
                if *index < preview.expected_index {
                    return None;
                }
                if *index > preview.expected_index {
                    preview.gap = true;
                    preview.expected_index = index + 1;
                    return None;
                }
                preview.expected_index = index + 1;
                if preview.gap {
                    return None;
                }
                let max_bytes = self.max_bytes;
                let max_total_bytes = self.max_total_bytes;
                Self::apply_chunk(
                    preview,
                    chunk,
                    max_bytes,
                    max_total_bytes,
                    &mut self.total_bytes,
                );
                self.enforce_bounds()
            }
            StreamFrame::End {
                attempt_id,
                outcome,
                ..
            } => {
                let preview = self.remove(attempt_id)?;
                Some(PreviewRetirement {
                    attempt_id: preview.attempt_id,
                    turn: preview.turn,
                    step: preview.step,
                    outcome: outcome_of(outcome),
                })
            }
        }
    }

    /// Retire any preview for a durable turn/step settlement.
    pub fn retire_turn_step(&mut self, turn: u64, step: u64) -> Vec<PreviewRetirement> {
        let ids: Vec<String> = self
            .previews
            .values()
            .filter(|preview| preview.turn == turn && preview.step == step)
            .map(|preview| preview.attempt_id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                self.remove(&id).map(|preview| PreviewRetirement {
                    attempt_id: preview.attempt_id,
                    turn: preview.turn,
                    step: preview.step,
                    outcome: PreviewOutcome::Committed,
                })
            })
            .collect()
    }

    /// Drop all previews, for example after a reconnect.
    pub fn clear(&mut self) {
        self.previews.clear();
        self.total_bytes = 0;
    }

    fn apply_chunk(
        preview: &mut Preview,
        chunk: &Value,
        max_bytes: usize,
        max_total_bytes: usize,
        total_bytes: &mut usize,
    ) {
        let Some(kind) = chunk.get("type").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "text-delta" => {
                if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                    append(preview, text, true, max_bytes, max_total_bytes, total_bytes);
                }
            }
            "reasoning-delta" => {
                if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                    append(
                        preview,
                        text,
                        false,
                        max_bytes,
                        max_total_bytes,
                        total_bytes,
                    );
                }
            }
            "tool-call-delta" => {
                let Some(index) = chunk.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let call_id = chunk.get("id").and_then(Value::as_str).map(str::to_string);
                let name = chunk
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let delta = chunk
                    .get("argumentsDelta")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let reserved = delta
                    .as_ref()
                    .map(|value| {
                        reserve_bytes(
                            preview,
                            value.len(),
                            max_bytes,
                            max_total_bytes,
                            total_bytes,
                        )
                    })
                    .unwrap_or(true);
                let mut truncated = false;
                {
                    let entry = preview.tools.entry(index).or_insert_with(|| ToolPreview {
                        index,
                        ..ToolPreview::default()
                    });
                    if let Some(call_id) = call_id {
                        entry.call_id = Some(call_id);
                    }
                    if let Some(name) = name {
                        entry.name = Some(name);
                    }
                    if let Some(delta) = delta {
                        if reserved {
                            entry.arguments.push_str(&delta);
                        } else {
                            truncated = true;
                        }
                    }
                }
                if truncated {
                    preview.truncated = true;
                }
            }
            "block-end" => {
                let Some(index) = chunk.get("index").and_then(Value::as_u64) else {
                    return;
                };
                if let Some(block) = chunk.get("block") {
                    let call_id = block.get("id").and_then(Value::as_str).map(str::to_string);
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let arguments = block
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let old = preview
                        .tools
                        .get(&index)
                        .map(|entry| entry.arguments.len())
                        .unwrap_or(0);
                    let new_len = arguments.as_ref().map_or(old, String::len);
                    let mut accepted = true;
                    if new_len > old {
                        // Keep the old arguments accounted while reserving the
                        // delta; a failed reserve leaves the old value intact.
                        accepted = reserve_bytes(
                            preview,
                            new_len - old,
                            max_bytes,
                            max_total_bytes,
                            total_bytes,
                        );
                    } else if new_len < old {
                        let shrink = old - new_len;
                        preview.bytes = preview.bytes.saturating_sub(shrink);
                        *total_bytes = total_bytes.saturating_sub(shrink);
                    }
                    let mut truncated = false;
                    {
                        let entry = preview.tools.entry(index).or_insert_with(|| ToolPreview {
                            index,
                            ..ToolPreview::default()
                        });
                        if let Some(call_id) = call_id {
                            entry.call_id = Some(call_id);
                        }
                        if let Some(name) = name {
                            entry.name = Some(name);
                        }
                        if let Some(arguments) = arguments {
                            if accepted {
                                entry.arguments = arguments;
                            } else {
                                truncated = true;
                            }
                        }
                        entry.complete = true;
                    }
                    if truncated {
                        preview.truncated = true;
                    }
                }
            }
            "finish" => preview.finished = true,
            // Unknown chunk types are ignored rather than interpreted.
            _ => {}
        }
    }

    fn remove(&mut self, attempt_id: &str) -> Option<Preview> {
        let preview = self.previews.remove(attempt_id)?;
        self.total_bytes = self.total_bytes.saturating_sub(preview.bytes);
        Some(preview)
    }

    /// Enforce the total bound globally; called after each applied chunk.
    fn enforce_bounds(&mut self) -> Option<PreviewRetirement> {
        // Per-attempt bounds are enforced while appending. The global bound is
        // enforced by refusing new bytes, so no attempt needs eviction here.
        None
    }
}

fn reserve_bytes(
    preview: &mut Preview,
    bytes: usize,
    max_bytes: usize,
    max_total_bytes: usize,
    total_bytes: &mut usize,
) -> bool {
    if preview.bytes + bytes > max_bytes || *total_bytes + bytes > max_total_bytes {
        return false;
    }
    preview.bytes += bytes;
    *total_bytes += bytes;
    true
}

fn append(
    preview: &mut Preview,
    text: &str,
    visible: bool,
    max_bytes: usize,
    max_total_bytes: usize,
    total_bytes: &mut usize,
) {
    if !reserve_bytes(preview, text.len(), max_bytes, max_total_bytes, total_bytes) {
        preview.truncated = true;
        return;
    }
    if visible {
        preview.text.push_str(text);
    } else {
        preview.reasoning.push_str(text);
    }
}

fn outcome_of(value: &Value) -> PreviewOutcome {
    match value.get("kind").and_then(Value::as_str) {
        Some("committed") => PreviewOutcome::Committed,
        Some("abandoned") => PreviewOutcome::Abandoned,
        _ => PreviewOutcome::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start(attempt: &str, turn: u64, step: u64) -> StreamFrame {
        StreamFrame::Start {
            attempt_id: attempt.into(),
            revision: 1,
            turn,
            step,
        }
    }

    fn chunk(attempt: &str, index: u64, value: Value) -> StreamFrame {
        StreamFrame::Chunk {
            attempt_id: attempt.into(),
            revision: 1,
            index,
            time: Some(index),
            chunk: value,
        }
    }

    #[test]
    fn text_deltas_merge_and_end_retires_the_preview() {
        let mut previews = PreviewAccumulator::default();
        assert!(previews.apply(&start("a", 1, 1)).is_none());
        previews.apply(&chunk(
            "a",
            0,
            json!({"type": "text-delta", "index": 0, "text": "hel"}),
        ));
        previews.apply(&chunk(
            "a",
            1,
            json!({"type": "text-delta", "index": 0, "text": "lo"}),
        ));
        assert_eq!(previews.views()[0].text, "hello");
        let retired = previews
            .apply(&StreamFrame::End {
                attempt_id: "a".into(),
                revision: 1,
                index: 2,
                outcome: json!({"kind": "committed", "eventType": "assistant/message", "seq": 9}),
            })
            .unwrap();
        assert_eq!(retired.outcome, PreviewOutcome::Committed);
        assert!(previews.is_empty());
    }

    #[test]
    fn unknown_attempt_chunks_are_ignored() {
        let mut previews = PreviewAccumulator::default();
        previews.apply(&chunk(
            "ghost",
            0,
            json!({"type": "text-delta", "text": "x"}),
        ));
        assert!(previews.is_empty());
    }

    #[test]
    fn dense_index_gap_marks_preview_incomplete() {
        let mut previews = PreviewAccumulator::default();
        previews.apply(&start("a", 1, 1));
        previews.apply(&chunk("a", 0, json!({"type": "text-delta", "text": "one"})));
        previews.apply(&chunk("a", 2, json!({"type": "text-delta", "text": "two"})));
        let view = &previews.views()[0];
        assert!(view.gap);
        assert_eq!(view.text, "one");
    }

    #[test]
    fn durable_settlement_retires_previews_for_that_step() {
        let mut previews = PreviewAccumulator::default();
        previews.apply(&start("a", 2, 3));
        let retired = previews.retire_turn_step(2, 3);
        assert_eq!(retired.len(), 1);
        assert!(previews.is_empty());
    }

    #[test]
    fn tool_argument_bytes_count_against_the_total_budget() {
        let mut previews = PreviewAccumulator::new(100, 150);
        previews.apply(&start("a", 1, 1));
        previews.apply(&chunk(
            "a",
            0,
            json!({"type": "tool-call-delta", "index": 0, "id": "c-1", "name": "bash", "argumentsDelta": "x".repeat(90)}),
        ));
        previews.apply(&chunk(
            "a",
            1,
            json!({"type": "tool-call-delta", "index": 1, "id": "c-2", "name": "bash", "argumentsDelta": "y".repeat(90)}),
        ));
        let view = &previews.views()[0];
        assert!(view.truncated);
        let total: usize = view.tools.iter().map(|tool| tool.arguments.len()).sum();
        assert!(total <= 150);
    }

    #[test]
    fn a_new_revision_rebuilds_the_attempt() {
        let mut previews = PreviewAccumulator::default();
        previews.apply(&start("a", 1, 1));
        previews.apply(&chunk("a", 0, json!({"type": "text-delta", "text": "old"})));
        previews.apply(&StreamFrame::Chunk {
            attempt_id: "a".into(),
            revision: 2,
            index: 0,
            time: Some(0),
            chunk: json!({"type": "text-delta", "text": "new"}),
        });
        let view = &previews.views()[0];
        assert_eq!(view.text, "new");
        assert!(!view.gap);
    }

    #[test]
    fn preview_bytes_are_bounded() {
        let mut previews = PreviewAccumulator::new(4, 8);
        previews.apply(&start("a", 1, 1));
        previews.apply(&chunk(
            "a",
            0,
            json!({"type": "text-delta", "text": "12345"}),
        ));
        let view = &previews.views()[0];
        assert!(view.truncated);
        assert!(view.text.is_empty() || view.text.len() <= 4);
    }
}
