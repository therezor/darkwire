//! Rules must fail closed for both address families, and never open a resolver.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};
use darkwire_security::egress::gateway_rules;
use serde_json::json;

fn container() -> EnvironmentDefinition {
    serde_json::from_value(json!({
        "schema": "darkwire.environment/1",
        "name": "test",
        "image": format!("sha256:{}", "e".repeat(64)),
        "user": "1000:1000",
    }))
    .unwrap()
}

fn network(allow: &[&str]) -> EnvironmentNetwork {
    EnvironmentNetwork {
        mode: NetworkMode::Allowlist,
        allow: allow.iter().map(|v| (*v).to_owned()).collect(),
    }
}

#[test]
fn address_rules_cover_both_families_and_deny_unmatched_traffic() {
    let rules = gateway_rules(
        &container(),
        &network(&["93.184.216.0/24", "2001:db9::/32", "93.184.216.34"]),
    )
    .unwrap();
    assert!(rules.contains("policy drop"));
    assert!(rules.contains("ip daddr 93.184.216.0/24 accept"));
    assert!(rules.contains("ip6 daddr 2001:db9::/32 accept"));
    assert!(rules.contains("ip daddr 93.184.216.34 accept"));
}

/// The proxy is reachable from every allow-list, not only one naming hosts:
/// it is the only thing that resolves a name, so a list of addresses alone
/// still needs somewhere for a name to be refused rather than hang.
#[test]
fn every_allow_list_reaches_the_proxy_and_nothing_reaches_a_resolver() {
    for entries in [
        vec!["93.184.216.0/24"],
        vec!["example.com"],
        vec!["93.184.216.0/24", ".example.com"],
    ] {
        let rules = gateway_rules(&container(), &network(&entries)).unwrap();
        assert!(rules.contains("meta skuid 65532 accept"), "{entries:?}");
        assert!(
            rules.contains("ip daddr 127.0.0.1 tcp dport 3128 accept"),
            "{entries:?}"
        );
        // Nothing inside the namespace resolves a name, so port 53 is never
        // opened and there is no resolver left to configure.
        assert!(!rules.contains("dport 53"), "{entries:?}");
    }
}

#[test]
fn a_name_entry_produces_no_destination_rule() {
    let rules = gateway_rules(&container(), &network(&["example.com", ".example.org"])).unwrap();
    assert!(!rules.contains("daddr 93."));
    assert!(!rules.contains("example"));
}

#[test]
fn addresses_and_names_may_be_listed_together() {
    let rules = gateway_rules(&container(), &network(&["10.0.0.0/8", "api.example.com"])).unwrap();
    assert!(rules.contains("ip daddr 10.0.0.0/8 accept"));
    assert!(rules.contains("meta skuid 65532 accept"));
}

#[test]
fn only_an_allowlist_gets_a_gateway_at_all() {
    for mode in [NetworkMode::None, NetworkMode::Open] {
        let mut asked = network(&[]);
        asked.mode = mode;
        let error = gateway_rules(&container(), &asked).unwrap_err();
        assert!(error.message.contains("restricted egress"));
    }
}

#[test]
fn a_gateway_needs_a_container_that_could_host_one() {
    let mut raw = container();
    raw.caps.add.push("NET_RAW".into());
    assert!(gateway_rules(&raw, &network(&["example.com"])).is_err());

    let mut proxy_uid = container();
    proxy_uid.user = "65532:65532".into();
    assert!(gateway_rules(&proxy_uid, &network(&["example.com"])).is_err());
}

/// A rule is printed from the parsed bytes, so an entry cannot carry nftables
/// syntax into the ruleset however it is spelled.
#[test]
fn policy_values_cannot_inject_firewall_rules() {
    let injected = gateway_rules(&container(), &network(&["0.0.0.0/0; accept"])).unwrap_err();
    assert!(
        injected.message.contains("not a name"),
        "{}",
        injected.message
    );

    let name = gateway_rules(&container(), &network(&["example.com; accept"])).unwrap_err();
    assert!(name.message.contains("not a name"), "{}", name.message);
}

/// The packet filter has no notion of the blocked table, so an entry naming the
/// metadata range has to be refused before it can become a rule.
#[test]
fn a_hard_blocked_entry_never_becomes_a_rule() {
    let error = gateway_rules(&container(), &network(&["169.254.0.0/16"])).unwrap_err();
    assert!(error.message.contains("which nothing may reach"));
}

/// `0.0.0.0/0` is a legal entry and contains the metadata endpoint, so the
/// ranges nothing may reach are dropped before any accept can match them.
#[test]
fn a_hard_blocked_range_is_dropped_before_the_widest_accept() {
    let rules = gateway_rules(&container(), &network(&["0.0.0.0/0", "::/0"])).unwrap();
    let at = |needle: &str| {
        rules
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from {rules}"))
    };
    let first_accept = at("ip daddr 0.0.0.0/0 accept").min(at("ip6 daddr ::/0 accept"));
    for drop in [
        "ip daddr 169.254.0.0/16 drop",
        "ip6 daddr fe80::/10 drop",
        "ip daddr 224.0.0.0/4 drop",
        "ip daddr 0.0.0.0/8 drop",
    ] {
        assert!(at(drop) < first_accept, "{drop}");
    }
    // The proxy stays reachable ahead of the drops.
    assert!(at("ip daddr 127.0.0.1 tcp dport 3128 accept") < at("169.254.0.0/16"));
    // Ranges an entry may unlock are not dropped.
    assert!(!rules.contains("10.0.0.0/8 drop"));
    assert!(!rules.contains("127.0.0.0/8 drop"));
}

/// `::1` classifies as loopback, which an entry may unlock, but it sits inside
/// the dropped `::/96`. Its accept has to come first or it would mean nothing.
#[test]
fn an_unlockable_entry_inside_a_dropped_range_is_accepted_before_the_drop() {
    let rules = gateway_rules(&container(), &network(&["::1", "0.0.0.0/0"])).unwrap();
    let at = |needle: &str| rules.find(needle).unwrap();
    assert!(at("ip6 daddr ::1 accept") < at("ip6 daddr ::/96 drop"));
    assert!(at("ip daddr 169.254.0.0/16 drop") < at("ip daddr 0.0.0.0/0 accept"));
}
