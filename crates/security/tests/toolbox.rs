//! Toolbox parsing, policy and the network ceiling.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use ghostai_core::ErrorKind;
use ghostai_protocol::{AgentToolboxNetwork, Toolbox, ToolboxNetworkMode};
use ghostai_security::{
    BUILTIN_TOOL_NAMES, assert_network_within_ceiling, assert_toolbox_policy, effective_network,
    manifest_hash, parse_toolbox, weakened_in,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use common::{message_of, read_fixture};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn manifest(overrides: &Value) -> Vec<u8> {
    let mut base = json!({
        "schema": "ghostai.toolbox/1",
        "name": "kali-pentest",
        "image": format!("docker.io/kalilinux/kali-rolling@{DIGEST}"),
    });
    for (key, value) in overrides.as_object().unwrap() {
        base[key] = value.clone();
    }
    serde_json::to_vec(&base).unwrap()
}

fn toolbox(overrides: &Value) -> Toolbox {
    parse_toolbox(&manifest(overrides)).unwrap()
}

fn network(mode: ToolboxNetworkMode, allow: &[&str]) -> AgentToolboxNetwork {
    AgentToolboxNetwork {
        mode,
        allow: allow.iter().map(|a| (*a).to_owned()).collect(),
    }
}

fn rank(mode: ToolboxNetworkMode) -> u8 {
    match mode {
        ToolboxNetworkMode::None => 0,
        ToolboxNetworkMode::Allowlist => 1,
        ToolboxNetworkMode::Open => 2,
    }
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
fn parses_a_minimal_manifest_with_every_default() {
    let toolbox = toolbox(&json!({}));
    assert_eq!(toolbox.workdir, "/workspace");
    assert_eq!(toolbox.caps.drop, ["ALL"]);
    assert!(toolbox.caps.add.is_empty());
    assert!(toolbox.security.no_new_privileges);
    assert_eq!(toolbox.network.max_mode, ToolboxNetworkMode::None);
    assert_eq!(toolbox.network.dns, ["127.0.0.11"]);
}

#[test]
fn parse_errors_name_the_problem() {
    let not_json = parse_toolbox(b"not json").unwrap_err();
    assert_eq!(not_json.kind, ErrorKind::Config);
    assert!(not_json.message.contains("not valid JSON"));

    let schema = message_of(&parse_toolbox(&manifest(
        &json!({"schema": "ghostai.sandbox-toolbox/2"}),
    )));
    assert!(schema.contains("not valid"));
    assert!(schema.contains("schema"));

    let runtime = message_of(&parse_toolbox(&manifest(&json!({"runtime": "containerd"}))));
    assert!(runtime.contains("runtime"));

    let root = message_of(&parse_toolbox(b"\"a string\""));
    assert!(root.contains("(root)"));

    // The schema's length rules are checked too, and name the field.
    let empty_name = message_of(&parse_toolbox(&manifest(&json!({"name": ""}))));
    assert!(empty_name.contains("name"), "{empty_name}");
}

#[test]
fn accepts_digest_pinned_images_and_refuses_tags() {
    assert!(assert_toolbox_policy(&toolbox(&json!({}))).is_ok());
    for image in [
        "kalilinux/kali-rolling:latest",
        "kali@sha256:abc",
        format!("-v/:/hostfs@{DIGEST}").as_str(),
        format!("--privileged@{DIGEST}").as_str(),
        format!(" alpine@{DIGEST}").as_str(),
        format!("alpine@{DIGEST} --privileged").as_str(),
    ] {
        let error = assert_toolbox_policy(&toolbox(&json!({"image": image}))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config, "{image}");
        assert!(error.message.contains("digest"), "{image}");
    }
    for image in [
        DIGEST.to_owned(),
        format!("alpine@{DIGEST}"),
        format!("docker.io/library/alpine@{DIGEST}"),
        format!("registry.example.com:5000/team/img@{DIGEST}"),
    ] {
        assert!(
            assert_toolbox_policy(&toolbox(&json!({"image": image}))).is_ok(),
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
        let error =
            assert_toolbox_policy(&toolbox(&json!({"caps": {"add": [capability]}}))).unwrap_err();
        assert_eq!(error.details["capability"], json!(capability));
    }
    assert!(assert_toolbox_policy(&toolbox(&json!({"caps": {"add": ["NET_RAW"]}}))).is_ok());
    // Surfaced in the review rather than refused.
    assert!(
        assert_toolbox_policy(&toolbox(&json!({"security": {"seccomp": "unconfined"}}))).is_ok()
    );
}

#[test]
fn refuses_an_entry_that_shadows_a_built_in() {
    for name in BUILTIN_TOOL_NAMES {
        let error =
            assert_toolbox_policy(&toolbox(&json!({"tools": [{"name": name}]}))).unwrap_err();
        assert!(error.message.contains("built-in tool"), "{name}");
    }
    assert!(
        assert_toolbox_policy(&toolbox(
            &json!({"tools": [{"name": "readfile"}, {"name": "execute"}]})
        ))
        .is_ok()
    );
}

#[test]
fn refuses_an_empty_proxy_host_entry() {
    let error = assert_toolbox_policy(&toolbox(&json!({"network": {"proxyAllowHosts": ["  "]}})))
        .unwrap_err();
    assert!(error.message.contains("empty proxy host"));
    assert!(
        assert_toolbox_policy(&toolbox(
            &json!({"network": {"proxyAllowHosts": ["deb.debian.org"]}})
        ))
        .is_ok()
    );
}

fn ceiling(max_mode: &str) -> Toolbox {
    toolbox(&json!({"network": {"maxMode": max_mode}}))
}

#[test]
fn refuses_an_agent_asking_for_more_than_the_ceiling() {
    let error = assert_network_within_ceiling(
        &ceiling("allowlist"),
        &network(ToolboxNetworkMode::Open, &[]),
        "pentest",
    )
    .unwrap_err();
    assert!(error.message.contains("permits at most"));
    assert_eq!(error.details["requested"], json!("open"));
    assert_eq!(error.details["maximum"], json!("allowlist"));
    assert!(
        assert_network_within_ceiling(
            &ceiling("none"),
            &network(ToolboxNetworkMode::Allowlist, &["10.0.0.0/8"]),
            "malware",
        )
        .unwrap_err()
        .message
        .contains("permits at most")
    );
}

#[test]
fn accepts_a_request_at_or_below_the_ceiling() {
    assert!(
        assert_network_within_ceiling(
            &ceiling("open"),
            &network(
                ToolboxNetworkMode::Allowlist,
                &["10.0.0.0/8", "192.168.1.0/24"]
            ),
            "a",
        )
        .is_ok()
    );
    assert!(
        assert_network_within_ceiling(
            &ceiling("open"),
            &network(ToolboxNetworkMode::None, &[]),
            "a"
        )
        .is_ok()
    );
}

#[test]
fn refuses_an_empty_allow_list_and_non_cidr_entries() {
    let empty = assert_network_within_ceiling(
        &ceiling("open"),
        &network(ToolboxNetworkMode::Allowlist, &[]),
        "a",
    )
    .unwrap_err();
    assert!(empty.message.contains("reaches nothing"));
    for entry in [
        "example.com",
        "10.0.0.1",
        "not a cidr",
        "10.0.0.0/64",
        "10.0.0.0/+8",
    ] {
        let error = assert_network_within_ceiling(
            &ceiling("open"),
            &network(ToolboxNetworkMode::Allowlist, &[entry]),
            "a",
        )
        .unwrap_err();
        assert!(error.message.contains("CIDR"), "{entry}");
        assert_eq!(error.details["entry"], json!(entry));
    }
}

#[test]
fn effective_network_intersects_the_ceiling() {
    let narrowed = effective_network(
        &ceiling("none"),
        &network(ToolboxNetworkMode::Allowlist, &["10.0.0.0/8"]),
    );
    assert_eq!(narrowed.mode, ToolboxNetworkMode::None);
    assert!(narrowed.allow.is_empty());

    let carried = effective_network(
        &toolbox(
            &json!({"network": {"maxMode": "open", "dns": ["1.1.1.1"], "proxyAllowHosts": ["deb.debian.org"]}}),
        ),
        &network(ToolboxNetworkMode::Open, &[]),
    );
    assert_eq!(carried.dns, ["1.1.1.1"]);
    assert_eq!(carried.proxy_allow_hosts, ["deb.debian.org"]);

    let listed = effective_network(
        &ceiling("open"),
        &network(ToolboxNetworkMode::Allowlist, &["10.0.0.0/8"]),
    );
    assert_eq!(listed.mode, ToolboxNetworkMode::Allowlist);
    assert_eq!(listed.allow, ["10.0.0.0/8"]);
    assert_eq!(listed, listed.clone());
}

#[test]
fn weakened_in_names_what_grants_more_than_the_defaults() {
    assert!(weakened_in(&toolbox(&json!({"user": "1000:1000"}))).is_empty());
    let loud = weakened_in(&toolbox(&json!({
        "security": {"devices": ["/dev/fuse", "/dev/sda"], "seccomp": "unconfined", "readOnlyRoot": false},
        "user": "0:0",
        "runtime": "runsc",
        "workdir": "/",
    })));
    assert_eq!(
        loud,
        [
            "devices    /dev/fuse, /dev/sda  (host device access)",
            "user       0:0  (may be root)",
            "seccomp    unconfined",
            "rootfs     writable",
            "runtime    runsc",
            "workdir    / (mounts the workspace over the root)",
        ]
    );
    let default_user = weakened_in(&toolbox(&json!({"runtime": "kata"})));
    assert_eq!(
        default_user,
        ["user       image default  (may be root)", "runtime    kata"]
    );
}

fn modes() -> impl Strategy<Value = ToolboxNetworkMode> {
    prop::sample::select(vec![
        ToolboxNetworkMode::None,
        ToolboxNetworkMode::Allowlist,
        ToolboxNetworkMode::Open,
    ])
}

proptest! {
    #[test]
    fn never_widens_beyond_the_ceiling(max_mode in modes(), requested in modes()) {
        // A `min` rather than a union means no ordering of calls can produce
        // more reach than the toolbox permits, whether or not the assert ran.
        let toolbox = toolbox(&json!({"network": {"maxMode": serde_json::to_value(max_mode).unwrap()}}));
        let resolved = effective_network(&toolbox, &network(requested, &["10.0.0.0/8"]));
        prop_assert!(rank(resolved.mode) <= rank(max_mode));
        prop_assert!(rank(resolved.mode) <= rank(requested));
    }
}
