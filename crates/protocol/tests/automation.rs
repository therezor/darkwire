//! Strict variants: a stray field is refused, not dropped.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use garde::Validate;
use ghostai_protocol::{
    AutomationJob, AutomationPayload, AutomationSchedule, RunStatus, Toolbox, ToolboxLimits,
};
use serde_json::json;

#[test]
fn a_schedule_carries_exactly_its_own_fields() {
    let at: AutomationSchedule = serde_json::from_value(json!({"kind": "at", "atMs": 5})).unwrap();
    assert_eq!(at.tag(), "at");
    assert!(
        serde_json::from_value::<AutomationSchedule>(
            json!({"kind": "cron", "expr": "* * * * *", "atMs": 5})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<AutomationSchedule>(
            json!({"kind": "cron", "expr": "0 9 * * *", "tz": "UTC"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<AutomationSchedule>(json!({"kind": "weekly"})).is_err());
    let every: AutomationSchedule =
        serde_json::from_value(json!({"kind": "every", "everyMs": 0})).unwrap();
    assert!(every.validate().is_err());
}

#[test]
fn a_payload_defaults_and_refuses_the_other_kind_s_field() {
    let heartbeat: AutomationPayload =
        serde_json::from_value(json!({"kind": "heartbeat"})).unwrap();
    let AutomationPayload::Heartbeat(h) = &heartbeat else {
        panic!("wrong kind")
    };
    assert_eq!(h.file, "TASK.md");
    assert!(!h.deliver);
    assert!(
        serde_json::from_value::<AutomationPayload>(
            json!({"kind": "scheduled", "message": "x", "file": "T.md"})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<AutomationPayload>(json!({"kind": "scheduled"})).is_err());
}

#[test]
fn a_job_fills_in_state_and_flags() {
    let job: AutomationJob = serde_json::from_value(json!({
        "id": "j1", "name": "nightly",
        "schedule": {"kind": "cron", "expr": "0 9 * * *"},
        "payload": {"kind": "scheduled", "message": "go"},
    }))
    .unwrap();
    assert!(job.enabled);
    assert_eq!(job.state.last_status, RunStatus::Pending);
    assert_eq!(job.state.run_count, 0);
    assert!(job.created_by.is_none());
    assert!(job.validate().is_ok());
    let text = serde_json::to_value(&job).unwrap();
    assert_eq!(text["state"]["lastStatus"], json!("pending"));
    assert!(text.get("createdBy").is_none());
}

#[test]
fn a_toolbox_manifest_coerces_its_limits() {
    let toolbox: Toolbox = serde_json::from_value(json!({
        "schema": "ghostai.toolbox/1", "name": "recon", "image": "img@sha256:abc",
        "limits": {"memoryMb": "4096", "cpus": "1.5"},
    }))
    .unwrap();
    assert_eq!(toolbox.limits.memory_mb, 4096);
    assert!((toolbox.limits.cpus - 1.5).abs() < f64::EPSILON);
    assert_eq!(toolbox.limits.pids_max, ToolboxLimits::default().pids_max);
    assert_eq!(toolbox.caps.drop, vec!["ALL"]);
    assert!(toolbox.security.read_only_root);
    assert_eq!(toolbox.network.dns, vec!["127.0.0.11"]);
    assert!(toolbox.validate().is_ok());
    assert!(
        serde_json::from_value::<Toolbox>(
            json!({"schema": "ghostai.toolbox/2", "name": "x", "image": "i"})
        )
        .is_err()
    );
}
