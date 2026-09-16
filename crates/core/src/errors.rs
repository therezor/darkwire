//! The error taxonomy.
//!
//! Every failure that crosses a crate boundary carries a `kind` from a closed
//! set. Nothing anywhere may branch on the *text* of an error: a model that
//! legitimately writes "rate limit" in its answer must not trigger a retry, and
//! a tool whose output legitimately begins with "Error" must not be recorded as
//! a failure. The kind is the truth; the message is for humans.

use std::error::Error as StdError;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The closed set of failure kinds. Serialises as `snake_case`, which is the
/// spelling the wire and the logs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Malformed or unloadable configuration.
    Config,
    /// A caller supplied arguments that failed validation.
    InvalidInput,
    /// The named thing does not exist.
    NotFound,
    /// The operation contradicts current state.
    Conflict,
    /// An approval was refused, or an extension lacked a declared capability.
    PermissionDenied,
    /// A path resolved outside the workspace jail. Always security-relevant.
    JailEscape,
    /// Transport-level failure: DNS, TCP, TLS, a blocked SSRF target.
    Network,
    /// The provider accepted the connection and rejected the request.
    Provider,
    /// A tool failed.
    Tool,
    /// A deadline passed.
    Timeout,
    /// The turn was cancelled. Never an error the user needs to see.
    Aborted,
    /// The other side asked for a slower pace.
    RateLimited,
    /// SQLite, or the filesystem underneath it.
    Storage,
    /// An extension failed.
    Extension,
    /// An invariant this codebase is supposed to uphold did not hold.
    Internal,
}

impl ErrorKind {
    /// Every kind, in the order the wire lists them.
    pub const ALL: [ErrorKind; 15] = [
        ErrorKind::Config,
        ErrorKind::InvalidInput,
        ErrorKind::NotFound,
        ErrorKind::Conflict,
        ErrorKind::PermissionDenied,
        ErrorKind::JailEscape,
        ErrorKind::Network,
        ErrorKind::Provider,
        ErrorKind::Tool,
        ErrorKind::Timeout,
        ErrorKind::Aborted,
        ErrorKind::RateLimited,
        ErrorKind::Storage,
        ErrorKind::Extension,
        ErrorKind::Internal,
    ];

    /// The `snake_case` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Config => "config",
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::JailEscape => "jail_escape",
            ErrorKind::Network => "network",
            ErrorKind::Provider => "provider",
            ErrorKind::Tool => "tool",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Aborted => "aborted",
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::Storage => "storage",
            ErrorKind::Extension => "extension",
            ErrorKind::Internal => "internal",
        }
    }

    /// Parses the `snake_case` spelling.
    pub fn parse(value: &str) -> Option<ErrorKind> {
        ErrorKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }

    /// Whether a kind is worth trying again *without changing anything*.
    ///
    /// Deliberately conservative. `Provider` is false because the overwhelmingly
    /// common cause is a malformed request, and blind retries against a 400 burn
    /// quota to reach the same answer; the resilience decorator overrides this
    /// per response, where the status code is actually known.
    pub fn default_retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::Network | ErrorKind::Timeout | ErrorKind::RateLimited
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The one error type every crate below the binary returns.
///
/// `details` must stay JSON-serialisable: it goes straight to the logger,
/// which redacts it by path.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WireError {
    /// Which closed kind this is.
    pub kind: ErrorKind,
    /// For humans. Never branched on.
    pub message: String,
    /// Whether retrying unchanged might succeed. Defaults per kind.
    pub retryable: bool,
    /// Structured context for the log line.
    pub details: Map<String, Value>,
    #[source]
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

/// `Result` with the crate's error.
pub type Result<T> = std::result::Result<T, WireError>;

impl WireError {
    /// A new error of `kind` with the kind's default `retryable`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: kind.default_retryable(),
            details: Map::new(),
            source: None,
        }
    }

    /// The `aborted` error, built from many places.
    pub fn aborted(what: &str) -> Self {
        Self::new(ErrorKind::Aborted, format!("{what} aborted"))
    }

    /// Overrides the kind's default `retryable`.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Adds one structured detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Replaces the structured details.
    #[must_use]
    pub fn with_details(mut self, details: Map<String, Value>) -> Self {
        self.details = details;
        self
    }

    /// Attaches the underlying error.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Whether this is the cancellation a caller asked for rather than a failure.
    pub fn is_aborted(&self) -> bool {
        self.kind == ErrorKind::Aborted
    }
}

impl From<rusqlite::Error> for WireError {
    fn from(error: rusqlite::Error) -> Self {
        WireError::new(ErrorKind::Storage, error.to_string()).with_source(error)
    }
}

impl From<std::io::Error> for WireError {
    fn from(error: std::io::Error) -> Self {
        let kind = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            _ => ErrorKind::Storage,
        };
        WireError::new(kind, error.to_string()).with_source(error)
    }
}
