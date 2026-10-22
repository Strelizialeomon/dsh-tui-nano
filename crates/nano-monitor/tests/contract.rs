//! S10 monitor display contracts: official projections only, honest delivery
//! receipts, UI-only stream facts, and visible budgets.

use nano_monitor::{DeliveryState, MonitorMode, MonitorModel, MonitorStream};
use serde_json::json;

const ROOT: &str = "root";

fn delivery(state: &str) -> serde_json::Value {
    json!({
        "messageId": "message-1",
        "mode": "notify",
        "state": state,
        "at": 2,
        "lines": 3,
        "bytes": 120,
        "estimatedTokens": 30,
        "firstOffset": 0,
        "lastOffset": 20,
        "omittedLines": 1,
        "textPreview": "[monitor] job=bash-a matched=3"
    })
}

fn monitor(stream: &str, listening: bool) -> serde_json::Value {
    json!({
        "id": "bash-a",
        "jobId": "bash-a",
        "mode": "notify",
        "listening": listening,
        "from": "new",
        "initialOffset": 12,
        "offset": 24,
        "unreadBytes": 12,
        "skippedBytes": 12,
        "filter": {"pattern": "error", "regex": false},
        "mergeWindowMs": 800,
        "budgets": {
            "maxNotifications": 50,
            "minIntervalMs": 1000,
            "maxLinesPerNotification": 20,
            "maxNotificationBytes": 8192,
            "maxNotificationTokens": 2048,
            "expiresAt": 1000000
        },
        "used": {"notifications": 1, "lines": 3, "bytes": 120, "estimatedTokens": 30},
        "exhausted": false,
        "stream": stream,
        "jobStatus": "running",
        "lossy": false,
        "gaps": 0,
        "heldMatches": 0,
        "heldBytes": 0,
        "droppedMatches": 0,
        "deliveries": [delivery("accepted")],
        "role": "user",
        "source": "monitor",
        "createdAt": 1,
        "updatedAt": 2
    })
}

fn list(seq: u64, monitor: serde_json::Value) -> serde_json::Value {
    json!({
        "address": {"rootSessionId": ROOT},
        "seq": seq,
        "monitors": [monitor]
    })
}

#[test]
fn a_list_projection_keeps_the_required_identity_and_budget_facts() {
    let mut model = MonitorModel::new(ROOT);
    model
        .apply_list(&list(1, monitor("watching", true)))
        .unwrap();
    let view = model.selected_monitor().unwrap();
    assert_eq!(view.job_id, "bash-a");
    assert_eq!(view.mode, MonitorMode::Notify);
    assert_eq!(view.stream, MonitorStream::Watching);
    assert_eq!(view.filter.as_ref().unwrap().label(), "字面「error」");
    assert_eq!(
        view.identity_label(),
        "role=user / source=monitor（user-role 消息，不是 Agent 授权）"
    );
    assert_eq!(view.last_delivery().unwrap().state, DeliveryState::Accepted);
    assert!(view.budget_label().contains("通知 1/50"));
}

#[test]
fn a_stale_list_cannot_roll_back_a_newer_live_projection() {
    let mut model = MonitorModel::new(ROOT);
    model.apply_event(
        "monitor.updated",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 5,
            "monitor": monitor("stream-ended", false)
        }),
    );
    assert_eq!(model.last_event_seq(), 5);
    let applied = model
        .apply_list(&list(4, monitor("watching", true)))
        .unwrap();
    assert!(!applied);
    assert_eq!(
        model.selected_monitor().unwrap().stream,
        MonitorStream::StreamEnded
    );
}

#[test]
fn live_events_upsert_by_monitor_identity() {
    let mut model = MonitorModel::new(ROOT);
    model.apply_event(
        "monitor.updated",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 1,
            "monitor": monitor("watching", true)
        }),
    );
    assert_eq!(model.len(), 1);
    model.apply_event(
        "monitor.updated",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 2,
            "monitor": monitor("stopped", false)
        }),
    );
    assert_eq!(model.len(), 1);
    assert_eq!(
        model.selected_monitor().unwrap().stream,
        MonitorStream::Stopped
    );
}

#[test]
fn terminal_stream_labels_never_claim_the_condition_was_met() {
    for (wire, expected) in [
        ("stream-ended", "stream-ended（流结束；不代表条件满足）"),
        ("failed", "failed（被观察任务失败）"),
        ("stopped", "stopped（被观察任务被终止）"),
    ] {
        let view: nano_monitor::MonitorView = serde_json::from_value(monitor(wire, false)).unwrap();
        assert_eq!(view.stream.label(), expected);
        assert!(view.stream.is_terminal());
    }
}

#[test]
fn exhausted_budget_is_visible_and_keeps_held_matches() {
    let mut value = monitor("watching", false);
    value["exhausted"] = json!(true);
    value["exhaustReason"] = json!("notifications");
    value["heldMatches"] = json!(7);
    value["used"] = json!({"notifications": 50, "lines": 80, "bytes": 900, "estimatedTokens": 225});
    let view: nano_monitor::MonitorView = serde_json::from_value(value).unwrap();
    let label = view.budget_label();
    assert!(label.contains("预算已用尽"));
    assert!(label.contains("暂存未投递 7 行"));
}

#[test]
fn inserted_and_claimed_are_not_proven_receipts() {
    for state in ["submitted", "inserted", "claimed", "unknown"] {
        let value: nano_monitor::MonitorDelivery = serde_json::from_value(delivery(state)).unwrap();
        assert!(!value.state.is_proven(), "{state} must not prove receipt");
    }
    let accepted: nano_monitor::MonitorDelivery =
        serde_json::from_value(delivery("accepted")).unwrap();
    assert!(accepted.state.is_proven());
}

#[test]
fn discarded_and_failed_are_failures() {
    for state in ["discarded", "failed"] {
        let value: nano_monitor::MonitorDelivery = serde_json::from_value(delivery(state)).unwrap();
        assert!(value.state.is_failure(), "{state} must be a failure");
    }
}

#[test]
fn a_monitor_event_without_a_projection_is_an_explicit_error() {
    let mut model = MonitorModel::new(ROOT);
    let changed = model.apply_event(
        "monitor.updated",
        &json!({"address": {"rootSessionId": ROOT}, "seq": 1}),
    );
    assert!(changed);
    assert!(model.last_error().is_some());
}

#[test]
fn a_foreign_address_is_ignored_instead_of_merging_sessions() {
    let mut model = MonitorModel::new(ROOT);
    let changed = model.apply_event(
        "monitor.updated",
        &json!({
            "address": {"rootSessionId": "other"},
            "seq": 1,
            "monitor": monitor("watching", true)
        }),
    );
    assert!(!changed);
    assert!(model.is_empty());
}

#[test]
fn clearing_the_model_drops_every_projection_and_sequence() {
    let mut model = MonitorModel::new(ROOT);
    model
        .apply_list(&list(3, monitor("watching", true)))
        .unwrap();
    model.clear();
    assert!(model.is_empty());
    assert_eq!(model.last_event_seq(), 0);
    assert!(model.last_notice().is_none());
}

#[test]
fn the_protocol_monitor_fixture_parses_as_this_type() {
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../../../protocol/nano-protocol.json")).unwrap();
    let monitor = &contract["fixtures"]["monitorEvent"]["params"]["payload"]["monitor"];
    let view: nano_monitor::MonitorView = serde_json::from_value(monitor.clone()).unwrap();
    assert_eq!(view.job_id, "bash-0123456789abcdef");
    assert_eq!(view.stream, MonitorStream::Watching);
    assert_eq!(view.last_delivery().unwrap().state, DeliveryState::Accepted);
    assert_eq!(
        view.identity_label(),
        "role=user / source=monitor（user-role 消息，不是 Agent 授权）"
    );
}

#[test]
fn creating_a_second_monitor_keeps_the_first_visible() {
    let mut model = MonitorModel::new(ROOT);
    model
        .apply_list(&list(1, monitor("watching", true)))
        .unwrap();
    let mut second = monitor("watching", true);
    second["id"] = json!("bash-b");
    second["jobId"] = json!("bash-b");
    model.apply_event(
        "monitor.updated",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 2,
            "monitor": second
        }),
    );
    assert_eq!(model.len(), 2);
    assert!(model.monitor_for_job("bash-a").is_some());
    assert!(model.select_job("bash-b"));
    assert_eq!(model.selected_monitor().unwrap().job_id, "bash-b");
}
