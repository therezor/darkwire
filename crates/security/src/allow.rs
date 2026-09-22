//! One grammar for "what may this reach", shared by every enforcement point.
//!
//! Egress used to be three lists with three different matchers: CIDRs for the
//! packet filter, exact names for the proxy, and a fourth spelling inside the
//! guarded fetch that also understood a leading dot. Three answers to one
//! question is how a destination ends up permitted in one place and refused in
//! another, so there is now one entry type, parsed once, and the filter, the
//! proxy and the fetch guard all ask it.
//!
//! An entry is exactly one of four shapes, tried in that order:
//!
//! ```text
//! 10.0.0.0/8      a block      matches an address inside it
//! 93.184.216.34   an address   matches that address
//! .example.com    a suffix     matches example.com and its subdomains
//! api.example.com a host       matches that name exactly
//! ```
//!
//! **A name entry never authorises a raw address, and an address entry never
//! authorises a name.** Letting a CIDR admit a hostname would mean resolving
//! every unlisted name to find out whether its answer lands inside the block,
//! which turns a refused destination into a DNS query an attacker chose. The
//! proxy already refuses to make that query before checking its list, and this
//! keeps that true. So `curl http://10.0.0.5` needs the block, and
//! `curl http://internal.corp` needs the name.
//!
//! Address order matters for a second reason: [`parse_ip_literal`] understands
//! every form the resolver does, so `2130706433` is an address here and can
//! never be mistaken for a name.

use darkwire_core::{ErrorKind, Result, WireError};

use crate::ip::{
    AddressCategory, IpFamily, ParsedCidr, ParsedIp, cidr_contains, classify_address, parse_cidr,
    parse_ip_literal,
};

/// The longest a DNS name may be, in bytes.
const MAX_NAME_BYTES: usize = 253;

/// One entry in an allow-list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowEntry {
    /// A CIDR block.
    Block(ParsedCidr),
    /// A single address.
    Address(ParsedIp),
    /// A name and everything under it. Stored without the leading dot.
    Suffix(String),
    /// One name, exactly.
    Host(String),
}

impl AllowEntry {
    /// The nftables family keyword and destination this entry permits, or
    /// `None` for a name, which a packet filter cannot express and which the
    /// egress proxy enforces instead.
    ///
    /// Rendered from the parsed value rather than echoed from the operator's
    /// text, because these strings are written into a ruleset. An entry of
    /// `0.0.0.0/0; accept` that survived to a rule would be a rule injection;
    /// one reprinted from four bytes and a prefix cannot be.
    pub fn as_filter_rule(&self) -> Option<(&'static str, String)> {
        let (family, destination) = match self {
            AllowEntry::Block(block) => (
                block.family,
                format!(
                    "{}/{}",
                    ParsedIp {
                        family: block.family,
                        canonical: String::new(),
                        bytes: block.bytes.clone(),
                    }
                    .to_ip_addr(),
                    block.prefix
                ),
            ),
            AllowEntry::Address(address) => (address.family, address.to_ip_addr().to_string()),
            AllowEntry::Suffix(_) | AllowEntry::Host(_) => return None,
        };
        Some((
            match family {
                IpFamily::V4 => "ip",
                IpFamily::V6 => "ip6",
            },
            destination,
        ))
    }
}

fn refuse(message: impl Into<String>, entry: &str) -> WireError {
    WireError::new(ErrorKind::Config, message).with_detail("entry", entry)
}

/// Whether a name is one DNS would carry.
///
/// Deliberately stricter than DNS itself: no underscore, no wildcard, no empty
/// label. A wildcard is the shape an operator reaches for and it is refused on
/// purpose, because `.example.com` already says what `*.example.com` means and
/// two spellings of one idea is how the three old lists drifted apart.
fn is_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// A category no entry may unlock, however it is written.
///
/// The packet filter has no notion of [`crate::BLOCKED_RANGES`]: an entry of
/// `169.254.0.0/16` would become `ip daddr 169.254.0.0/16 accept` and raw TCP
/// would reach the cloud metadata endpoint. There is no second line of defence
/// behind this check, so it is the one that has to hold.
///
/// A prefix that merely *overlaps* a blocked range is not refused. `0.0.0.0/0`
/// is not inside `0.0.0.0/8`, and an operator writing "everything" means it.
fn hard_blocked(entry: &AllowEntry) -> Option<&'static str> {
    let probe = match entry {
        AllowEntry::Address(address) => address.clone(),
        AllowEntry::Block(block) => ParsedIp {
            family: block.family,
            canonical: String::new(),
            bytes: block.bytes.clone(),
        },
        _ => return None,
    };
    let range = classify_address(&probe)?;
    if !matches!(
        range.category,
        AddressCategory::LinkLocal
            | AddressCategory::Multicast
            | AddressCategory::Unspecified
            | AddressCategory::Reserved
    ) {
        return None;
    }
    // A block is only inside the range if the range is at least as general.
    if let AllowEntry::Block(block) = entry
        && let Some(blocked) = parse_cidr(range.cidr)
        && block.prefix < blocked.prefix
    {
        return None;
    }
    Some(range.label)
}

/// Parses one entry, refusing anything no enforcement point could honour.
pub fn parse_allow_entry(text: &str) -> Result<AllowEntry> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(refuse("An egress entry is empty", text));
    }

    let entry = if let Some(block) = parse_cidr(trimmed) {
        AllowEntry::Block(block)
    } else if let Some(address) = parse_ip_literal(trimmed) {
        AllowEntry::Address(address)
    } else if let Some(bare) = trimmed.strip_prefix('.') {
        let lowered = bare.to_ascii_lowercase();
        if !is_dns_name(&lowered) {
            return Err(refuse(
                format!(
                    "Egress entry \"{trimmed}\" is not a name, an address or a CIDR block.\n  \
                     Write a name (api.example.com), a suffix (.example.com), an address\n  \
                     (93.184.216.34) or a block (10.0.0.0/8). Wildcards are not accepted:\n  \
                     \".example.com\" already covers every subdomain."
                ),
                trimmed,
            ));
        }
        AllowEntry::Suffix(lowered)
    } else {
        let lowered = trimmed.to_ascii_lowercase();
        if !is_dns_name(&lowered) {
            return Err(refuse(
                format!(
                    "Egress entry \"{trimmed}\" is not a name, an address or a CIDR block.\n  \
                     Write a name (api.example.com), a suffix (.example.com), an address\n  \
                     (93.184.216.34) or a block (10.0.0.0/8). Wildcards are not accepted:\n  \
                     \".example.com\" already covers every subdomain."
                ),
                trimmed,
            ));
        }
        AllowEntry::Host(lowered)
    };

    if let Some(label) = hard_blocked(&entry) {
        return Err(refuse(
            format!(
                "Egress entry \"{trimmed}\" is in the {label} range, which nothing may reach.\n  \
                 It covers the cloud metadata endpoint and the addresses an SSRF is aimed at,\n  \
                 and the packet filter has no second check behind this one."
            ),
            trimmed,
        ));
    }
    Ok(entry)
}

/// A parsed allow-list. Empty reaches nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AllowList {
    entries: Vec<AllowEntry>,
}

impl AllowList {
    /// Parses every entry, failing on the first one no enforcement point could
    /// honour.
    pub fn parse(entries: &[String]) -> Result<AllowList> {
        Ok(AllowList {
            entries: entries
                .iter()
                .map(|entry| parse_allow_entry(entry))
                .collect::<Result<Vec<_>>>()?,
        })
    }

    /// Whether it names nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry a packet filter can express, rendered, in the order given.
    pub fn filter_rules(&self) -> impl Iterator<Item = (&'static str, String)> {
        self.entries.iter().filter_map(AllowEntry::as_filter_rule)
    }

    /// The entry matching a destination host, which may itself be an address.
    ///
    /// An address host is matched against the address entries, never against
    /// the names: see the module header.
    pub fn match_host(&self, host: &str) -> Option<&AllowEntry> {
        if let Some(literal) = parse_ip_literal(host) {
            return self.match_address(&literal);
        }
        let needle = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.entries.iter().find(|entry| match entry {
            AllowEntry::Host(name) => needle == *name,
            AllowEntry::Suffix(name) => needle == *name || needle.ends_with(&format!(".{name}")),
            _ => false,
        })
    }

    /// The entry matching an address.
    pub fn match_address(&self, ip: &ParsedIp) -> Option<&AllowEntry> {
        self.entries.iter().find(|entry| match entry {
            AllowEntry::Address(address) => {
                address.bytes == ip.bytes && address.family == ip.family
            }
            AllowEntry::Block(block) => cidr_contains(block, ip),
            _ => false,
        })
    }
}
