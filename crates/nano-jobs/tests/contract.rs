//! S08 model contracts: official projections only, non-consuming offsets,
//! visible gaps, and stop facts that never claim a process exit.

use nano_jobs::{BashRecord, BashState, JobStatus, JobsModel, ShellResult};
use serde_json::json;

const ROOT: &str = "root";

fn running(id: &str) -> serde_json::Value {
    json!({
        "id": id,
        "kind": "bash",
        "label": "sleep 30",
        "owner": ROOT,
        "status": "running",
        "startedAt": 1,
        "output": {"total": 0, "earliest": 0}
    })
}

#[test]
fn job_projection_keeps_the_durable_tool_call_link() {
    let mut model = JobsModel::new(ROOT);
    model
        .apply_snapshot(&json!({
            "jobs": [{
                "id": "bash-0123456789abcdef",
                "kind": "bash",
                "label": "sleep 30",
                "status": "running",
                "startedAt": 1,
                "output": {"total": 12, "earliest": 0},
                "toolCall": {"callId": "c-1", "rootCallId": "r-1"}
            }]
        }))
        .unwrap();
    let job = &model.jobs()[0];
    assert_eq!(job.status(), JobStatus::Running);
    assert_eq!(job.call_label(), Some("r-1"));
    assert_eq!(job.tool_call.as_ref().unwrap().call_id, "c-1");
}

#[test]
fn events_upsert_in_official_order_and_removal_drops_the_record() {
    let mut model = JobsModel::new(ROOT);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "registered",
            "job": running("bash-a")
        }),
    );
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "registered",
            "job": running("bash-b")
        }),
    );
    assert_eq!(model.jobs()[0].id, "bash-a");
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "stopping",
            "job": {
                "id": "bash-a",
                "kind": "bash",
                "label": "sleep 30",
                "status": "stopping",
                "startedAt": 1,
                "output": {"total": 0, "earliest": 0}
            }
        }),
    );
    assert_eq!(model.jobs()[0].status(), JobStatus::Stopping);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "removed",
            "job": running("bash-a")
        }),
    );
    assert_eq!(model.jobs().len(), 1);
    assert_eq!(model.jobs()[0].id, "bash-b");
}

#[test]
fn a_stale_snapshot_cannot_roll_back_a_terminal_job() {
    let mut model = JobsModel::new(ROOT);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 5,
            "type": "settled",
            "cause": "producer",
            "awaited": false,
            "job": {
                "id": "bash-a",
                "kind": "bash",
                "label": "true",
                "status": "completed",
                "detail": "exit code: 0",
                "startedAt": 1,
                "finishedAt": 2,
                "output": {"total": 10, "earliest": 0}
            }
        }),
    );
    assert_eq!(model.last_event_seq(), 5);
    let applied = model
        .apply_snapshot(&json!({
            "seq": 4,
            "jobs": [{
                "id": "bash-a",
                "kind": "bash",
                "label": "true",
                "status": "running",
                "startedAt": 1,
                "output": {"total": 0, "earliest": 0}
            }]
        }))
        .unwrap();
    assert!(!applied);
    assert_eq!(model.jobs()[0].status(), JobStatus::Completed);
}

#[test]
fn a_snapshot_never_regresses_terminal_to_active_even_without_seq() {
    let mut model = JobsModel::new(ROOT);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "settled",
            "job": {
                "id": "bash-a",
                "kind": "bash",
                "label": "true",
                "status": "killed",
                "startedAt": 1,
                "finishedAt": 2,
                "output": {"total": 0, "earliest": 0}
            }
        }),
    );
    model
        .apply_snapshot(&json!({
            "jobs": [{
                "id": "bash-a",
                "kind": "bash",
                "label": "true",
                "status": "running",
                "startedAt": 1,
                "output": {"total": 0, "earliest": 0}
            }]
        }))
        .unwrap();
    assert_eq!(model.jobs()[0].status(), JobStatus::Killed);
}

#[test]
fn an_output_event_advances_the_projection_total() {
    let mut model = JobsModel::new(ROOT);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "registered",
            "job": running("bash-a")
        }),
    );
    assert_eq!(model.jobs()[0].output.total, 0);
    model.apply_event(
        "jobs.output",
        &json!({
            "address": {"rootSessionId": ROOT},
            "seq": 2,
            "jobId": "bash-a",
            "total": 42
        }),
    );
    assert_eq!(model.jobs()[0].output.total, 42);
}

#[test]
fn progress_and_spill_paths_are_kept_for_the_ui() {
    let mut model = JobsModel::new(ROOT);
    model
        .apply_snapshot(&json!({
            "seq": 1,
            "jobs": [{
                "id": "bash-a",
                "kind": "bash",
                "label": "build",
                "status": "running",
                "progress": "3/10",
                "startedAt": 1,
                "output": {"total": 10, "earliest": 2, "spillPaths": ["/tmp/full.log"]}
            }]
        }))
        .unwrap();
    let job = &model.jobs()[0];
    assert_eq!(job.progress.as_deref(), Some("3/10"));
    assert_eq!(job.output.spill_paths, vec!["/tmp/full.log"]);
    assert!(job.status_line().contains("3/10"));
}

#[test]
fn the_job_list_bound_drops_the_oldest_settled_projection() {
    let mut model = JobsModel::new(ROOT).with_bounds(2, 4096);
    for (id, status) in [
        ("bash-a", "completed"),
        ("bash-b", "running"),
        ("bash-c", "running"),
    ] {
        model.apply_event(
            "jobs.event",
            &json!({
                "address": {"rootSessionId": ROOT},
                "type": "registered",
                "job": {
                    "id": id,
                    "kind": "bash",
                    "label": id,
                    "status": status,
                    "startedAt": 1,
                    "output": {"total": 0, "earliest": 0}
                }
            }),
        );
    }
    assert_eq!(model.len(), 2);
    assert_eq!(model.jobs()[0].id, "bash-b");
    assert_eq!(model.jobs()[1].id, "bash-c");
}

#[test]
fn another_root_session_is_never_mixed_in() {
    let mut model = JobsModel::new(ROOT);
    assert!(!model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": "other"},
            "type": "registered",
            "job": running("bash-a")
        }),
    ));
    assert!(model.is_empty());
}

#[test]
fn follow_is_offset_based_and_marks_gaps_instead_of_splicing() {
    let mut model = JobsModel::new(ROOT).with_bounds(10, 4096);
    model
        .apply_follow(
            "bash-a",
            &json!({
                "chunks": [{"at": 0, "text": "one\n"}],
                "next": 4,
                "lossy": false
            }),
        )
        .unwrap();
    assert_eq!(model.next_offset("bash-a"), 4);
    assert_eq!(model.output("bash-a").unwrap().text, "one\n");

    model
        .apply_follow(
            "bash-a",
            &json!({
                "chunks": [
                    {"at": 100, "text": "tail\n", "gapBefore": true},
                    {"at": 105, "text": "err\n", "channel": "stderr"}
                ],
                "next": 109,
                "lossy": true
            }),
        )
        .unwrap();
    let buffer = model.output("bash-a").unwrap();
    assert_eq!(buffer.next, 109);
    assert!(buffer.lossy);
    assert!(buffer.text.contains("输出缺口"));
    assert!(buffer.text.contains("[stderr]"));
    assert!(!buffer.text.contains("two"));
}

#[test]
fn a_lost_head_keeps_the_tail_on_a_character_boundary() {
    let mut model = JobsModel::new(ROOT).with_bounds(10, 8);
    model
        .apply_follow(
            "bash-a",
            &json!({
                "chunks": [{"at": 0, "text": "一二三四五六七八"}],
                "next": 24,
                "lossy": false
            }),
        )
        .unwrap();
    let buffer = model.output("bash-a").unwrap();
    assert!(buffer.head_truncated);
    assert!(buffer.text.len() <= 8);
    assert!("一二三四五六七八".ends_with(&buffer.text));
}

#[test]
fn settled_jobs_keep_their_natural_status_and_detail() {
    let mut model = JobsModel::new(ROOT);
    model.apply_event(
        "jobs.event",
        &json!({
            "address": {"rootSessionId": ROOT},
            "type": "settled",
            "cause": "producer",
            "awaited": false,
            "job": {
                "id": "bash-a",
                "kind": "bash",
                "label": "true",
                "status": "completed",
                "detail": "exit code: 0",
                "startedAt": 1,
                "finishedAt": 2,
                "output": {"total": 0, "earliest": 0}
            }
        }),
    );
    assert_eq!(model.jobs()[0].status(), JobStatus::Completed);
    let notice = model.last_notice().unwrap();
    assert!(notice.contains("完成"));
    assert!(notice.contains("exit code: 0"));
}

#[test]
fn shell_outcomes_are_results_not_business_errors() {
    let result: ShellResult = serde_json::from_value(json!({
        "exitCode": 3,
        "signal": null,
        "timedOut": false,
        "aborted": false,
        "stdout": {"text": "out", "truncated": true, "spillPath": "/tmp/spill"},
        "stderr": {"text": "err", "truncated": false}
    }))
    .unwrap();
    assert_eq!(result.outcome_label(), "退出码 3");
    assert!(result.stdout.display().contains("/tmp/spill"));

    let signal: ShellResult = serde_json::from_value(json!({
        "exitCode": null,
        "signal": "SIGTERM",
        "timedOut": false,
        "aborted": false,
        "stdout": {"text": "", "truncated": false},
        "stderr": {"text": "", "truncated": false}
    }))
    .unwrap();
    assert_eq!(signal.outcome_label(), "被信号 SIGTERM 终止");

    let missing: ShellResult = serde_json::from_value(json!({
        "exitCode": null,
        "signal": null,
        "timedOut": false,
        "aborted": false,
        "stdout": {"text": "", "truncated": false},
        "stderr": {"text": "", "truncated": false}
    }))
    .unwrap();
    assert_eq!(missing.outcome_label(), "退出码未知");
}

#[test]
fn a_bash_record_finishes_with_the_official_message_identity() {
    let mut record = BashRecord::new(1, "printf hi");
    record
        .finish(&json!({
            "exitCode": 0,
            "signal": null,
            "timedOut": false,
            "aborted": false,
            "stdout": {"text": "hi", "truncated": false},
            "stderr": {"text": "", "truncated": false},
            "injectedMessageId": "m-1"
        }))
        .unwrap();
    assert_eq!(record.state, BashState::Done);
    assert!(record.matches_injected("m-1"));
    assert_eq!(record.stdout(), "hi");

    record.unknown("transport closed");
    assert!(matches!(record.state, BashState::Unknown(_)));
}
