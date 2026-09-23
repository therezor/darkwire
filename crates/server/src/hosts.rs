//! Which `Host` names this server answers to.
//!
//! A browser's same-origin policy is keyed on the name, not the address. A page
//! on `evil.example` can point that name at `127.0.0.1` a moment after it loads
//! (DNS rebinding) and then read this server as its own origin. What it cannot
//! change is the `Host` the browser sends, which still says `evil.example`, so
//! refusing names this server does not recognise closes the hole for every
//! route at once: the API, the socket and the UI.
//!
//! Always accepted:
//!
//!  - `localhost` and anything under `.localhost`, which browsers resolve to
//!    loopback themselves.
//!  - Any IP literal. A page can only have an IP origin by being served from
//!    that address, so there is no name for an attacker to rebind.
//!  - The bind host and the machine's hostname, with its `.local` form.
//!
//! Anything else has to be listed in `server.allowedHosts`. The port is not
//! part of the check, because a rebinding attack changes the name and never
//! needs a new port. An entry that names a port is the one exception, and
//! matches only that port.
//!
//! A request with no `Host` at all is let through. Every browser sends one, so
//! its absence is a client that is not a browser, and those are not what this
//! defends against.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use darkwire_core::ErrorKind;
use darkwire_protocol::config::ServerConfig;
use darkwire_protocol::ws::ErrorCode;

use crate::errors::HttpError;

/// A host as compared: the name, lowercased, and the port when one was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    /// Lowercased, with any trailing dot and IPv6 brackets removed.
    pub name: String,
    /// The port, when the text carried one.
    pub port: Option<u16>,
}

impl Authority {
    /// Reads `name`, `name:port`, `[v6]` or `[v6]:port`.
    ///
    /// `None` for text that is none of those, which never matches anything.
    pub fn parse(raw: &str) -> Option<Authority> {
        let raw = raw.trim();
        let (name, port) = if let Some(rest) = raw.strip_prefix('[') {
            let (inside, after) = rest.split_once(']')?;
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':')?.parse().ok()?),
            };
            (inside, port)
        } else {
            match raw.rsplit_once(':') {
                // Two colons and no brackets is a bare IPv6 address.
                Some((name, _)) if name.contains(':') => (raw, None),
                Some((name, port)) => (name, Some(port.parse().ok()?)),
                None => (raw, None),
            }
        };
        let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        Some(Authority { name, port })
    }

    fn is_ip_literal(&self) -> bool {
        self.name.parse::<std::net::IpAddr>().is_ok()
    }
}

/// The names a server answers to.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    /// Accepted on any port: the bind host and the machine's names.
    builtin: Vec<String>,
    /// `server.allowedHosts`, parsed.
    configured: Vec<Authority>,
}

impl HostPolicy {
    /// The policy for a server bound as `server` says, on a machine called
    /// `hostname`.
    pub fn new(server: &ServerConfig, hostname: Option<&str>) -> HostPolicy {
        let mut builtin = Vec::new();
        if let Some(bind) = Authority::parse(&server.host) {
            builtin.push(bind.name);
        }
        if let Some(machine) = hostname.and_then(Authority::parse) {
            let name = machine.name;
            match name.strip_suffix(".local") {
                Some(bare) => builtin.push(bare.to_owned()),
                None if !name.contains('.') => builtin.push(format!("{name}.local")),
                None => {}
            }
            builtin.push(name);
        }
        HostPolicy {
            builtin,
            configured: server
                .allowed_hosts
                .iter()
                .filter_map(|entry| Authority::parse(entry))
                .collect(),
        }
    }

    /// The policy for this machine, reading its hostname from the system.
    pub fn for_this_machine(server: &ServerConfig) -> HostPolicy {
        let hostname = gethostname::gethostname();
        HostPolicy::new(server, hostname.to_str())
    }

    /// Whether a `Host` header value names this server.
    pub fn allows(&self, host: &str) -> bool {
        let Some(host) = Authority::parse(host) else {
            return false;
        };
        host.is_ip_literal()
            || host.name == "localhost"
            || host.name.ends_with(".localhost")
            || self.builtin.contains(&host.name)
            || self.lists(&host)
    }

    /// Whether `server.allowedHosts` names this authority.
    ///
    /// The socket's `Origin` check asks only this. The names that are always
    /// accepted describe this listener reached directly, where the `Origin`
    /// already matches the `Host`. Accepting `localhost` origins in general
    /// would let a page from another local server open the socket.
    pub fn lists(&self, host: &Authority) -> bool {
        self.configured.iter().any(|entry| {
            entry.name == host.name && entry.port.is_none_or(|port| host.port == Some(port))
        })
    }
}

/// Refuses a request whose `Host` this server does not answer to.
///
/// 421 Misdirected Request, which is what the status exists for: the request
/// reached a server that is not the one its name points at.
pub async fn require_known_host(
    State(policy): State<Arc<HostPolicy>>,
    request: Request,
    next: Next,
) -> Result<Response, HttpError> {
    // A `Host` that is not text is refused as a name nobody configured, not
    // waved through as a missing one.
    let host = match request.headers().get(header::HOST) {
        Some(value) => Some(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        None => request.uri().authority().map(|a| a.as_str().to_owned()),
    };
    if let Some(host) = host
        && !policy.allows(&host)
    {
        return Err(HttpError::new(
            StatusCode::MISDIRECTED_REQUEST,
            ErrorCode::BadRequest,
            ErrorKind::PermissionDenied,
            format!(
                "This server does not answer to the host name {host}. \
                 If it is yours, add it to server.allowedHosts."
            ),
        )
        .with_detail("host", host));
    }
    Ok(next.run(request).await)
}
