//! Structured logging.
//!
//! The redaction list is the point of this module. An agent runtime logs
//! provider requests, tool arguments, extension config and channel payloads,
//! and every one of those is a plausible carrier for an API key, a bearer token
//! or a Telegram bot secret. Redaction is applied by *path* rather than by
//! scanning values, because scanning cannot tell a key from any other opaque
//! string and would either miss secrets or mangle legitimate output.
//!
//! Paths are matched against the object a log line becomes, so the convention
//! is to log structured context, `tracing::info!(tool = name, "executing")`,
//! rather than interpolating it into the message string, where no redaction
//! can reach it. A nested value is logged with `%`, as a `serde_json::Value`:
//! `tracing::info!(headers = %json_value, "request")`. Its compact JSON form is
//! parsed back into a tree so `headers.authorization` is a path the list can
//! see; any other `%` or `?` value stays a string.
//!
//! Lines are JSON, one per line, with the field names an operator's existing
//! log tooling already reads: numeric `level` (10 trace, 20 debug, 30 info, 40
//! warn, 50 error), `time` as epoch milliseconds, `pid`, `hostname`, `msg`.
//! Epoch milliseconds rather than a date string so log timestamps are directly
//! comparable with the `created_at_ms` columns in the database.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::Registry;

use crate::clock::{Clock, SystemClock};

/// Wildcards cover the shapes secrets actually arrive in: a bare field, one
/// level inside a named bag (`provider.apiKey`), and one level inside a keyed
/// record (`providers.openai.apiKey`). Deeper nesting is not enumerated;
/// instead, callers log the specific sub-object they mean, which is better
/// practice anyway and keeps this list short enough to audit.
pub const REDACT_PATHS: [&str; 31] = [
    "apiKey",
    "api_key",
    "token",
    "accessToken",
    "refreshToken",
    "clientSecret",
    "password",
    "passphrase",
    "secret",
    "authorization",
    "cookie",
    "*.apiKey",
    "*.api_key",
    "*.token",
    "*.accessToken",
    "*.refreshToken",
    "*.clientSecret",
    "*.password",
    "*.passphrase",
    "*.secret",
    "*.authorization",
    "*.cookie",
    "*.*.apiKey",
    "*.*.token",
    "*.*.secret",
    "headers.authorization",
    "headers.cookie",
    "headers[\"set-cookie\"]",
    "req.headers.authorization",
    "req.headers.cookie",
    "res.headers[\"set-cookie\"]",
];

/// What a redacted value is replaced with.
pub const REDACT_CENSOR: &str = "[redacted]";

/// One segment of a redaction path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Any key at this level. Array indices count as keys.
    Wildcard,
    /// This key exactly.
    Key(String),
}

/// Splits `a.b["c-d"].*` into its segments.
fn segments(path: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("[\"") {
            let end = after.find("\"]").unwrap_or(after.len());
            out.push(Segment::Key(after[..end].to_owned()));
            rest = after.get(end + 2..).unwrap_or("");
            continue;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let piece = &rest[..end];
        out.push(if piece == "*" {
            Segment::Wildcard
        } else {
            Segment::Key(piece.to_owned())
        });
        rest = &rest[end..];
    }
    out
}

/// Replaces every value at one of [`REDACT_PATHS`] with [`REDACT_CENSOR`].
///
/// Public so a caller holding structured data bound for somewhere other than
/// the log, an error's `details` on its way to a client, can apply the same
/// list. A wildcard matches every key at its level, including array indices, so
/// a secret inside a list of providers is not a way past the list.
pub fn redact(value: &mut Value) {
    for path in REDACT_PATHS {
        redact_path(value, &segments(path));
    }
}

fn redact_path(value: &mut Value, path: &[Segment]) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    match (head, value) {
        (Segment::Key(key), Value::Object(map)) => {
            if let Some(child) = map.get_mut(key) {
                redact_child(child, tail);
            }
        }
        (Segment::Wildcard, Value::Object(map)) => {
            for child in map.values_mut() {
                redact_child(child, tail);
            }
        }
        (Segment::Wildcard, Value::Array(items)) => {
            for child in items {
                redact_child(child, tail);
            }
        }
        _ => {}
    }
}

fn redact_child(child: &mut Value, tail: &[Segment]) {
    if tail.is_empty() {
        *child = Value::from(REDACT_CENSOR);
    } else {
        redact_path(child, tail);
    }
}

/// The verbosity threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogLevel {
    /// Everything.
    Trace,
    /// Diagnostics.
    Debug,
    /// The default.
    Info,
    /// Something to look at.
    Warn,
    /// Something failed.
    Error,
    /// Nothing below a crash. Nothing here emits at this level, so it drops
    /// every line while staying a valid configuration.
    Fatal,
    /// Nothing at all.
    Silent,
}

impl LogLevel {
    /// Parses the configuration spelling.
    pub fn parse(value: &str) -> Option<LogLevel> {
        Some(match value {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "info" => LogLevel::Info,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            "fatal" => LogLevel::Fatal,
            "silent" => LogLevel::Silent,
            _ => return None,
        })
    }

    /// The configuration spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Fatal => "fatal",
            LogLevel::Silent => "silent",
        }
    }

    /// The numeric level written on every line.
    pub fn number(self) -> u8 {
        match self {
            LogLevel::Trace => 10,
            LogLevel::Debug => 20,
            LogLevel::Info => 30,
            LogLevel::Warn => 40,
            LogLevel::Error => 50,
            LogLevel::Fatal => 60,
            LogLevel::Silent => u8::MAX,
        }
    }

    fn of(level: Level) -> LogLevel {
        match level {
            Level::TRACE => LogLevel::Trace,
            Level::DEBUG => LogLevel::Debug,
            Level::INFO => LogLevel::Info,
            Level::WARN => LogLevel::Warn,
            Level::ERROR => LogLevel::Error,
        }
    }
}

/// The level to run at: the explicit one, else `GHOSTAI_LOG_LEVEL`, else
/// `LOG_LEVEL`, else `info`.
///
/// An unrecognised level must not take the process down at boot: losing the
/// logger loses the diagnostics needed to work out why, so a typo reads as
/// `info`.
pub fn resolve_level<S: std::hash::BuildHasher>(
    explicit: Option<LogLevel>,
    env: &HashMap<String, String, S>,
) -> LogLevel {
    if let Some(level) = explicit {
        return level;
    }
    env.get("GHOSTAI_LOG_LEVEL")
        .or_else(|| env.get("LOG_LEVEL"))
        .and_then(|value| LogLevel::parse(value))
        .unwrap_or(LogLevel::Info)
}

/// Where finished lines go.
pub trait LogSink: Send + Sync {
    /// Writes one line, without its terminator.
    fn write_line(&self, line: &str);
}

/// Standard output, one line per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdoutSink;

impl LogSink for StdoutSink {
    fn write_line(&self, line: &str) {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        // A closed stdout is not a reason to stop the process that was logging
        // to it; there is nowhere left to report the failure anyway.
        let _ = writeln!(handle, "{line}");
    }
}

/// Inputs to [`create_logger`].
pub struct LoggerOptions {
    /// Defaults to `GHOSTAI_LOG_LEVEL`, then `LOG_LEVEL`, then `info`.
    pub level: Option<LogLevel>,
    /// Component name, emitted as `name` on every line.
    pub name: Option<String>,
    /// Defaults to stdout. Tests pass a capturing sink and assert on the JSON.
    pub sink: Option<Arc<dyn LogSink>>,
    /// Extra fields on every line: session key, turn id, channel.
    pub base: Map<String, Value>,
    /// The environment to consult for the level; defaults to the process's.
    pub env: Option<HashMap<String, String>>,
    /// Where `time` comes from.
    pub clock: Arc<dyn Clock>,
    /// Overrides the host's name.
    pub hostname: Option<String>,
}

impl Default for LoggerOptions {
    fn default() -> Self {
        Self {
            level: None,
            name: None,
            sink: None,
            base: Map::new(),
            env: None,
            clock: Arc::new(SystemClock),
            hostname: None,
        }
    }
}

impl std::fmt::Debug for LoggerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggerOptions")
            .field("level", &self.level)
            .field("name", &self.name)
            .field("base", &self.base)
            .field("hostname", &self.hostname)
            .finish_non_exhaustive()
    }
}

/// The layer that turns events into redacted JSON lines.
pub struct JsonLayer {
    threshold: u8,
    sink: Arc<dyn LogSink>,
    clock: Arc<dyn Clock>,
    /// `name` and the caller's base fields, in that order.
    base: Map<String, Value>,
    pid: u32,
    hostname: String,
}

impl std::fmt::Debug for JsonLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonLayer")
            .field("threshold", &self.threshold)
            .field("base", &self.base)
            .field("pid", &self.pid)
            .field("hostname", &self.hostname)
            .finish_non_exhaustive()
    }
}

/// Builds the layer. Attach it to a registry with [`subscriber`], or compose
/// it with other layers.
pub fn create_logger(options: LoggerOptions) -> JsonLayer {
    let env = options
        .env
        .unwrap_or_else(|| std::env::vars().collect::<HashMap<_, _>>());
    let level = resolve_level(options.level, &env);

    let mut base = Map::new();
    if let Some(name) = options.name {
        base.insert("name".to_owned(), Value::from(name));
    }
    base.extend(options.base);

    JsonLayer {
        threshold: level.number(),
        sink: options.sink.unwrap_or_else(|| Arc::new(StdoutSink)),
        clock: options.clock,
        base,
        pid: std::process::id(),
        hostname: options
            .hostname
            .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned()),
    }
}

/// A complete subscriber: a registry carrying one [`JsonLayer`].
pub fn subscriber(options: LoggerOptions) -> impl Subscriber + Send + Sync {
    Registry::default().with(create_logger(options))
}

/// A subscriber that discards everything.
///
/// Every component logs through `tracing` rather than an optional handle, so
/// there is a single code path instead of a check at every call site. This is
/// what tests install when the assertion is about behaviour rather than output.
pub fn silent_logger() -> impl Subscriber + Send + Sync {
    subscriber(LoggerOptions {
        level: Some(LogLevel::Silent),
        ..LoggerOptions::default()
    })
}

impl JsonLayer {
    fn accepts(&self, metadata: &Metadata<'_>) -> bool {
        LogLevel::of(*metadata.level()).number() >= self.threshold
    }
}

impl<S: Subscriber> Layer<S> for JsonLayer {
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        self.accepts(metadata)
    }

    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if !self.accepts(event.metadata()) {
            return;
        }

        let mut fields = FieldVisitor::default();
        event.record(&mut fields);

        let mut line = Map::new();
        line.insert(
            "level".to_owned(),
            Value::from(LogLevel::of(*event.metadata().level()).number()),
        );
        line.insert("time".to_owned(), Value::from(self.clock.now_ms()));
        line.insert("pid".to_owned(), Value::from(self.pid));
        line.insert("hostname".to_owned(), Value::from(self.hostname.as_str()));
        line.extend(self.base.iter().map(|(k, v)| (k.clone(), v.clone())));
        line.extend(fields.fields);
        line.insert(
            "msg".to_owned(),
            Value::from(fields.message.unwrap_or_default()),
        );

        let mut value = Value::Object(line);
        redact(&mut value);
        // Every value here came from `serde_json` types, which always serialise.
        if let Ok(text) = serde_json::to_string(&value) {
            self.sink.write_line(&text);
        }
    }
}

/// Collects an event's fields into JSON, keeping `message` aside as `msg`.
#[derive(Default)]
struct FieldVisitor {
    fields: Map<String, Value>,
    message: Option<String>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(match value {
                Value::String(text) => text,
                other => other.to_string(),
            });
        } else {
            self.fields.insert(field.name().to_owned(), value);
        }
    }
}

impl Visit for FieldVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::from(value.to_string()));
    }

    /// `%value` and `?value` both land here. Text that is a JSON object or
    /// array becomes a tree so the redaction paths can see inside it; anything
    /// else stays the string it printed as.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(text);
            return;
        }
        let structured = text
            .starts_with(['{', '['])
            .then(|| serde_json::from_str::<Value>(&text).ok())
            .flatten()
            .filter(|parsed| parsed.is_object() || parsed.is_array());
        self.put(field, structured.unwrap_or_else(|| Value::from(text)));
    }
}
