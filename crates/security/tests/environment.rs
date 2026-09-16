//! Container definitions: parsing, install policy, and gateway compatibility.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use ghostai_core::ErrorKind;
use ghostai_protocol::environment::EnvironmentDefinition;
use ghostai_security::{
    assert_environment_policy, assert_gateway_compatible, manifest_hash, parse_environment,
    weakened_in,
};
use serde_json::{Value, json};

use common::{message_of, read_fixture};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn definition_bytes(overrides: &Value) -> Vec<u8> {
    let mut base = json!({
        "schema": "ghostai.environment/1",
        "name": "kali-pentest",
        "image": format!("docker.io/kalilinux/kali-rolling@{DIGEST}"),
    });
    for (key, value) in overrides.as_object().unwrap() {
        base[key] = value.clone();
    }
    serde_json::to_vec(&base).unwrap()
}

fn container(overrides: &Value) -> EnvironmentDefinition {
    parse_environment(&definition_bytes(overrides)).unwrap()
}

#[test]
fn manifest_hash_matches_the_fixture_cases() {
    let fixture = read_fixture("extension/digest/expected.json");
    for case in fixture["manifestHash"].as_array().unwrap() {
        assert_eq!(
            manifest_hash(case["input"].as_str().unwrap().as_bytes()),
            case["output"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
    assert_ne!(manifest_hash(b"{\"a\":1}"), manifest_hash(b"{ \"a\": 1 }"));
    assert_eq!(manifest_hash(b"x").len(), 64);
}

#[test]
fn parses_a_minimal_definition_with_every_default() {
    let container = container(&json!({}));
    assert_eq!(container.workdir, "/workspace");
    assert_eq!(container.user, "1000:1000");
    assert_eq!(container.caps.drop, ["ALL"]);
    assert!(container.caps.add.is_empty());
    assert!(container.security.no_new_privileges);
    assert!(container.security.read_only_root);
    assert!(!container.shared);
}

#[test]
fn parse_errors_name_the_problem() {
    let malformed = parse_environment(b"{").unwrap_err();
    assert_eq!(malformed.kind, ErrorKind::Config);
    assert!(malformed.message.contains("not valid YAML"));

    let schema = message_of(&parse_environment(&definition_bytes(
        &json!({"schema": "ghostai.container/2"}),
    )));
    assert!(schema.contains("not valid"));
    assert!(schema.contains("schema"));

    let runtime = message_of(&parse_environment(&definition_bytes(
        &json!({"runtime": "containerd"}),
    )));
    assert!(runtime.contains("runtime"));

    let root = message_of(&parse_environment(b"\"a string\""));
    assert!(root.contains("(root)"));

    let empty_image = message_of(&parse_environment(&definition_bytes(&json!({"image": ""}))));
    assert!(empty_image.contains("image"), "{empty_image}");

    // A tool grant belongs to the agent permission map. Naming one here is refused rather
    // than ignored, so the two manifests cannot drift into one.
    let grants = message_of(&parse_environment(&definition_bytes(
        &json!({"tools": [{"name": "git_status", "definition": "git-status"}]}),
    )));
    assert!(grants.contains("not valid"), "{grants}");
}

#[test]
fn accepts_digest_pinned_images_and_refuses_tags() {
    assert!(assert_environment_policy(&container(&json!({}))).is_ok());
    for image in [
        "kalilinux/kali-rolling:latest",
        "kali@sha256:abc",
        format!("-v/:/hostfs@{DIGEST}").as_str(),
        format!("--privileged@{DIGEST}").as_str(),
        format!(" alpine@{DIGEST}").as_str(),
        format!("alpine@{DIGEST} --privileged").as_str(),
    ] {
        let error = assert_environment_policy(&container(&json!({"image": image}))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config, "{image}");
        assert!(error.message.contains("digest"), "{image}");
        assert_eq!(error.details["environment"], json!("kali-pentest"));
    }
    for image in [
        DIGEST.to_owned(),
        format!("alpine@{DIGEST}"),
        format!("docker.io/library/alpine@{DIGEST}"),
        format!("registry.example.com:5000/team/img@{DIGEST}"),
    ] {
        assert!(
            assert_environment_policy(&container(&json!({"image": image}))).is_ok(),
            "{image}"
        );
    }
}

#[test]
fn refuses_forbidden_capabilities_however_spelled() {
    for capability in [
        "NET_ADMIN",
        "CAP_NET_ADMIN",
        "net_admin",
        "SYS_ADMIN",
        "SYS_MODULE",
    ] {
        let error = assert_environment_policy(&container(&json!({"caps": {"add": [capability]}})))
            .unwrap_err();
        assert_eq!(error.details["capability"], json!(capability));
    }
    // Legitimate for a container with no network, so refused only by the
    // gateway check.
    assert!(assert_environment_policy(&container(&json!({"caps": {"add": ["NET_RAW"]}}))).is_ok());
    // Surfaced in the review rather than refused.
    assert!(
        assert_environment_policy(&container(&json!({"security": {"seccomp": "unconfined"}})))
            .is_ok()
    );
}

#[test]
fn refuses_a_workdir_that_would_bury_the_image() {
    for workdir in ["/", "workspace", "relative/path", ""] {
        let error =
            assert_environment_policy(&container(&json!({"workdir": workdir}))).unwrap_err();
        assert!(error.message.contains("absolute path"), "{workdir}");
        assert_eq!(error.details["workdir"], json!(workdir));
    }
    assert!(assert_environment_policy(&container(&json!({"workdir": "/srv/work"}))).is_ok());
}

#[test]
fn a_gateway_needs_a_non_root_numeric_uid() {
    for user in ["0:0", "root:root", "", "1000x:1000"] {
        let error = assert_gateway_compatible(&container(&json!({"user": user}))).unwrap_err();
        assert!(error.message.contains("restricted allow-list"), "{user}");
        assert_eq!(error.kind, ErrorKind::Config);
    }
    // The message names the image default rather than an empty string.
    let default = assert_gateway_compatible(&container(&json!({"user": ""}))).unwrap_err();
    assert!(default.message.contains("the image default"));
    assert!(assert_gateway_compatible(&container(&json!({"user": "1000:1000"}))).is_ok());
    assert!(assert_gateway_compatible(&container(&json!({"user": "1000"}))).is_ok());
}

#[test]
fn a_gateway_refuses_the_uid_the_proxy_reserves() {
    let error = assert_gateway_compatible(&container(&json!({"user": "65532:65532"}))).unwrap_err();
    assert!(error.message.contains("reserves for itself"));
}

#[test]
fn a_gateway_needs_no_new_privileges_and_no_forging_capability() {
    let error =
        assert_gateway_compatible(&container(&json!({"security": {"noNewPrivileges": false}})))
            .unwrap_err();
    assert!(error.message.contains("no-new-privileges"));

    for capability in ["NET_RAW", "cap_net_raw", "SETUID", "SETGID"] {
        let error = assert_gateway_compatible(&container(&json!({"caps": {"add": [capability]}})))
            .unwrap_err();
        assert_eq!(error.details["capability"], json!(capability));
        assert!(
            error.message.contains("restricted allow-list"),
            "{capability}"
        );
    }
    // A capability that does not defeat the filter is left alone.
    assert!(assert_gateway_compatible(&container(&json!({"caps": {"add": ["CHOWN"]}}))).is_ok());
}

#[test]
fn weakened_in_names_what_grants_more_than_the_defaults() {
    assert!(weakened_in(&container(&json!({}))).is_empty());
    let loud = weakened_in(&container(&json!({
        "security": {
            "devices": ["/dev/fuse", "/dev/sda"],
            "seccomp": "unconfined",
            "readOnlyRoot": false,
            "noNewPrivileges": false,
        },
        "user": "0:0",
        "runtime": "runsc",
    })));
    assert_eq!(
        loud,
        [
            "devices    /dev/fuse, /dev/sda  (host device access)",
            "user       0:0  (may be root)",
            "seccomp    unconfined",
            "rootfs     writable",
            "privileges may be gained (no-new-privileges off)",
            "runtime    runsc",
        ]
    );
    let default_user = weakened_in(&container(&json!({"user": "", "runtime": "kata"})));
    assert_eq!(
        default_user,
        ["user       image default  (may be root)", "runtime    kata"]
    );
}
