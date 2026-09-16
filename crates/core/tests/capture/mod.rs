//! A log sink that keeps every line, for the suites that assert on what the
//! logger wrote. Each suite is its own binary and uses a subset of this.
#![allow(
    dead_code,
    reason = "each test binary uses a subset of the shared helpers"
)]

use std::collections::HashMap;
use std::sync::Arc;

use darkwire_core::logger::{LogLevel, LogSink, LoggerOptions, subscriber};
use darkwire_core::testkit::ManualClock;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tracing::Subscriber;

/// The frozen wall clock every captured line is stamped with.
pub const NOW: i64 = 1_700_000_000_000;

/// Keeps every line written to it.
#[derive(Debug, Default)]
pub struct Captured {
    lines: Mutex<Vec<String>>,
}

impl Captured {
    /// A fresh, empty sink.
    pub fn new() -> Arc<Captured> {
        Arc::new(Captured::default())
    }

    /// Every line, parsed.
    pub fn lines(&self) -> Vec<Map<String, Value>> {
        self.lines
            .lock()
            .iter()
            .map(|line| match serde_json::from_str(line) {
                Ok(Value::Object(map)) => map,
                other => panic!("a log line that is not a JSON object: {other:?}"),
            })
            .collect()
    }

    /// The `msg` of every line, in order.
    pub fn messages(&self) -> Vec<String> {
        self.lines()
            .iter()
            .map(|line| line["msg"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

impl LogSink for Captured {
    fn write_line(&self, line: &str) {
        self.lines.lock().push(line.to_owned());
    }
}

/// Options for a logger writing to `sink` at `level`, with nothing read from
/// the process environment and a frozen clock.
pub fn options(sink: &Arc<Captured>, level: Option<LogLevel>) -> LoggerOptions {
    LoggerOptions {
        level,
        sink: Some(Arc::clone(sink) as Arc<dyn LogSink>),
        env: Some(HashMap::new()),
        clock: Arc::new(ManualClock::at(NOW)),
        hostname: Some("test-host".to_owned()),
        ..LoggerOptions::default()
    }
}

/// A subscriber over `sink` at `level`.
pub fn capturing(sink: &Arc<Captured>, level: LogLevel) -> impl Subscriber + Send + Sync {
    subscriber(options(sink, Some(level)))
}
