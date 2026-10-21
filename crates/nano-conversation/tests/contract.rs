//! Cross-check the conversation wire shapes against the shared protocol file.

use serde_json::Value;

use nano_conversation::{parse_conversation_event, parse_session_event, ConversationEvent};

const CONTRACT_TEXT: &str = include_str!("../../../protocol/nano-protocol.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_TEXT).expect("protocol contract is valid JSON")
}

#[test]
fn submission_limit_matches_the_contract() {
    let contract = contract();
    assert_eq!(
        contract["limits"]["maxSubmitTextBytes"].as_u64(),
        Some(nano_conversation::MAX_SUBMIT_TEXT_BYTES as u64)
    );
    assert_eq!(
        contract["limits"]["maxReplayEvents"].as_u64(),
        Some(nano_conversation::MAX_REPLAY_EVENTS)
    );
}

#[test]
fn conversation_event_fixture_parses() {
    let contract = contract();
    let params = &contract["fixtures"]["conversationEvent"]["params"];
    let kind = params["kind"].as_str().unwrap();
    let event = parse_conversation_event(kind, &params["payload"]).unwrap();
    match event {
        ConversationEvent::Status { address, status } => {
            assert_eq!(address.root_session_id, "session-fixture");
            assert_eq!(status, nano_conversation::ConversationStatus::Running);
        }
        other => panic!("expected status event, got {other:?}"),
    }
}

#[test]
fn session_event_fixture_parses() {
    let contract = contract();
    let payload = &contract["fixtures"]["event"]["params"]["payload"];
    let event = parse_session_event(payload).unwrap();
    assert_eq!(event.address.root_session_id, "session-fixture");
    assert_eq!(event.event_type, "permission/preset");
}

#[test]
fn submit_fixture_has_the_required_request_shape() {
    let contract = contract();
    let params = &contract["fixtures"]["conversationSubmit"]["params"];
    assert_eq!(params["delivery"], "queue");
    assert!(params["requestId"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(params["content"][0]["type"], "text");
    assert!(params["address"]["rootSessionId"].as_str().is_some());
}

#[test]
fn every_event_kind_is_covered_by_the_contract_shape() {
    let contract = contract();
    let kinds: Vec<&str> = contract["eventKinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    // Exact set/order, not a subset: a future unknown kind must fail this
    // contract test instead of silently widening what the conversation model
    // accepts.
    assert_eq!(
        kinds,
        vec![
            "session.event",
            "conversation.status",
            "conversation.inbox",
            "conversation.stream",
            "schedule.changed",
            "controls.model",
            "controls.permission",
            "sessions.context",
            "skills.changed",
            "instructions.changed",
            "jobs.event",
            "jobs.output"
        ]
    );
    for (owner, kind) in [
        ("S04", "controls.model"),
        ("S04", "controls.permission"),
        ("S07", "sessions.context"),
        ("S05", "skills.changed"),
        ("S05", "instructions.changed"),
        ("S08", "jobs.event"),
        ("S08", "jobs.output"),
        ("S11", "schedule.changed"),
    ] {
        let event = contract["eventKinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == kind)
            .unwrap();
        assert_eq!(event["owner"], owner, "{kind} owner");
    }
}
