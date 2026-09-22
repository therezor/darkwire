//! The one egress grammar: what parses, what is refused, and what matches what.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a case that cannot parse is a failing test either way"
)]

use darkwire_security::{AllowEntry, AllowList, ParsedIp, parse_allow_entry, parse_ip_literal};

fn list(entries: &[&str]) -> AllowList {
    AllowList::parse(
        &entries
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect::<Vec<_>>(),
    )
    .expect("the entries parse")
}

fn ip(text: &str) -> ParsedIp {
    parse_ip_literal(text).expect("an address literal")
}

fn refusal(entry: &str) -> String {
    parse_allow_entry(entry)
        .expect_err("the entry should be refused")
        .message
}

#[test]
fn each_shape_parses_as_itself() {
    assert!(matches!(
        parse_allow_entry("10.0.0.0/8"),
        Ok(AllowEntry::Block(_))
    ));
    assert!(matches!(
        parse_allow_entry("2001:db9::/32"),
        Ok(AllowEntry::Block(_))
    ));
    assert!(matches!(
        parse_allow_entry("93.184.216.34"),
        Ok(AllowEntry::Address(_))
    ));
    assert!(matches!(
        parse_allow_entry(".example.com"),
        Ok(AllowEntry::Suffix(_))
    ));
    assert!(matches!(
        parse_allow_entry("api.example.com"),
        Ok(AllowEntry::Host(_))
    ));
}

/// The resolver accepts every one of these, so the grammar has to as well, or a
/// list of names would quietly admit an address written in a form nobody read.
#[test]
fn a_numeric_form_is_an_address_and_never_a_name() {
    for text in ["2130706433", "0177.0.0.1", "127.1", "0x7f.1"] {
        assert!(
            matches!(parse_allow_entry(text), Ok(AllowEntry::Address(_))),
            "{text} should parse as an address"
        );
    }
}

#[test]
fn a_name_is_lowercased_and_matched_case_insensitively() {
    let allowed = list(&["API.Example.COM"]);
    assert!(allowed.match_host("api.example.com").is_some());
    assert!(allowed.match_host("API.EXAMPLE.COM").is_some());
    // A trailing dot is the DNS root label, not a different host.
    assert!(allowed.match_host("api.example.com.").is_some());
}

#[test]
fn a_suffix_covers_the_bare_name_and_its_subdomains_and_nothing_else() {
    let allowed = list(&[".example.com"]);
    assert!(allowed.match_host("example.com").is_some());
    assert!(allowed.match_host("a.b.example.com").is_some());
    assert!(allowed.match_host("notexample.com").is_none());
    assert!(allowed.match_host("example.com.evil.test").is_none());
}

#[test]
fn a_host_entry_does_not_cover_its_subdomains() {
    let allowed = list(&["example.com"]);
    assert!(allowed.match_host("example.com").is_some());
    assert!(allowed.match_host("a.example.com").is_none());
}

/// The rule the module header calls load-bearing. A block authorising a name
/// would mean resolving every unlisted name to find out, which is the lookup an
/// attacker chooses.
#[test]
fn a_block_never_authorises_a_name_and_a_name_never_authorises_an_address() {
    let blocks = list(&["10.0.0.0/8"]);
    assert!(blocks.match_host("internal.corp").is_none());
    assert!(blocks.match_host("10.0.0.5").is_some());
    assert!(blocks.match_address(&ip("10.0.0.5")).is_some());

    let names = list(&["internal.corp", ".example.com"]);
    assert!(names.match_address(&ip("10.0.0.5")).is_none());
    assert!(names.match_host("10.0.0.5").is_none());
}

#[test]
fn a_block_matches_only_its_own_family() {
    let allowed = list(&["10.0.0.0/8"]);
    assert!(allowed.match_address(&ip("10.1.2.3")).is_some());
    assert!(allowed.match_address(&ip("11.0.0.1")).is_none());
    assert!(allowed.match_address(&ip("::ffff:10.0.0.5")).is_some());

    let six = list(&["2001:db9::/32"]);
    assert!(six.match_address(&ip("2001:db9::1")).is_some());
    assert!(six.match_address(&ip("10.0.0.1")).is_none());
}

#[test]
fn an_address_entry_matches_that_address_alone() {
    let allowed = list(&["93.184.216.34"]);
    assert!(allowed.match_address(&ip("93.184.216.34")).is_some());
    assert!(allowed.match_address(&ip("93.184.216.35")).is_none());
}

#[test]
fn a_malformed_entry_is_refused_and_named() {
    for entry in [
        "*.example.com",
        "example.com.",
        "under_score.example.com",
        "a..b.example.com",
        ".",
        ".*.example.com",
        "..example.com",
        "10.0.0.0/33",
        "",
        "   ",
    ] {
        let message = refusal(entry);
        assert!(
            message.contains("not a name") || message.contains("is empty"),
            "{entry}: {message}"
        );
    }
    let long = format!("{}.example.com", "a".repeat(250));
    assert!(refusal(&long).contains("not a name"));
}

#[test]
fn a_wildcard_refusal_names_the_suffix_form_to_use_instead() {
    assert!(refusal("*.example.com").contains(".example.com"));
}

/// The packet filter has no notion of the blocked table, so this parser is the
/// only thing standing between an operator's typo and raw TCP to the metadata
/// endpoint.
#[test]
fn a_hard_blocked_range_is_refused_however_it_is_written() {
    for entry in [
        "169.254.169.254",
        "169.254.0.0/16",
        "169.254.169.0/24",
        "224.0.0.1",
        "0.0.0.0",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
    ] {
        let message = refusal(entry);
        assert!(
            message.contains("which nothing may reach"),
            "{entry}: {message}"
        );
    }
}

/// Loopback and private are the two categories an operator legitimately lists:
/// a model server on this host, a service on the LAN.
#[test]
fn loopback_and_private_entries_are_accepted() {
    for entry in [
        "127.0.0.1",
        "127.0.0.0/8",
        "::1",
        "10.0.0.0/8",
        "192.168.1.0/24",
        "fd00::/8",
    ] {
        assert!(
            parse_allow_entry(entry).is_ok(),
            "{entry} should be accepted"
        );
    }
}

/// A prefix that merely overlaps a blocked range is not inside it. An operator
/// writing "everything" means everything, and the fetch guard still refuses the
/// hard categories at connect time.
#[test]
fn a_block_wider_than_a_blocked_range_is_accepted() {
    assert!(parse_allow_entry("0.0.0.0/0").is_ok());
    assert!(parse_allow_entry("::/0").is_ok());
}

/// Written into an nftables ruleset, so it is reprinted from the parsed bytes
/// rather than echoed. An entry carrying a semicolon cannot reach a rule.
#[test]
fn a_rule_is_rendered_from_the_parsed_value() {
    let allowed = list(&[
        "10.0.0.0/8",
        "93.184.216.34",
        "2001:0DB9:0000::/32",
        ".example.com",
    ]);
    assert_eq!(
        allowed.filter_rules().collect::<Vec<_>>(),
        vec![
            ("ip", "10.0.0.0/8".to_owned()),
            ("ip", "93.184.216.34".to_owned()),
            ("ip6", "2001:db9::/32".to_owned()),
        ]
    );
    assert!(parse_allow_entry("0.0.0.0/0; accept").is_err());
}

/// A packet filter cannot express a name, which is why the proxy exists.
#[test]
fn a_name_entry_produces_no_packet_filter_rule() {
    for entry in [".example.com", "api.example.com"] {
        assert!(
            parse_allow_entry(entry)
                .expect("the entry parses")
                .as_filter_rule()
                .is_none(),
            "{entry}"
        );
    }
    assert_eq!(
        list(&[".example.com", "api.example.com"])
            .filter_rules()
            .count(),
        0
    );
}

#[test]
fn an_empty_list_is_empty_and_a_populated_one_is_not() {
    assert!(!list(&["10.0.0.0/8"]).is_empty());
    assert!(AllowList::default().is_empty());
}

#[test]
fn the_first_bad_entry_fails_the_whole_list_and_the_detail_names_it() {
    let error =
        AllowList::parse(&["example.com".to_owned(), "*.bad".to_owned()]).expect_err("a refusal");
    assert_eq!(error.details["entry"], "*.bad");
}
