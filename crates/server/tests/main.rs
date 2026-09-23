//! Every integration test for this crate, as one binary.
//!
//! One binary per crate rather than one per file: each binary links the whole
//! dependency tree, so a file each cost minutes of linking and gigabytes of
//! `target/` for no isolation that nextest does not already give per test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that does not hold is a failing test either way"
)]

mod agent_binding;
mod app;
mod approvals;
mod auth;
mod auth_matrix;
mod auth_store;
mod automation_port;
mod automation_store;
mod blocking;
mod boot;
mod context;
mod cursor;
mod ddl_parity;
mod errors;
mod exec_rules;
mod heartbeat;
mod hosts;
mod hub;
mod login_throttle;
mod manifest;
mod notifications;
mod openapi;
mod queries;
mod rate_limit;
mod replay;
mod routes_auth;
mod routes_automation;
mod routes_catalogue;
mod routes_files;
mod routes_notifications;
mod routes_providers;
mod routes_sessions;
mod routes_settings;
mod routes_system;
mod routes_workspaces;
mod routes_ws;
mod scheduler;
mod schema;
mod signing;
mod turn_log;
mod ui;
mod workspace;
