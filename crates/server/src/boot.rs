//! The question asked before a socket is opened.
//!
//! It is a refusal rather than a warning, and that distinction is the whole
//! point of this module. A warning about an unauthenticated bind scrolls past
//! in a terminal that nobody is watching, and what survives it is a
//! shell-capable agent answering to anyone who can route a packet to the host.
//! A process that will not start is impossible to miss and impossible to
//! ignore.
//!
//! An install with authentication on and no password set is *not* a second
//! refusal, because the one interface that could set a password is the one
//! thing such an install cannot reach — and starting anyway would answer 401 on
//! every route, which reads as a bug in the UI rather than as unfinished setup.
//! [`AuthStore::issue_setup_code`](crate::auth_store::AuthStore::issue_setup_code)
//! closes that loop instead: the server starts, prints a single-use code to the
//! terminal the operator is already looking at, and refuses everything else
//! until it is spent.

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::config::{Config, is_loopback_host};

/// Returns a `config` error if this configuration must not be served.
///
/// Called before the listener is bound, so a refusal costs nothing and leaves
/// nothing to tear down.
pub fn assert_boot_policy(config: &Config) -> Result<()> {
    let server = &config.server;
    if server.auth.enabled || is_loopback_host(&server.host) {
        return Ok(());
    }

    let host = &server.host;
    let port = server.port;
    Err(WireError::new(
        ErrorKind::Config,
        format!(
            "Refusing to start: server.host is \"{host}\" and server.auth.enabled is false.\n\
             \x20 Binding beyond loopback without authentication exposes an agent that can\n\
             \x20 read files and run commands to anyone who can reach {host}:{port}.\n\
             \x20 Set server.auth.enabled to true, or bind to 127.0.0.1."
        ),
    )
    .with_detail("host", host.clone())
    .with_detail("port", port))
}
