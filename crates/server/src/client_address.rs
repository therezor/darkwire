//! Who sent a request, when a reverse proxy may stand in front.
//!
//! The socket peer is the only address a request carries that its sender
//! cannot forge, so it is the answer by default. Behind a proxy every request
//! arrives from the proxy, and the login throttle and the rate limits would
//! then hold one bucket for every caller. `server.trustedProxies` names the
//! peers whose `X-Forwarded-For` is believed. The header is read from the
//! right, skipping hops that are themselves trusted, so the first address
//! found is the one the outermost trusted proxy saw. A client can prepend
//! whatever it likes, and none of it is read.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Extensions, HeaderMap};
use axum::middleware::Next;
use axum::response::Response;
use darkwire_security::{ParsedCidr, ParsedIp, cidr_contains, parse_cidr, parse_ip_literal};

/// The caller's address, resolved once per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAddress(pub String);

/// `server.trustedProxies`, parsed.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    blocks: Vec<ParsedCidr>,
}

impl TrustedProxies {
    /// Parses each entry as a CIDR block or a single address.
    pub fn new(entries: &[String]) -> TrustedProxies {
        let blocks = entries
            .iter()
            .filter_map(|entry| {
                parse_cidr(entry).or_else(|| {
                    let ip = parse_ip_literal(entry)?;
                    let prefix = u32::try_from(ip.bytes.len() * 8).ok()?;
                    Some(ParsedCidr {
                        family: ip.family,
                        bytes: ip.bytes,
                        prefix,
                    })
                })
            })
            .collect();
        TrustedProxies { blocks }
    }

    fn trusts(&self, ip: &ParsedIp) -> bool {
        self.blocks.iter().any(|block| cidr_contains(block, ip))
    }

    /// The caller behind `peer`, given the request's headers.
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> String {
        let Some(peer) = parse_ip_literal(&peer.to_string()) else {
            return peer.to_string();
        };
        if !self.trusts(&peer) {
            return peer.canonical;
        }
        let hops = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .collect::<Vec<_>>();
        for hop in hops.iter().rev() {
            // Strict parsing: a hop the standard library will not read is not
            // one to key a throttle on, and the proxy is the safer answer.
            let Some(ip) = hop
                .trim()
                .parse::<IpAddr>()
                .ok()
                .and_then(|ip| parse_ip_literal(&ip.to_string()))
            else {
                break;
            };
            if !self.trusts(&ip) {
                return ip.canonical;
            }
        }
        peer.canonical
    }
}

/// Resolves the caller once, before any limit or handler asks who it is.
pub async fn resolve_client(
    State(proxies): State<Arc<TrustedProxies>>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        let address = proxies.client(peer.ip(), request.headers());
        request.extensions_mut().insert(ClientAddress(address));
    }
    next.run(request).await
}

/// Who is being throttled.
///
/// A request with no recorded peer, which is an in-process test service,
/// shares one bucket. That is what a test wants and is unreachable over a
/// socket.
pub fn client_of(extensions: &Extensions) -> String {
    extensions
        .get::<ClientAddress>()
        .map_or_else(|| "local".to_owned(), |address| address.0.clone())
}
