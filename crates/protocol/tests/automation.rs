//! Strict variants: a stray field is refused, not dropped.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use garde::Validate;
use ghostai_protocol::toolbox::{ContainerDefinition, ContainerLimits};
use ghostai_protocol::{AutomationJob, AutomationPayload, AutomationSchedule, RunStatus, Toolbox};
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
fn a_container_manifest_coerces_its_limits() {
    let container: ContainerDefinition = serde_json::from_value(json!({
        "schema": "ghostai.container/1", "name": "dev", "image": "img@sha256:abc",
        "limits": {"memoryMb": "4096", "cpus": "1.5"},
    }))
    .unwrap();
    assert_eq!(container.limits.memory_mb, 4096);
    assert!((container.limits.cpus - 1.5).abs() < f64::EPSILON);
    assert_eq!(
        container.limits.pids_max,
        ContainerLimits::default().pids_max
    );
    assert_eq!(container.caps.drop, vec!["ALL"]);
    assert!(container.security.read_only_root);
    assert!(container.validate().is_ok());
    assert!(
        serde_json::from_value::<ContainerDefinition>(
            json!({"schema": "ghostai.container/2", "name": "x", "image": "i"})
        )
        .is_err()
    );
}

#[test]
fn a_toolbox_manifest_carries_grants_and_no_image() {
    let toolbox: Toolbox = serde_json::from_value(json!({
        "schema": "ghostai.toolbox/1", "name": "recon",
        "tools": [{"name": "git_status", "definition": "git-status"}],
    }))
    .unwrap();
    assert_eq!(toolbox.tools.len(), 1);
    assert_eq!(toolbox.tools[0].definition, "git-status");
    assert!(toolbox.validate().is_ok());
    // An image belongs to a container, so a toolbox naming one is rejected
    // rather than quietly ignored.
    assert!(
        serde_json::from_value::<Toolbox>(json!({
            "schema": "ghostai.toolbox/1", "name": "recon", "tools": [], "image": "img",
        }))
        .is_err()
    );
}
