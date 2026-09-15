//! Rules must fail closed for both address families and proxy-only egress.

#![allow(clippy::unwrap_used, reason = "test assertions")]

use ghostai_protocol::toolbox::ContainerDefinition;
use ghostai_protocol::{ContainerNetwork, NetworkMode};
use ghostai_security::egress::gateway_rules;
use serde_json::json;

fn container() -> ContainerDefinition {
    serde_json::from_value(json!({
        "schema": "ghostai.container/1",
        "name": "test",
        "image": format!("sha256:{}", "e".repeat(64)),
        "user": "1000:1000",
    }))
    .unwrap()
}

fn network(allow: &[&str], hosts: &[&str], dns: &[&str]) -> ContainerNetwork {
    ContainerNetwork {
        mode: NetworkMode::Allowlist,
        allow: allow.iter().map(|v| (*v).to_owned()).collect(),
        hosts: hosts.iter().map(|v| (*v).to_owned()).collect(),
        dns: dns.iter().map(|v| (*v).to_owned()).collect(),
    }
}

#[test]
fn cidr_rules_include_ipv6_and_deny_unmatched_traffic() {
    let rules = gateway_rules(
        &container(),
        &network(&["192.0.2.0/24", "2001:db8::/32"], &[], &["1.1.1.1"]),
    )
    .unwrap();
    assert!(rules.contains("policy drop"));
    assert!(rules.contains("ip daddr 192.0.2.0/24 accept"));
    assert!(rules.contains("ip6 daddr 2001:db8::/32 accept"));
    assert!(rules.contains("ip daddr 1.1.1.1 udp dport 53 accept"));
    assert!(rules.contains("ip daddr 1.1.1.1 tcp dport 53 accept"));
    assert!(!rules.contains("meta skuid"));
}

#[test]
fn an_ipv6_resolver_is_matched_in_its_own_family() {
    let rules = gateway_rules(
        &container(),
        &network(&["2001:db8::/32"], &[], &["2606:4700:4700::1111"]),
    )
    .unwrap();
    assert!(rules.contains("ip6 daddr 2606:4700:4700::1111 udp dport 53 accept"));
}

#[test]
fn only_an_allowlist_gets_a_gateway_at_all() {
    for mode in [NetworkMode::None, NetworkMode::Open] {
        let mut asked = network(&[], &[], &[]);
        asked.mode = mode;
        let error = gateway_rules(&container(), &asked).unwrap_err();
        assert!(error.message.contains("restricted egress"));
    }
}

#[test]
fn a_cidr_list_refuses_a_loopback_resolver_it_could_never_match() {
    let error = gateway_rules(
        &container(),
        &network(&["192.0.2.0/24"], &[], &["127.0.0.11"]),
    )
    .unwrap_err();
    assert!(error.message.contains("non-loopback"));
}

#[test]
fn domain_proxy_cannot_be_combined_with_direct_egress_or_raw_sockets() {
    let rules = gateway_rules(&container(), &network(&[], &["example.com"], &[])).unwrap();
    assert!(rules.contains("meta skuid 65532 accept"));
    assert!(rules.contains("ip daddr 127.0.0.1 tcp dport 3128 accept"));
    assert!(!rules.contains("dport 53"));

    let both = gateway_rules(
        &container(),
        &network(&["0.0.0.0/0"], &["example.com"], &[]),
    )
    .unwrap_err();
    assert!(both.message.contains("not both"));

    let mut raw = container();
    raw.caps.add.push("NET_RAW".into());
    assert!(gateway_rules(&raw, &network(&[], &["example.com"], &[])).is_err());

    let mut proxy_uid = container();
    proxy_uid.user = "65532:65532".into();
    assert!(gateway_rules(&proxy_uid, &network(&[], &["example.com"], &[])).is_err());
}

#[test]
fn policy_values_cannot_inject_firewall_rules() {
    let injected_cidr = gateway_rules(
        &container(),
        &network(&["0.0.0.0/0; accept"], &[], &["1.1.1.1"]),
    )
    .unwrap_err();
    assert!(injected_cidr.message.contains("Invalid CIDR"));

    let injected_dns = gateway_rules(
        &container(),
        &network(&["192.0.2.0/24"], &[], &["1.1.1.1; accept"]),
    )
    .unwrap_err();
    assert!(injected_dns.message.contains("IP literals"));
}
