//! Id rules, re-exported from the protocol crate.
//!
//! The rules live there because both ends need them: the server turns an id
//! into a directory and decides whether one may be created, while the browser
//! mints one before the request. Two implementations of a rule whose whole job
//! is that two things cannot collide is not a rule.

pub use ghostai_protocol::ids::{
    DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, MAX_SLUG_ID_LENGTH, RESERVED_AGENT_IDS,
    RESERVED_DEVICE_NAMES, RESERVED_WORKSPACE_IDS, SLUG_ID_PATTERN, derive_agent_id,
    derive_workspace_id, is_agent_id, is_extension_id, is_slug_id, is_workspace_id, slugify,
};
