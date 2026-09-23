//! Compile an approved egress request into rules for a private namespace.
//!
//! The rules go into a gateway container's own network namespace, which the
//! tool container then *shares*. That is what makes the filter unavoidable:
//! there is no interface in the tool container to route around, and no host
//! firewall table is touched, so an installation's own rules are never rewritten
//! by a turn.
//!
//! One allow-list, enforced in two places that cannot disagree because they read
//! the same parsed entries. Blocks and addresses become `daddr` rules here, in
//! the packet filter, which is the only thing that works for traffic that is not
//! HTTP. Names are left to the egress proxy, which sees the *name* rather than
//! an address DNS rebinding chose, so the filter permits the proxy's own uid and
//! the loopback port it listens on and nothing else.
//!
//! **Port 53 is never opened.** Nothing inside the namespace resolves a name;
//! the proxy resolves on the engine's side. That is what removed the resolver an
//! operator used to have to name, and it is why a name is reachable over HTTP
//! and HTTPS only.

use std::fmt::Write as _;

use darkwire_core::Result;
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};

use crate::allow::{AllowList, hard_blocked_filter_rules};
use crate::environment::assert_gateway_compatible;
use crate::environment::invalid;

/// The uid the egress proxy runs as. Reserved: traffic from it is accepted
/// unfiltered, so a tool container that could become it would be unfiltered too.
pub const PROXY_UID: &str = "65532";

/// The loopback port the egress proxy listens on inside the shared namespace.
pub const PROXY_PORT: u16 = 3128;

/// Compile one approved request. No engine-owned host tables are changed.
pub fn gateway_rules(
    container: &EnvironmentDefinition,
    network: &EnvironmentNetwork,
) -> Result<String> {
    if network.mode != NetworkMode::Allowlist {
        return Err(invalid("Gateway requires restricted egress"));
    }
    assert_gateway_compatible(container)?;
    let mut rules = String::from(
        "table inet darkwire {\n chain output { type filter hook output priority 0; policy drop;\n ct state established,related accept\n",
    );
    // The proxy is always reachable, because a list of addresses alone still
    // needs somewhere for a name to be refused rather than silently time out.
    writeln!(
        rules,
        " meta skuid {PROXY_UID} accept\n ip daddr 127.0.0.1 tcp dport {PROXY_PORT} accept"
    )
    .map_err(|e| invalid(e.to_string()))?;
    // Rendered from the parsed entry, never from the operator's text: these
    // strings become a ruleset, and an entry carrying a semicolon would be a
    // rule injection. `AllowList::parse` has already refused an entry inside a
    // range nothing may reach. A wider one such as `0.0.0.0/0` still contains
    // those ranges, so they are dropped first. A narrower one the parser
    // allowed inside them, such as `::1`, is accepted before the drops.
    let allow = AllowList::parse(&network.allow)?;
    let (before, after) = allow.filter_rules_around_drops();
    let accept = |(family, destination)| (family, destination, "accept");
    let drops =
        hard_blocked_filter_rules().map(|(family, destination)| (family, destination, "drop"));
    for (family, destination, verdict) in before
        .into_iter()
        .map(accept)
        .chain(drops)
        .chain(after.into_iter().map(accept))
    {
        writeln!(rules, " {family} daddr {destination} {verdict}")
            .map_err(|e| invalid(e.to_string()))?;
    }
    rules.push_str(
        " }\n chain input { type filter hook input priority 0; policy drop;\n ct state established,related accept\n iifname lo accept\n }\n}\n",
    );
    Ok(rules)
}
