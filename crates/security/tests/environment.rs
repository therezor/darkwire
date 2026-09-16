//! Container definitions: parsing, install policy, and gateway compatibility.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use darkwire_core::ErrorKind;
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_security::{
    assert_environment_policy, assert_gateway_compatible, manifest_hash, parse_environment,
    weakened_in,
};
use serde_json::{Value, json};

use common::{message_of, read_fixture};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn definition_bytes(overrides: &Value) -> Vec<u8> {
    let mut base = json!({
        "schema": "darkwire.environment/1",
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

    // A writable `/tmp` under that read-only root, because `exec` records its
    // pid there and a container without one loses cancellation silently.
    assert_eq!(container.security.tmpfs, ["/tmp:rw,nosuid,size=64m"]);

    // Sized for a small board: this runs on a Raspberry Pi, and one place is
    // one container rather than one per agent and session.
    assert_eq!(container.limits.memory_mb, 512);
    assert!((container.limits.cpus - 1.0).abs() < f64::EPSILON);
    assert_eq!(container.limits.pids_max, 256);
    assert_eq!(container.limits.shm_size_mb, 64);
}

#[test]
fn parse_errors_name_the_problem() {
    let malformed = parse_environment(b"{").unwrap_err();
    assert_eq!(malformed.kind, ErrorKind::Config);
    assert!(malformed.message.contains("not valid YAML"));

    let schema = message_of(&parse_environment(&definition_bytes(
        &json!({"schema": "darkwire.container/2"}),
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

// An agent's own egress request
//
// These ran through `preset install` before that command existed, which is how
// they came to be asserted nowhere else. They are the guards between an agent
// asking for a network and a gateway that could actually enforce the ask, so
// they belong beside the definition they are checked against.

mod agent_network {
    use darkwire_core::ErrorKind;
    use darkwire_protocol::config::{EnvironmentNetwork, NetworkMode};
    use darkwire_security::assert_environment_network;

    fn allowlist(allow: &[&str], hosts: &[&str], dns: &[&str]) -> EnvironmentNetwork {
        EnvironmentNetwork {
            mode: NetworkMode::Allowlist,
            allow: allow.iter().map(|entry| (*entry).to_owned()).collect(),
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            dns: dns.iter().map(|ip| (*ip).to_owned()).collect(),
        }
    }

    #[test]
    fn entries_without_the_allowlist_mode_are_refused_rather_than_ignored() {
        // Silently dropping them would leave the config claiming a boundary the
        // container does not have.
        let mut network = allowlist(&["10.0.0.0/8"], &[], &["10.0.0.53"]);
        network.mode = NetworkMode::None;

        let refusal = assert_environment_network(&network, "coder").unwrap_err();

        assert_eq!(refusal.kind, ErrorKind::Config);
        assert!(refusal.message.contains("would have no effect"));
        assert_eq!(refusal.details["agentId"], "coder");
    }

    #[test]
    fn cidrs_and_hosts_together_are_refused() {
        // One is enforced by the packet filter and the other by the proxy, so a
        // request enforced in two places is enforced in neither.
        let network = allowlist(&["10.0.0.0/8"], &["example.com"], &["10.0.0.53"]);

        let refusal = assert_environment_network(&network, "coder").unwrap_err();

        assert!(refusal.message.contains("Choose one"));
    }

    #[test]
    fn an_allow_entry_that_is_not_a_cidr_is_refused_and_named() {
        let network = allowlist(&["example.com"], &[], &["10.0.0.53"]);

        let refusal = assert_environment_network(&network, "coder").unwrap_err();

        assert!(refusal.message.contains("not a CIDR block"));
        assert_eq!(refusal.details["entry"], "example.com");
    }

    #[test]
    fn a_host_that_is_not_an_exact_dns_name_is_refused_and_named() {
        for host in ["", "*.example.com", ".example.com", "example.com."] {
            let network = allowlist(&[], &[host], &[]);

            let refusal = assert_environment_network(&network, "coder").unwrap_err();

            assert!(
                refusal.message.contains("not an exact DNS name"),
                "{host}: {}",
                refusal.message
            );
            assert_eq!(refusal.details["host"], host);
        }
    }

    #[test]
    fn a_resolver_that_is_not_an_ip_literal_is_refused() {
        // A name cannot be resolved by something that has to be resolved first.
        let network = allowlist(&["10.0.0.0/8"], &[], &["dns.example.com"]);

        let refusal = assert_environment_network(&network, "coder").unwrap_err();

        assert!(refusal.message.contains("not an IP literal"));
        assert_eq!(refusal.details["resolver"], "dns.example.com");
    }

    #[test]
    fn a_cidr_allow_list_with_no_resolver_is_refused() {
        // Nothing in the container could resolve a hostname, so every name
        // would fail and the failure would look like the network being down.
        let network = allowlist(&["10.0.0.0/8"], &[], &[]);

        let refusal = assert_environment_network(&network, "coder").unwrap_err();

        assert!(refusal.message.contains("names no DNS resolver"));
    }

    #[test]
    fn the_two_shapes_that_are_enforceable_are_accepted() {
        assert!(
            assert_environment_network(&allowlist(&["10.0.0.0/8"], &[], &["10.0.0.53"]), "a")
                .is_ok()
        );
        assert!(assert_environment_network(&allowlist(&[], &["example.com"], &[]), "a").is_ok());
        assert!(assert_environment_network(&EnvironmentNetwork::default(), "a").is_ok());
    }
}
