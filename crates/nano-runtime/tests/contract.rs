//! Contract conformance: the Rust constants and wire types must match
//! `protocol/nano-protocol.json`, and every fixture must parse with this build.

use std::collections::BTreeSet;

use serde_json::Value;

use nano_runtime::frame::FrameReader;
use nano_runtime::protocol::{
    parse_incoming, Address, EventParams, FrameRules, Hello, ShutdownReport, StartupErrorParams,
    StreamEndedParams,
};
use nano_runtime::protocol::KNOWN_CAPABILITIES;
use nano_runtime::{
    ERROR_CODE_NAMES, EVENT_KINDS, GRACEFUL_SHUTDOWN_MS, IMPLEMENTED_METHODS, LIFECYCLE_STATES,
    LIFECYCLE_TRANSITIONS, MAX_ATTACHMENT_CHUNK_BYTES, MAX_FRAME_BYTES, MAX_PENDING_REQUESTS,
    MAX_REPLAY_EVENTS, SHUTDOWN_STEP_NAMES, STARTUP_ERROR_CODES, TERM_GRACE_MS,
};

const CONTRACT_TEXT: &str = include_str!("../../../protocol/nano-protocol.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_TEXT).expect("protocol contract is valid JSON")
}

#[test]
fn protocol_version_matches() {
    let contract = contract();
    assert_eq!(contract["protocol"]["major"].as_u64(), Some(u64::from(nano_runtime::PROTOCOL_MAJOR)));
    assert_eq!(contract["protocol"]["minor"].as_u64(), Some(u64::from(nano_runtime::PROTOCOL_MINOR)));
}

#[test]
fn limits_match() {
    let limits = &contract()["limits"];
    assert_eq!(limits["maxFrameBytes"].as_u64(), Some(MAX_FRAME_BYTES as u64));
    assert_eq!(limits["maxPendingRequests"].as_u64(), Some(MAX_PENDING_REQUESTS as u64));
    assert_eq!(
        limits["maxAttachmentChunkBytes"].as_u64(),
        Some(MAX_ATTACHMENT_CHUNK_BYTES as u64)
    );
    assert_eq!(limits["gracefulShutdownMs"].as_u64(), Some(GRACEFUL_SHUTDOWN_MS));
    assert_eq!(limits["termGraceMs"].as_u64(), Some(TERM_GRACE_MS));
    assert_eq!(limits["mountLockStaleMs"].as_u64(), Some(30_000));
    assert_eq!(limits["maxReplayEvents"].as_u64(), Some(MAX_REPLAY_EVENTS));
}

#[test]
fn capability_names_and_shutdown_steps_match() {
    let contract = contract();
    let names: BTreeSet<&str> = contract["capabilityNames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(names, KNOWN_CAPABILITIES.into_iter().collect::<BTreeSet<_>>());
    let steps: BTreeSet<&str> = contract["shutdownSteps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(steps, SHUTDOWN_STEP_NAMES.into_iter().collect::<BTreeSet<_>>());
    assert_eq!(
        contract["shapes"]["capability"]["required"].as_str(),
        Some("boolean?")
    );
}

#[test]
fn lifecycle_matches() {
    let contract = contract();
    let states: BTreeSet<&str> = contract["lifecycle"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(states, LIFECYCLE_STATES.into_iter().collect::<BTreeSet<_>>());
    let edges: BTreeSet<(&str, &str)> = contract["lifecycle"]["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| (edge[0].as_str().unwrap(), edge[1].as_str().unwrap()))
        .collect();
    assert_eq!(edges, LIFECYCLE_TRANSITIONS.into_iter().collect::<BTreeSet<_>>());
}

#[test]
fn error_codes_match() {
    let contract = contract();
    let codes: BTreeSet<(&str, i64)> = contract["errorCodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["name"].as_str().unwrap(), entry["code"].as_i64().unwrap()))
        .collect();
    assert_eq!(codes, ERROR_CODE_NAMES.into_iter().collect::<BTreeSet<_>>());
}

#[test]
fn startup_codes_and_event_kinds_match() {
    let contract = contract();
    let startup: BTreeSet<&str> = contract["startupErrorCodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(startup, STARTUP_ERROR_CODES.into_iter().collect::<BTreeSet<_>>());
    let kinds: BTreeSet<&str> = contract["eventKinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, EVENT_KINDS.into_iter().collect::<BTreeSet<_>>());
}

#[test]
fn implemented_methods_match() {
    let contract = contract();
    let methods = contract["methods"].as_array().unwrap();
    let implemented: BTreeSet<&str> = methods
        .iter()
        .filter(|method| method["status"] == "implemented")
        .map(|method| method["name"].as_str().unwrap())
        .collect();
    assert_eq!(implemented, IMPLEMENTED_METHODS.into_iter().collect::<BTreeSet<_>>());
    for method in methods {
        let name = method["name"].as_str().unwrap();
        assert!(name.starts_with("nano."), "{name} is outside nano.*");
        assert!(method["owner"].is_string(), "{name} needs an owner");
        assert!(
            method["direction"] == "client-to-host" || method["direction"] == "host-to-client",
            "{name} needs a direction"
        );
        match method["status"].as_str().unwrap() {
            "implemented" => assert!(
                method["owner"] == "S01" || method["owner"] == "S02" || method["owner"] == "S04",
                "{name} is implemented by an owner this contract test does not know"
            ),
            "reserved" => assert_ne!(method["owner"], "S01", "{name} is reserved but owned by S01"),
            other => panic!("unknown method status {other}"),
        }
    }
}

#[test]
fn fixtures_parse_with_rust_types() {
    let fixtures = &contract()["fixtures"];

    let response_line = serde_json::to_string(&fixtures["response"]).unwrap();
    let parsed = parse_incoming(
        &response_line,
        FrameRules {
            require_nonce: false,
            nonce: "",
        },
    )
    .unwrap();
    match parsed {
        nano_runtime::protocol::IncomingFrame::Response { id, result, error } => {
            assert_eq!(id.as_deref(), Some("req-1"));
            assert!(result.is_some());
            assert!(error.is_none());
        }
        other => panic!("expected response, got {other:?}"),
    }

    let error_line = serde_json::to_string(&fixtures["errorResponse"]).unwrap();
    match parse_incoming(
        &error_line,
        FrameRules {
            require_nonce: false,
            nonce: "",
        },
    )
    .unwrap()
    {
        nano_runtime::protocol::IncomingFrame::Response { error, .. } => {
            assert_eq!(error.unwrap().code, 1001);
        }
        other => panic!("expected error response, got {other:?}"),
    }

    let hello: Hello = serde_json::from_value(fixtures["hello"]["params"].clone()).unwrap();
    hello.validate("rt-fixture", 1).unwrap();
    assert_eq!(hello.root_session_id, "session-fixture");
    assert_eq!(hello.limits.max_frame_bytes, MAX_FRAME_BYTES as u64);

    let startup: StartupErrorParams =
        serde_json::from_value(fixtures["startupError"]["params"].clone()).unwrap();
    startup.validate().unwrap();
    assert_eq!(startup.code, "occupied");
    assert_eq!(startup.holder_pid, Some(4242));

    let event: EventParams = serde_json::from_value(fixtures["event"]["params"].clone()).unwrap();
    event.validate("rt-fixture", 1).unwrap();
    assert_eq!(event.sequence, 0);

    let stream_end: StreamEndedParams =
        serde_json::from_value(fixtures["streamEnded"]["params"].clone()).unwrap();
    stream_end.validate("rt-fixture", 1).unwrap();

    let report: ShutdownReport =
        serde_json::from_value(fixtures["shutdownResult"]["result"].clone()).unwrap();
    assert!(report.clean);
    assert!(report.steps.iter().any(|step| step.name == "persistence"));
    assert!(report.steps.iter().any(|step| step.name == "dispose"));
}

#[test]
fn request_fixture_matches_shape() {
    let fixtures = contract();
    let request = &fixtures["fixtures"]["request"];
    assert_eq!(request["method"], "nano.subscribe");
    assert!(request["params"]["address"]["rootSessionId"].is_string());
    let encoded = nano_runtime::protocol::encode_request(
        "req-1",
        "nano.subscribe",
        request["params"].clone(),
        None,
    )
    .unwrap();
    let mut reader = FrameReader::new(std::io::BufReader::new(std::io::Cursor::new(encoded)));
    let line = reader.read_frame().unwrap().unwrap();
    assert_eq!(line, serde_json::to_string(request).unwrap());
}

#[test]
fn address_round_trips() {
    let address = Address {
        root_session_id: "session-a".into(),
        child_session_id: None,
    };
    let value = serde_json::to_value(&address).unwrap();
    assert_eq!(value["rootSessionId"], "session-a");
    assert!(value.get("childSessionId").is_none());
}
