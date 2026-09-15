//! The environment service owns engine authority; clients only name granted
//! operations.
//!
//! Nothing a client sends names an image, a mount, a daemon argument or a shell
//! command. A request names an installed container definition, an installed
//! toolbox, one of that toolbox's grants and structured inputs — and the
//! service re-reads both definitions from the policy directory before and
//! during every call, rather than trusting the digest the caller resolved when
//! it started.
#![forbid(unsafe_code)]

pub mod container_pool;
pub mod proxy;
pub mod service;
