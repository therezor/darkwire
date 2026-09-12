//! Address literal parsing, and the ranges egress may never reach.
//!
//! A URL host is not safe to classify by string comparison, because the resolver
//! that eventually connects is far more liberal than a naive reader assumes.
//! `getaddrinfo` accepts `inet_aton` forms, so every one of these reaches
//! 127.0.0.1:
//!
//! ```text
//! http://2130706433/        decimal
//! http://0177.0.0.1/        octal
//! http://0x7f.1/            hex, with the last part absorbing the rest
//! http://127.1/             short form
//! http://[::ffff:7f00:1]/   IPv4 mapped into IPv6
//! ```
//!
//! A guard that checks `hostname == "localhost"`, or that only understands
//! dotted quads, passes all five. So parsing happens here, with the same
//! semantics the resolver uses, before anything is treated as a hostname —
//! [`parse_ip_literal`] returning `None` is what earns a string a DNS lookup.
//!
//! The blocked table is data, and deliberately wider than "private". Cloud
//! metadata at 169.254.169.254 is the single highest-value SSRF target in
//! existence and it is link-local, not private. The IPv6 transition prefixes
//! (6to4, Teredo, NAT64) embed IPv4 addresses and are blocked wholesale rather
//! than unwrapped, because a partially-correct unwrapper is worse than a refusal
//! for ranges no agent has a reason to fetch from.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// Which address family a literal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpFamily {
    /// IPv4, four bytes.
    V4,
    /// IPv6, sixteen bytes.
    V6,
}

impl IpFamily {
    /// `4` or `6`, the spelling logs and the wire use.
    pub fn number(self) -> u8 {
        match self {
            IpFamily::V4 => 4,
            IpFamily::V6 => 6,
        }
    }
}

/// A host string that turned out to be an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedIp {
    /// The family.
    pub family: IpFamily,
    /// Dotted quad, or the RFC 5952 compressed IPv6 form. For logs and messages.
    pub canonical: String,
    /// 4 bytes for IPv4, 16 for IPv6.
    pub bytes: Vec<u8>,
}

impl ParsedIp {
    /// The address as the standard library spells it, for a socket.
    pub fn to_ip_addr(&self) -> IpAddr {
        let mut four = [0u8; 4];
        let mut sixteen = [0u8; 16];
        match self.family {
            IpFamily::V4 => {
                four.copy_from_slice(&self.bytes);
                IpAddr::V4(Ipv4Addr::from(four))
            }
            IpFamily::V6 => {
                sixteen.copy_from_slice(&self.bytes);
                IpAddr::V6(Ipv6Addr::from(sixteen))
            }
        }
    }
}

/// Why a range is blocked. The network policy keys off this: loopback is
/// unlockable because a self-hosted agent's own model server is at 127.0.0.1,
/// and private because a LAN deployment is legitimate. Nothing unlocks
/// link-local — that is the metadata endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AddressCategory {
    /// 127/8 and `::1`.
    Loopback,
    /// RFC 1918, carrier-grade NAT and unique-local.
    Private,
    /// 169.254/16 and `fe80::/10`, including cloud metadata.
    LinkLocal,
    /// Multicast in either family.
    Multicast,
    /// The unspecified address and the IPv4-compatible IPv6 form.
    Unspecified,
    /// Documentation, benchmarking and the transition prefixes.
    Reserved,
}

/// One entry in the blocked table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRange {
    /// The block, as `address/prefix`.
    pub cidr: &'static str,
    /// For messages.
    pub label: &'static str,
    /// What policy may unlock it.
    pub category: AddressCategory,
}

const fn range(cidr: &'static str, label: &'static str, category: AddressCategory) -> AddressRange {
    AddressRange {
        cidr,
        label,
        category,
    }
}

/// Every range egress is refused by default, most general first.
pub const BLOCKED_RANGES: [AddressRange; 25] = [
    range("0.0.0.0/8", "this network", AddressCategory::Unspecified),
    range("10.0.0.0/8", "private", AddressCategory::Private),
    range(
        "100.64.0.0/10",
        "carrier-grade NAT",
        AddressCategory::Private,
    ),
    range("127.0.0.0/8", "loopback", AddressCategory::Loopback),
    range(
        "169.254.0.0/16",
        "link-local and cloud metadata",
        AddressCategory::LinkLocal,
    ),
    range("172.16.0.0/12", "private", AddressCategory::Private),
    range(
        "192.0.0.0/24",
        "IETF protocol assignments",
        AddressCategory::Reserved,
    ),
    range("192.0.2.0/24", "documentation", AddressCategory::Reserved),
    range(
        "192.88.99.0/24",
        "6to4 relay anycast",
        AddressCategory::Reserved,
    ),
    range("192.168.0.0/16", "private", AddressCategory::Private),
    range("198.18.0.0/15", "benchmarking", AddressCategory::Reserved),
    range(
        "198.51.100.0/24",
        "documentation",
        AddressCategory::Reserved,
    ),
    range("203.0.113.0/24", "documentation", AddressCategory::Reserved),
    range("224.0.0.0/4", "multicast", AddressCategory::Multicast),
    range(
        "240.0.0.0/4",
        "reserved, including broadcast",
        AddressCategory::Reserved,
    ),
    // `::/96` covers both the unspecified address and the deprecated
    // IPv4-compatible form `::127.0.0.1`. IPv4-*mapped* addresses are not here:
    // `parse_ip_literal` returns them as family 4, so they meet the table above.
    range(
        "::/96",
        "unspecified and IPv4-compatible",
        AddressCategory::Unspecified,
    ),
    range("::1/128", "loopback", AddressCategory::Loopback),
    range(
        "64:ff9b::/96",
        "NAT64, embeds IPv4",
        AddressCategory::Reserved,
    ),
    range("100::/64", "discard-only", AddressCategory::Reserved),
    range(
        "2001::/32",
        "Teredo, embeds IPv4",
        AddressCategory::Reserved,
    ),
    range("2001:db8::/32", "documentation", AddressCategory::Reserved),
    range("2002::/16", "6to4, embeds IPv4", AddressCategory::Reserved),
    range("fc00::/7", "unique local", AddressCategory::Private),
    range("fe80::/10", "link-local", AddressCategory::LinkLocal),
    range("ff00::/8", "multicast", AddressCategory::Multicast),
];

// Parsing

fn is_hex_part(part: &str) -> bool {
    let Some(digits) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_octal_part(part: &str) -> bool {
    let Some(digits) = part.strip_prefix('0') else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| (b'0'..=b'7').contains(&b))
}

fn is_decimal_part(part: &str) -> bool {
    part == "0"
        || (part
            .bytes()
            .next()
            .is_some_and(|b| (b'1'..=b'9').contains(&b))
            && part.bytes().all(|b| b.is_ascii_digit()))
}

/// One `inet_aton` component: hex, octal or decimal, in that precedence.
///
/// Values wider than the address are refused by the caller; parsing saturates
/// rather than failing so a 40-digit part still reaches that check.
fn parse_numeric_part(part: &str) -> Option<u64> {
    let (digits, radix) = if is_hex_part(part) {
        (&part[2..], 16)
    } else if is_octal_part(part) {
        (&part[1..], 8)
    } else if is_decimal_part(part) {
        (part, 10)
    } else {
        return None;
    };
    Some(digits.bytes().fold(0u64, |acc, b| {
        let digit = u64::from(char::from(b).to_digit(radix).unwrap_or(0));
        acc.saturating_mul(radix.into()).saturating_add(digit)
    }))
}

fn ipv4_from_bytes(bytes: [u8; 4]) -> ParsedIp {
    ParsedIp {
        family: IpFamily::V4,
        canonical: bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join("."),
        bytes: bytes.to_vec(),
    }
}

/// `inet_aton` semantics: one to four parts, each hex/octal/decimal, where the
/// final part fills all remaining low-order bytes.
fn parse_ipv4_numeric(text: &str) -> Option<ParsedIp> {
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    let last_index = parts.len() - 1;
    let mut bytes = [0u8; 4];
    let mut last = 0u64;
    for (index, part) in parts.iter().enumerate() {
        let value = parse_numeric_part(part)?;
        if index == last_index {
            // The final part fills every byte the earlier parts did not.
            let width = 8 * (4 - last_index);
            if value >= 1u64 << width {
                return None;
            }
            last = value;
            break;
        }
        if value > 0xff {
            return None;
        }
        bytes[index] = u8::try_from(value).unwrap_or(0);
    }
    let mut remainder = last;
    for index in (last_index..4).rev() {
        bytes[index] = u8::try_from(remainder % 0x100).unwrap_or(0);
        remainder /= 0x100;
    }
    Some(ipv4_from_bytes(bytes))
}

/// Strict dotted quad. What is legal *inside* an IPv6 literal — no octal, no
/// short forms.
fn parse_dotted_quad(text: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (index, part) in parts.iter().enumerate() {
        if !is_decimal_part(part) {
            return None;
        }
        bytes[index] = part.parse::<u8>().ok()?;
    }
    Some(bytes)
}

/// `terminal` says whether this half ends the address. An embedded IPv4 address
/// is only legal in the final position of the whole literal, so `1.2.3.4::5` has
/// to be refused even though the quad ends the half it appears in.
fn ipv6_groups_to_bytes(groups: &[&str], terminal: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if group.is_empty() {
            return None;
        }
        if group.contains('.') {
            if !terminal || index != groups.len() - 1 {
                return None;
            }
            out.extend_from_slice(&parse_dotted_quad(group)?);
            continue;
        }
        if group.len() > 4 || !group.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u16::from_str_radix(group, 16).ok()?;
        out.extend_from_slice(&value.to_be_bytes());
    }
    (out.len() <= 16).then_some(out)
}

/// `::ffff:a.b.c.d` — an IPv4 address wearing an IPv6 costume.
fn is_ipv4_mapped(bytes: &[u8]) -> bool {
    bytes[..10].iter().all(|b| *b == 0) && bytes[10] == 0xff && bytes[11] == 0xff
}

fn format_ipv6(bytes: &[u8]) -> String {
    let groups: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();

    let mut best_start: Option<usize> = None;
    let mut best_length = 0;
    let mut run_start: Option<usize> = None;
    for index in 0..=groups.len() {
        if index < groups.len() && groups[index] == 0 {
            if run_start.is_none() {
                run_start = Some(index);
            }
            continue;
        }
        if let Some(start) = run_start {
            let length = index - start;
            // RFC 5952: only a run of two or more groups is compressed.
            if length > best_length && length > 1 {
                best_start = Some(start);
                best_length = length;
            }
            run_start = None;
        }
    }

    let text: Vec<String> = groups.iter().map(|g| format!("{g:x}")).collect();
    let Some(start) = best_start else {
        return text.join(":");
    };
    let head = text[..start].join(":");
    let tail = text[start + best_length..].join(":");
    format!("{head}::{tail}")
}

fn parse_ipv6(text: &str) -> Option<ParsedIp> {
    let halves: Vec<&str> = text.split("::").collect();
    if halves.len() > 2 {
        return None;
    }
    let head_text = halves[0];
    let tail_text = halves.get(1).copied();
    let head_groups: Vec<&str> = if head_text.is_empty() {
        Vec::new()
    } else {
        head_text.split(':').collect()
    };
    let tail_groups: Vec<&str> = match tail_text {
        None | Some("") => Vec::new(),
        Some(tail) => tail.split(':').collect(),
    };
    let head = ipv6_groups_to_bytes(&head_groups, tail_text.is_none())?;
    let tail = ipv6_groups_to_bytes(&tail_groups, true)?;

    let bytes: Vec<u8> = if tail_text.is_none() {
        if head.len() != 16 {
            return None;
        }
        head
    } else {
        // `::` must stand for at least one elided group, so the explicit halves
        // cannot themselves add up to a whole address.
        if head.len() + tail.len() > 14 {
            return None;
        }
        let mut bytes = vec![0u8; 16];
        bytes[..head.len()].copy_from_slice(&head);
        bytes[16 - tail.len()..].copy_from_slice(&tail);
        bytes
    };

    if is_ipv4_mapped(&bytes) {
        let mut four = [0u8; 4];
        four.copy_from_slice(&bytes[12..]);
        return Some(ipv4_from_bytes(four));
    }
    Some(ParsedIp {
        family: IpFamily::V6,
        canonical: format_ipv6(&bytes),
        bytes,
    })
}

/// Parses a host as an address literal, or returns `None` if it is a name.
///
/// `None` is the only thing that earns a host a DNS lookup, so anything the
/// resolver would treat as numeric must be recognised here.
pub fn parse_ip_literal(host: &str) -> Option<ParsedIp> {
    let mut text = host.trim();
    if let Some(inner) = text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        text = inner;
    }
    if let Some(zone) = text.find('%') {
        text = &text[..zone];
    }
    if text.contains(':') {
        return parse_ipv6(text);
    }
    // A trailing dot is the DNS root label on a name, and is ignored by the
    // resolver on a numeric form. `127.0.0.1.` must not slip through as a name.
    if let Some(trimmed) = text.strip_suffix('.') {
        text = trimmed;
    }
    parse_ipv4_numeric(text)
}

// Classification

/// A CIDR block, parsed once so containment is a byte comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCidr {
    /// The family.
    pub family: IpFamily,
    /// The network address. Host bits are *not* masked off — see [`cidr_contains`].
    pub bytes: Vec<u8>,
    /// The prefix length.
    pub prefix: u32,
}

/// Parses `10.0.0.0/8`, or `None` if it is not one.
///
/// Public because the sandbox egress allow-list needs exactly this and had no
/// business reimplementing it. Note the deliberate asymmetry with
/// [`BLOCKED_RANGES`]: this module's *policy* refuses private ranges for
/// [`crate::guarded_fetch`], but an engagement scope is `192.168.1.0/24` and is
/// entirely legitimate. Parsing is shared; policy is the caller's.
pub fn parse_cidr(text: &str) -> Option<ParsedCidr> {
    let slash = text.rfind('/')?;
    let parsed = parse_ip_literal(&text[..slash])?;
    let prefix_text = &text[slash + 1..];
    // A prefix is one to three digits or it is not a prefix: no sign, no
    // whitespace, nothing after the digits.
    if prefix_text.is_empty()
        || prefix_text.len() > 3
        || !prefix_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let prefix: u32 = prefix_text.parse().ok()?;
    if prefix > u32::try_from(parsed.bytes.len() * 8).unwrap_or(u32::MAX) {
        return None;
    }
    Some(ParsedCidr {
        family: parsed.family,
        bytes: parsed.bytes,
        prefix,
    })
}

/// Whether an address falls inside a block. Families must match.
pub fn cidr_contains(cidr: &ParsedCidr, ip: &ParsedIp) -> bool {
    cidr.family == ip.family && matches_prefix(&ip.bytes, &cidr.bytes, cidr.prefix)
}

struct CompiledRange {
    range: AddressRange,
    cidr: ParsedCidr,
}

/// Sorted most-specific-first, so `::1` classifies as loopback rather than as
/// the `::/96` block that also contains it. Category drives policy — a
/// deployment that allows loopback so it can reach its own model server must
/// get the same answer for `[::1]` as for `127.0.0.1`.
static COMPILED_RANGES: LazyLock<Vec<CompiledRange>> = LazyLock::new(|| {
    let mut compiled: Vec<CompiledRange> = BLOCKED_RANGES
        .iter()
        .filter_map(|range| {
            parse_cidr(range.cidr).map(|cidr| CompiledRange {
                range: *range,
                cidr,
            })
        })
        .collect();
    compiled.sort_by_key(|compiled| std::cmp::Reverse(compiled.cidr.prefix));
    compiled
});

fn matches_prefix(address: &[u8], network: &[u8], prefix: u32) -> bool {
    let whole_bytes = (prefix >> 3) as usize;
    if address[..whole_bytes] != network[..whole_bytes] {
        return false;
    }
    let remaining_bits = prefix & 7;
    if remaining_bits == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - remaining_bits);
    let left = address.get(whole_bytes).copied().unwrap_or(0);
    let right = network.get(whole_bytes).copied().unwrap_or(0);
    (left & mask) == (right & mask)
}

/// The range an address falls in, or `None` if it is publicly routable.
pub fn classify_address(ip: &ParsedIp) -> Option<&'static AddressRange> {
    COMPILED_RANGES
        .iter()
        .find(|compiled| cidr_contains(&compiled.cidr, ip))
        .map(|compiled| &compiled.range)
}
