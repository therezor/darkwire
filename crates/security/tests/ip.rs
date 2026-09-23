//! Address literal parsing and the blocked table, against `fixtures/ip`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::collections::HashSet;
use std::net::IpAddr;

use darkwire_security::{
    AddressCategory, BLOCKED_RANGES, IpFamily, ParsedIp, cidr_contains, classify_address,
    parse_cidr, parse_ip_literal,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use common::{cases, read_fixture};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn parse(host: &str) -> ParsedIp {
    parse_ip_literal(host).unwrap_or_else(|| panic!("expected {host} to parse as an address"))
}

fn category(host: &str) -> Option<AddressCategory> {
    classify_address(&parse(host)).map(|range| range.category)
}

fn literal_value(parsed: Option<ParsedIp>) -> Value {
    match parsed {
        None => Value::Null,
        Some(parsed) => json!({
            "family": parsed.family.number(),
            "canonical": parsed.canonical,
            "bytes": hex(&parsed.bytes),
        }),
    }
}

#[test]
fn matches_the_classify_fixture() {
    let fixture = read_fixture("ip/classify.json");
    let mut failures = Vec::new();
    for case in cases(&fixture) {
        let input = &case["input"];
        let actual = if let Some(cidr) = input["cidr"].as_str() {
            let host = input["host"].as_str().unwrap();
            let block = parse_cidr(cidr);
            let address = parse(host);
            json!({
                "cidr": block.as_ref().map(|b| json!({
                    "family": b.family.number(),
                    "bytes": hex(&b.bytes),
                    "prefix": b.prefix,
                })),
                "contains": block.as_ref().map(|b| cidr_contains(b, &address)),
            })
        } else {
            let host = input["host"].as_str().unwrap();
            let parsed = parse_ip_literal(host);
            let range = parsed
                .as_ref()
                .and_then(classify_address)
                .map(|range| serde_json::to_value(range).unwrap());
            json!({
                "literal": literal_value(parsed),
                "range": range.unwrap_or(Value::Null),
            })
        };
        if actual != case["output"] {
            failures.push(format!(
                "{}\n  expected {}\n  actual   {}",
                case["name"], case["output"], actual
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases(&fixture).len(), 137);
}

#[test]
fn every_blocked_range_parses_in_its_own_family() {
    assert!(BLOCKED_RANGES.len() > 20);
    for range in &BLOCKED_RANGES {
        let (address, prefix) = range.cidr.split_once('/').unwrap();
        assert!(parse_ip_literal(address).is_some(), "{}", range.cidr);
        assert!(prefix.parse::<u32>().is_ok(), "{}", range.cidr);
        assert!(parse_cidr(range.cidr).is_some());
    }
}

#[test]
fn blocked_ranges_have_no_duplicate_cidrs() {
    let cidrs: HashSet<&str> = BLOCKED_RANGES.iter().map(|range| range.cidr).collect();
    assert_eq!(cidrs.len(), BLOCKED_RANGES.len());
}

#[test]
fn every_encoding_of_loopback_is_loopback() {
    for host in [
        "127.0.0.1",
        "2130706433",
        "0177.0.0.1",
        "0x7f000001",
        "127.1",
        "127.0.0.1.",
        "::ffff:127.0.0.1",
        "::ffff:7f00:1",
        "[::ffff:127.0.0.1]",
    ] {
        assert_eq!(category(host), Some(AddressCategory::Loopback), "{host}");
    }
}

#[test]
fn every_encoding_of_the_metadata_endpoint_is_link_local() {
    for host in [
        "169.254.169.254",
        "2852039166",
        "0251.0376.0251.0376",
        "0xa9fea9fe",
        "169.254.43518",
        "::ffff:a9fe:a9fe",
    ] {
        assert_eq!(category(host), Some(AddressCategory::LinkLocal), "{host}");
    }
}

#[test]
fn the_most_specific_range_wins() {
    // Category drives policy: a deployment that allows loopback to reach its own
    // model server must get the same answer for [::1] as for 127.0.0.1.
    assert_eq!(classify_address(&parse("::1")).unwrap().cidr, "::1/128");
}

#[test]
fn converts_to_the_standard_address_types() {
    assert_eq!(parse("127.1").to_ip_addr(), IpAddr::from([127, 0, 0, 1]));
    assert_eq!(
        parse("::1").to_ip_addr(),
        IpAddr::from([0u16, 0, 0, 0, 0, 0, 0, 1])
    );
    assert_eq!(IpFamily::V4.number(), 4);
    assert_eq!(IpFamily::V6.number(), 6);
}

#[test]
fn a_far_too_long_numeric_part_is_refused_rather_than_wrapping() {
    assert!(parse_ip_literal("99999999999999999999999").is_none());
    assert!(parse_ip_literal("0xffffffffffffffffffffffff").is_none());
    assert!(parse_ip_literal("1.99999999999999999999").is_none());
}

#[test]
fn category_serialises_kebab_case() {
    assert_eq!(
        serde_json::to_value(AddressCategory::LinkLocal).unwrap(),
        json!("link-local")
    );
}

fn format_ipv4(bytes: [u8; 4], style: u8) -> String {
    let [a, b, c, d] = bytes;
    let whole = u32::from_be_bytes(bytes);
    let dotted = format!("{a}.{b}.{c}.{d}");
    match style {
        0 => dotted,
        1 => whole.to_string(),
        2 => format!("0x{whole:x}"),
        3 => [a, b, c, d]
            .iter()
            .map(|byte| format!("0{byte:o}"))
            .collect::<Vec<_>>()
            .join("."),
        4 => format!("{a}.{b}.{}", u32::from(c) * 256 + u32::from(d)),
        5 => format!("{dotted}."),
        _ => format!("::ffff:{dotted}"),
    }
}

/// Every generated address lies inside a range the table blocks.
fn blocked_ipv4() -> impl Strategy<Value = [u8; 4]> {
    prop_oneof![
        (Just(127u8), any::<u8>(), any::<u8>(), any::<u8>()),
        (Just(10u8), any::<u8>(), any::<u8>(), any::<u8>()),
        (Just(192u8), Just(168u8), any::<u8>(), any::<u8>()),
        (Just(169u8), Just(254u8), any::<u8>(), any::<u8>()),
        (Just(172u8), 16u8..=31, any::<u8>(), any::<u8>()),
        (Just(100u8), 64u8..=127, any::<u8>(), any::<u8>()),
    ]
    .prop_map(|(a, b, c, d)| [a, b, c, d])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn encoding_cannot_smuggle_a_blocked_address(bytes in blocked_ipv4(), style in 0u8..=6) {
        let host = format_ipv4(bytes, style);
        let parsed = parse_ip_literal(&host);
        prop_assert!(parsed.is_some(), "{host}");
        prop_assert!(classify_address(&parsed.unwrap()).is_some(), "{host}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn every_encoding_agrees_on_the_canonical_form(bytes in blocked_ipv4()) {
        let canonical = parse(&format_ipv4(bytes, 0)).canonical;
        for style in [1u8, 2, 3, 5, 6] {
            prop_assert_eq!(&parse(&format_ipv4(bytes, style)).canonical, &canonical);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn never_crashes_on_arbitrary_host_strings(host in any::<String>()) {
        if let Some(parsed) = parse_ip_literal(&host) {
            let expected = if parsed.family == IpFamily::V4 { 4 } else { 16 };
            prop_assert_eq!(parsed.bytes.len(), expected);
            classify_address(&parsed);
            let _ = parsed.to_ip_addr();
        }
    }

    #[test]
    fn never_crashes_on_arbitrary_cidr_strings(text in any::<String>()) {
        if let Some(parsed) = parse_cidr(&text) {
            prop_assert!(parsed.prefix <= u32::try_from(parsed.bytes.len() * 8).unwrap());
            cidr_contains(&parsed, &parse("10.0.0.1"));
        }
    }
}
