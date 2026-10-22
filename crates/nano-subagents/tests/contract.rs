use std::time::{Duration, Instant};

use nano_conversation::SessionAddress;
use nano_subagents::{
    ComposerGate, EntryOrigin, ExitTarget, Lamp, Lifecycle, LifecycleEvent, StopReason,
    SubagentState, CLEANUP_DELAY, DOCK_LIMIT,
};
use serde_json::json;

fn row(id: &str, mode: &str, activity: &str) -> serde_json::Value {
    json!({
        "id": id,
        "parentId": "root",
        "depth": 1,
        "mode": mode,
        "label": format!("task {id}"),
        "activity": activity,
        "hasChildren": false
    })
}

#[test]
fn snapshot_dock_keeps_official_order_and_limits_to_four() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [
                row("a", "continuable", "running"),
                row("b", "continuable", "inactive"),
                row("c", "one-shot", "inactive"),
                row("d", "external", "inactive"),
                row("e", "continuable", "running"),
            ],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();

    let (dock, overflow) = state.dock();
    assert_eq!(
        dock.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c", "d"]
    );
    assert_eq!(overflow, 1);
    assert_eq!(dock.len(), DOCK_LIMIT);
    // External rows are not local and never count toward the pill.
    assert_eq!(state.local_count(), 4);
}

#[test]
fn external_rows_are_not_navigable() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [row("a", "external", "inactive")],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    assert!(!state.enter_dock(0));
    assert!(state.mounted().is_none());
}

#[test]
fn one_shot_opens_read_only_and_unknown_resolves() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [
                row("one", "one-shot", "inactive"),
                row("mystery", "unknown", "inactive")
            ],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    assert!(state.enter_dock(0));
    // No official history read yet: the composer is resolving, not enabled.
    assert_eq!(state.mounted_gate(), Some(ComposerGate::Resolving));
    state
        .apply_description(&json!({
            "id": "one",
            "parentId": "root",
            "mode": "one-shot",
            "activity": "inactive",
            "parentLive": true
        }))
        .unwrap();
    assert_eq!(state.mounted_gate(), Some(ComposerGate::ReadOnly));
    state.exit();

    assert!(state.enter_dock(1));
    assert_eq!(state.mounted_gate(), Some(ComposerGate::Resolving));
    state
        .apply_description(&json!({
            "id": "mystery",
            "parentId": "root",
            "mode": "continuable",
            "label": "resolved task",
            "activity": "inactive",
            "parentLive": true
        }))
        .unwrap();
    assert_eq!(state.mounted_gate(), Some(ComposerGate::Enabled));
    assert_eq!(
        state
            .mounted()
            .unwrap()
            .address
            .child
            .as_ref()
            .unwrap()
            .child_session_id,
        "mystery"
    );
}

#[test]
fn parent_liveness_controls_the_composer() {
    assert_eq!(
        ComposerGate::resolve(
            nano_subagents::AgentMode::Continuable,
            true,
            nano_subagents::Activity::Inactive
        ),
        ComposerGate::Enabled
    );
    assert_eq!(
        ComposerGate::resolve(
            nano_subagents::AgentMode::Continuable,
            false,
            nano_subagents::Activity::Inactive
        ),
        ComposerGate::ReadOnly
    );
    assert_eq!(
        ComposerGate::resolve(
            nano_subagents::AgentMode::Continuable,
            false,
            nano_subagents::Activity::Running
        ),
        ComposerGate::StopOnly
    );
    assert!(ComposerGate::StopOnly.can_stop());
    assert!(!ComposerGate::StopOnly.can_send());
    assert!(!ComposerGate::ReadOnly.can_stop());
}

#[test]
fn terminal_reasons_map_to_lamps_and_unknown_stays_unproven() {
    let mut lifecycle = Lifecycle::new();
    let running = nano_subagents::AgentRow {
        id: "a".into(),
        parent_id: "root".into(),
        depth: 1,
        mode: nano_subagents::AgentMode::Continuable,
        label: None,
        activity: nano_subagents::Activity::Running,
        has_children: false,
        last_turn_completed: None,
        settled_ms: None,
        active_since: None,
        active_through: None,
        parent_live: None,
        seq: None,
    };
    assert_eq!(lifecycle.lamp(&running), Lamp::Running);

    lifecycle.apply(&LifecycleEvent::End {
        child_id: "a".into(),
        reason: StopReason::parse("aborted"),
    });
    assert_eq!(lifecycle.lamp(&running), Lamp::Warned);

    lifecycle.apply(&LifecycleEvent::End {
        child_id: "a".into(),
        reason: StopReason::parse("completed"),
    });
    assert_eq!(lifecycle.lamp(&running), Lamp::Completed);

    lifecycle.apply(&LifecycleEvent::End {
        child_id: "a".into(),
        reason: StopReason::parse("error"),
    });
    assert_eq!(lifecycle.lamp(&running), Lamp::Error);

    lifecycle.apply(&LifecycleEvent::End {
        child_id: "a".into(),
        reason: StopReason::parse("future-reason"),
    });
    assert_eq!(lifecycle.lamp(&running), Lamp::Inactive);
}

#[test]
fn blink_uses_the_eight_hundred_millisecond_period() {
    let lifecycle = Lifecycle::new();
    let now = Instant::now();
    assert!(lifecycle.blink_on(now));
    assert!(!lifecycle.blink_on(now + nano_subagents::BLINK_PERIOD / 2));
    assert!(lifecycle.blink_on(now + nano_subagents::BLINK_PERIOD));
}

#[test]
fn cleanup_hides_only_the_dock_entry_after_the_delay() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [row("a", "one-shot", "inactive")],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    state
        .apply_event(&json!({"type": "end", "childId": "a", "stopReason": "completed"}))
        .unwrap();
    assert_eq!(state.dock().0.len(), 1);
    assert!(!state.cleanup(Instant::now() + CLEANUP_DELAY / 2));
    assert_eq!(state.dock().0.len(), 1);
    assert!(state.cleanup(Instant::now() + CLEANUP_DELAY + Duration::from_millis(1)));
    assert!(state.dock().0.is_empty());
    // The record is not deleted: a fresh snapshot or the all-agents list can
    // still find it.
    assert!(state.list_rows().iter().any(|row| row.id == "a"));
}

#[test]
fn continuable_end_keeps_the_dock_entry_but_one_shot_end_hides_it() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [
                row("live", "continuable", "inactive"),
                row("done", "one-shot", "inactive")
            ],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    state
        .apply_event(&json!({"type": "end", "childId": "live", "stopReason": "completed"}))
        .unwrap();
    state
        .apply_event(&json!({"type": "end", "childId": "done", "stopReason": "completed"}))
        .unwrap();
    assert!(state.cleanup(Instant::now() + CLEANUP_DELAY + Duration::from_millis(1)));
    let (dock, _) = state.dock();
    let ids: Vec<&str> = dock.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["live"]);
    // The one-shot record remains in the catalog/all-agents list.
    assert!(state.list_rows().iter().any(|row| row.id == "done"));
}

#[test]
fn last_turn_completed_never_paints_a_terminal_lamp() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [{
                "id": "a",
                "parentId": "root",
                "depth": 1,
                "mode": "continuable",
                "activity": "inactive",
                "hasChildren": false,
                "lastTurnCompleted": true
            }],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    let row = state.catalog().row("a").unwrap();
    assert_eq!(state.lamp(row), Lamp::Inactive);
}

#[test]
fn ordinary_turn_completion_does_not_hide_the_dock_entry() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [{
                "id": "a",
                "parentId": "root",
                "depth": 1,
                "mode": "continuable",
                "activity": "inactive",
                "hasChildren": false,
                "lastTurnCompleted": true
            }],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    assert_eq!(state.dock().0.len(), 1);
    assert!(!state.cleanup(Instant::now() + CLEANUP_DELAY * 4));
}

#[test]
fn navigation_returns_to_the_exact_origin() {
    let mut state = SubagentState::new("root");
    state
        .apply_snapshot(&json!({
            "agents": [
                row("a", "continuable", "inactive"),
                row("b", "continuable", "inactive")
            ],
            "diagnostics": [],
            "parentLive": true
        }))
        .unwrap();
    state.nav_mut().dock_selected = 1;
    assert!(state.enter_dock(1));
    assert!(matches!(
        state.mounted().unwrap().origin,
        EntryOrigin::Dock { index: 1, .. }
    ));
    assert!(matches!(
        state.exit(),
        Some(ExitTarget::Dock { index: 1, .. })
    ));
    assert!(!state.mounted().is_some());

    state
        .apply_list(&json!({
            "entries": [
                {"id": "a", "parentId": "root", "depth": 1, "mode": "continuable", "activity": "inactive", "hasChildren": false},
                {"id": "c", "parentId": "a", "depth": 2, "mode": "one-shot", "activity": "inactive", "hasChildren": false}
            ],
            "diagnostics": []
        }))
        .unwrap();
    assert!(state.enter_list(1));
    assert_eq!(
        state.mounted().unwrap().origin,
        EntryOrigin::List {
            child_id: "c".into()
        }
    );
    assert_eq!(
        state.exit(),
        Some(ExitTarget::List {
            child_id: "c".into()
        })
    );
    assert!(state.nav().list_open);
}

#[test]
fn diagnostics_are_preserved() {
    let mut state = SubagentState::new("root");
    state
        .apply_list(&json!({
            "entries": [],
            "diagnostics": [
                {"id": "broken", "parentId": "root", "depth": 1, "reason": "corrupt"}
            ]
        }))
        .unwrap();
    assert_eq!(state.diagnostics().len(), 1);
    assert_eq!(state.diagnostics()[0].reason, "corrupt");
}

#[test]
fn event_payload_and_prompt_params_are_wire_compatible() {
    let event = nano_subagents::parse_event(&json!({
        "type": "end",
        "childId": "a",
        "stopReason": "max-tokens"
    }))
    .unwrap();
    assert_eq!(
        event,
        LifecycleEvent::End {
            child_id: "a".into(),
            reason: StopReason::MaxTokens
        }
    );

    let address = SessionAddress::child(
        "root",
        "root",
        "a",
        nano_conversation::SubagentMode::Continuable,
    );
    let params = nano_subagents::prompt_params(
        &address,
        "req-1",
        nano_conversation::Delivery::Queue,
        &[json!({"type": "text", "text": "hello"})],
    );
    assert_eq!(params["address"]["rootSessionId"], "root");
    assert_eq!(params["address"]["childSessionId"], "a");
    assert_eq!(params["delivery"], "queue");
    assert_eq!(params["requestId"], "req-1");
}
