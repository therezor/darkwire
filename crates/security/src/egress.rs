//! Compile an approved egress request into rules for a private namespace.
//!
//! The rules go into a gateway container's own network namespace, which the
//! tool container then *shares*. That is what makes the filter unavoidable:
//! there is no interface in the tool container to route around, and no host
//! firewall table is touched, so an installation's own rules are never rewritten
//! by a turn.
//!
//! Two shapes, and they are alternatives rather than layers. A CIDR allow-list
//! is enforced here, in the packet filter, and is the only thing that works for
//! traffic that is not HTTP. A host allow-list is enforced by the egress proxy
//! instead: this filter then permits only the proxy's own uid and the loopback
//! port it listens on, so every connection is made by something that saw the
//! *name* rather than an address DNS rebinding chose. Mixing them would mean two
//! enforcement points disagreeing about one request, so it is refused.

use std::fmt::Write as _;

use darkwire_core::Result;
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};

use crate::environment::assert_gateway_compatible;
use crate::environment::invalid;
use crate::parse_cidr;

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
    if network.hosts.is_empty() {
        for range in &network.allow {
            if parse_cidr(range).is_none() {
                return Err(invalid(format!("Invalid CIDR: {range}")));
            }
            let family = if range.contains(':') { "ip6" } else { "ip" };
            writeln!(rules, " {family} daddr {range} accept")
                .map_err(|e| invalid(e.to_string()))?;
        }
        for resolver in &network.dns {
            let address: std::net::IpAddr = resolver
                .parse()
                .map_err(|_| invalid("DNS resolvers must be IP literals"))?;
            // An engine's embedded resolver answers on a loopback address that
            // belongs to the namespace the container shares with the gateway,
            // so a rule naming it would match traffic this filter never sees.
            // A restricted allow-list therefore needs a resolver it can
            // actually match a destination against.
            if address.is_loopback() {
                return Err(invalid(
                    "Restricted CIDR egress requires explicit non-loopback DNS resolvers",
                ));
            }
            let family = if address.is_ipv6() { "ip6" } else { "ip" };
            writeln!(
                rules,
                " {family} daddr {address} udp dport 53 accept\n {family} daddr {address} tcp dport 53 accept"
            )
            .map_err(|e| invalid(e.to_string()))?;
        }
    } else {
        if !network.allow.is_empty() {
            return Err(invalid(
                "Choose CIDR restrictions or domain proxy restrictions, not both",
            ));
        }
        writeln!(
            rules,
            " meta skuid {PROXY_UID} accept\n ip daddr 127.0.0.1 tcp dport {PROXY_PORT} accept"
        )
        .map_err(|e| invalid(e.to_string()))?;
    }
    rules.push_str(
        " }\n chain input { type filter hook input priority 0; policy drop;\n ct state established,related accept\n iifname lo accept\n }\n}\n",
    );
    Ok(rules)
}
