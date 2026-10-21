//! Contract tests for the reading layer: S02 identities plus the durable
//! events that enrich them, the D04 view split, and address isolation.

use nano_conversation::{ConversationModel, Delivery, SessionAddress};
use nano_transcript::{
    EntryKind, RecordedQuestionState, RenderCache, ScrollState, ToolDisplay, TranscriptModel,
    TranscriptSource,
};
use serde_json::{json, Value};
use std::time::Instant;

fn root() -> SessionAddress {
    SessionAddress::root("root")
}

fn payload(seq: u64, event_type: &str, data: Value) -> Value {
    json!({
        "address": {"rootSessionId": "root"},
        "seq": seq,
        "type": event_type,
        "data": data,
    })
}

fn apply(model: &mut ConversationModel, transcript: &mut TranscriptModel, payload: &Value) {
    assert!(model.apply_event("session.event", payload));
    transcript.apply_event("session.event", payload);
}

#[test]
fn assistant_reasoning_and_duration_come_from_the_durable_event() {
    let mut model = ConversationModel::new(root());
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    apply(
        &mut model,
        &mut transcript,
        &payload(
            1,
            "assistant/message",
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "a-1",
                    "role": "assistant",
                    "content": [
                        {"type": "reasoning", "text": "think"},
                        {"type": "text", "text": "answer"}
                    ]
                },
                "stream": [
                    {"type": "reasoning-chunks", "time0": 1000, "index": 0, "dt": [100, 50], "texts": ["think"]}
                ]
            }),
        ),
    );
    transcript.sync(&model, Instant::now());
    let entry = transcript
        .entries()
        .iter()
        .find(|entry| entry.identity == "a-1")
        .unwrap();
    let EntryKind::Assistant {
        text,
        reasoning,
        reasoning_ms,
        ..
    } = &entry.kind
    else {
        panic!("expected assistant");
    };
    assert_eq!(text, "answer");
    assert_eq!(reasoning, "think");
    assert_eq!(*reasoning_ms, Some(150));
}

#[test]
fn tool_result_metadata_enriches_the_same_s02_call_identity() {
    let mut model = ConversationModel::new(root());
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    apply(
        &mut model,
        &mut transcript,
        &payload(
            1,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c-1", "name": "write", "arguments": "{\"file_path\":\"a.txt\",\"content\":\"x\"}"}),
        ),
    );
    apply(
        &mut model,
        &mut transcript,
        &payload(
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
                    "content": [{"type": "text", "text": "Updated file"}]
                },
                "meta": {"diffs": [{"path": "a.txt", "oldText": null, "newText": "x"}]}
            }),
        ),
    );
    transcript.sync(&model, Instant::now());
    let entry = transcript
        .entries()
        .iter()
        .find(|entry| entry.identity == "call-c-1")
        .unwrap();
    let EntryKind::Tool(record) = &entry.kind else {
        panic!("expected tool");
    };
    let ToolDisplay::File { diff, .. } = &record.display else {
        panic!("expected file");
    };
    let diff = diff.as_ref().unwrap();
    assert_eq!(diff.added, Some(1));
    assert_eq!(diff.deleted, None);
    assert_eq!(diff.stat_label(), "+1");
}

#[test]
fn approval_audit_attaches_the_outcome_to_the_tool_row() {
    let mut model = ConversationModel::new(root());
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    apply(
        &mut model,
        &mut transcript,
        &payload(
            1,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c-1", "name": "bash", "arguments": "{\"command\":\"echo hi\"}"}),
        ),
    );
    apply(
        &mut model,
        &mut transcript,
        &payload(
            2,
            "approval/asked",
            json!({"id": "ap-1", "toolName": "bash", "callId": "c-1", "reason": "sandbox"}),
        ),
    );
    apply(
        &mut model,
        &mut transcript,
        &payload(
            3,
            "approval/decided",
            json!({"id": "ap-1", "outcome": "rejected"}),
        ),
    );
    transcript.sync(&model, Instant::now());
    let entry = transcript
        .entries()
        .iter()
        .find(|entry| entry.identity == "call-c-1")
        .unwrap();
    let EntryKind::Tool(record) = &entry.kind else {
        panic!("expected tool");
    };
    let approval = record.approval.as_ref().unwrap();
    assert!(approval.asked);
    assert_eq!(
        approval.outcome,
        Some(nano_decisions::ApprovalOutcome::Rejected)
    );
    assert_eq!(approval.outcome_label(), "已拒绝");
}

#[test]
fn a_timed_question_projection_and_reply_settle_the_tool_row() {
    let mut model = ConversationModel::new(root());
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    apply(
        &mut model,
        &mut transcript,
        &payload(
            1,
            "tool/call",
            json!({
                "turn": 1,
                "step": 1,
                "callId": "c-1",
                "name": "ask_user_question",
                "arguments": "{\"timeout\":120,\"questions\":[{\"id\":\"q1\",\"question\":\"Pick\",\"options\":[{\"label\":\"A\"}]}]}"
            }),
        ),
    );
    transcript.apply_event(
        "decisions.projection",
        &json!({
            "address": {"rootSessionId": "root"},
            "active": [],
            "settled": [{"callId": "c-1", "answers": [{"id": "q1", "selected": ["A"]}]}]
        }),
    );
    transcript.sync(&model, Instant::now());
    let entry = transcript
        .entries()
        .iter()
        .find(|entry| entry.identity == "call-c-1")
        .unwrap();
    let EntryKind::Tool(record) = &entry.kind else {
        panic!("expected tool");
    };
    let ToolDisplay::AskUser { record } = &record.display else {
        panic!("expected question");
    };
    assert_eq!(record.state, RecordedQuestionState::Settled);
    assert!(record.timed);
    assert_eq!(record.answers[0].selected, vec!["A"]);
    assert!(record.can_review());
}

#[test]
fn a_read_only_view_never_expands() {
    let mut model = ConversationModel::new(root());
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::ReadOnly);
    apply(
        &mut model,
        &mut transcript,
        &payload(
            1,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c-1", "name": "bash", "arguments": "{\"command\":\"echo hi\"}"}),
        ),
    );
    transcript.sync(&model, Instant::now());
    assert!(!transcript.toggle("call-c-1"));
    assert!(!transcript.is_expanded("call-c-1"));
}

#[test]
fn events_for_another_address_do_not_enrich_this_transcript() {
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    let other = json!({
        "address": {"rootSessionId": "other"},
        "seq": 1,
        "type": "assistant/message",
        "data": {"turn": 1, "step": 1, "message": {"id": "a", "content": [{"type": "text", "text": "x"}]}}
    });
    assert!(!transcript.apply_event("session.event", &other));
    let mut cache = RenderCache::new();
    let mut scroll = ScrollState::default();
    let rendered = nano_transcript::render(&transcript, &mut cache, &mut scroll, 80, 20);
    assert_eq!(rendered.lines.len(), 0);
}

#[test]
fn the_same_renderer_serves_both_d04_views() {
    let address = root();
    let interactive =
        nano_transcript::TranscriptView::new(address.clone(), TranscriptSource::Interactive);
    let read_only = nano_transcript::TranscriptView::new(address, TranscriptSource::ReadOnly);
    assert_ne!(interactive.source(), read_only.source());
    assert_eq!(interactive.source(), TranscriptSource::Interactive);
    assert_eq!(read_only.source(), TranscriptSource::ReadOnly);
}

#[test]
fn a_submission_echo_is_display_only_and_does_not_become_history() {
    let mut model = ConversationModel::new(root());
    model.begin_submit("hello", Delivery::Queue).unwrap();
    let mut transcript = TranscriptModel::new(root(), TranscriptSource::Interactive);
    transcript.sync(&model, Instant::now());
    let entry = transcript.entries().first().unwrap();
    assert!(matches!(entry.kind, EntryKind::Echo { .. }));
    assert!(model.history().is_empty());
}

#[test]
fn the_reading_anchor_survives_preview_to_message_handoff() {
    let mut model = ConversationModel::new(root());
    let mut view = nano_transcript::TranscriptView::new(root(), TranscriptSource::Interactive);
    model.apply_event(
        "conversation.stream",
        &json!({
            "address": {"rootSessionId": "root"},
            "frame": {"type": "start", "attemptId": "a-1", "revision": 1, "turn": 1, "step": 1}
        }),
    );
    model.apply_event(
        "conversation.stream",
        &json!({
            "address": {"rootSessionId": "root"},
            "frame": {
                "type": "chunk", "attemptId": "a-1", "revision": 1, "index": 0,
                "chunk": {"type": "text-delta", "index": 0, "text": "partial"}
            }
        }),
    );
    view.sync(&model, Instant::now());
    assert!(view
        .entries()
        .iter()
        .any(|entry| entry.identity == "preview-a-1"));
    view.scroll_mut().set_anchor("preview-a-1", 0);
    let event = payload(
        2,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": {
                "id": "m-1",
                "role": "assistant",
                "content": [{"type": "text", "text": "final"}]
            },
            "stream": []
        }),
    );
    model.apply_event("session.event", &event);
    view.apply_event("session.event", &event);
    view.sync(&model, Instant::now());
    assert_eq!(
        view.scroll()
            .anchor()
            .map(|anchor| anchor.identity.as_str()),
        Some("m-1")
    );
}
