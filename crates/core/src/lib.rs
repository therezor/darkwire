//! The shared spine.
//!
//! Error taxonomy, injectable clock, cron, the on-disk layout, the SQLite
//! connection and stores, config load and save, the logger with its redaction
//! paths, message constructors and history windowing. No network and no child
//! processes here: those belong to `darkwire-security`, and a direct request or
//! spawn from this crate would bypass both guards. Filesystem access is allowed,
//! but only for paths DarkWire owns; nothing here resolves an agent-supplied path.
#![forbid(unsafe_code)]

pub mod clock;
pub mod config;
pub mod cron;
pub mod db;
pub mod errors;
pub mod frontmatter;
pub mod history;
pub mod ids;
pub mod logger;
pub mod memory;
pub mod message_bus;
pub mod messages;
pub mod paths;
pub mod session_store;
pub mod session_title;
pub mod sqlite_row;
pub mod workspace_files;
pub mod workspace_store;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use clock::{Clock, SystemClock, sleep};
pub use config::{LoadConfigOptions, LoadedConfig, load_config, parse_config, save_config};
pub use cron::{CronSpec, next_cron_run, parse_cron};
pub use db::Database;
pub use errors::{ErrorKind, Result, WireError};
pub use history::{HistoryOptions, SessionHistorySource, history_for_llm, session_history};
pub use logger::{JsonLayer, LogLevel, LogSink, LoggerOptions, create_logger, silent_logger};
pub use message_bus::{MessageBus, MessageBusOptions, PublishResult, RateLimiter};
pub use messages::{assistant_message, system_message, text_of, tool_message, user_message};
pub use paths::{
    HOME_ENV_VAR, ResolveWirePaths, WirePaths, ensure_dir, expand_home, extension_data_dir_for,
    extension_dir_for, resolve_path, shared_dir_for, workspace_dir_for,
};
pub use session_store::{
    SessionRecord, SessionStore, StoredMessageRecord, TurnStatsRecord, to_stored_message,
};
pub use sqlite_row::{RowReader, parse_metadata};
pub use workspace_store::{WorkspaceRecord, WorkspaceStore};
