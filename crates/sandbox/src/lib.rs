//! The sandbox service owns engine authority; clients only name approved
//! operations.
//!
//! Nothing a client sends names an image, a mount, a daemon argument or a shell
//! command. A request names an approved container, an approved toolbox, one of
//! that toolbox's grants and structured inputs — and the service re-checks both
//! approvals against the policy directory before and during every call, rather
//! than trusting the caller's word that they were approved when it started.
#![forbid(unsafe_code)]

pub mod container_pool;
pub mod proxy;
pub mod service;
