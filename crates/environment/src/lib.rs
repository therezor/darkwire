//! The environment service owns engine authority; a client only names where a
//! command should run.
//!
//! Nothing a client sends names an image, a mount, a daemon argument or a shell
//! command. A request names an installed container definition and an argv array
//! — and the service re-reads that definition from the policy directory before
//! and during every call, rather than trusting the digest the caller resolved
//! when it started.
#![forbid(unsafe_code)]

pub mod container_pool;
pub mod proxy;
pub mod service;
